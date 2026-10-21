//! Runs the wrapped program and pipes its own log lines into Irori's, so its output is visible
//! from the same place every other extension's is — not a separate log file nobody thinks to
//! check.
//!
//! Note when testing a change here: `install` copies an extension's files into
//! `/var/lib/irori/extensions/<id>/`, and that copy is what the supervisor actually runs.
//! Rebuilding the dev container's image alone leaves it stale — reinstall the extension (through
//! the UI, or `DELETE`/`POST` on its `/api/extensions/{id}` routes) to pick up a new binary
//! or schema.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

/// How to read the program's own log lines.
#[derive(Debug, Clone, Copy)]
pub struct LogStyle {
    /// What the program is called in Irori's log, e.g. `zigbee2mqtt`.
    pub name: &'static str,
    /// The sentence a line says went wrong, without the program's timestamp and level, or `None`
    /// for an ordinary line. Programs that write everything to stdout mark errors only by their
    /// own level, and that line is what reaches the extension's card when it fails: "Failed to
    /// start EZSP layer" is an answer, "exited exit status: 1" is not.
    pub error: fn(&str) -> Option<String>,
    /// Whether a line is the program's own, at a level below error. Such a line stays ordinary
    /// even on stderr, where some programs write their warnings (matter.js does): otherwise a
    /// "BLE is not enabled" warning would read as an error, and as the reason it stopped.
    pub ordinary: fn(&str) -> bool,
}

/// The running program, and the two background tasks forwarding its stdout/stderr into Irori's
/// own log.
#[derive(Debug)]
pub struct Spawned {
    pub child: Child,
    logs: [JoinHandle<()>; 2],
    /// The last line the program called an error, kept so the exit can say *why* rather than
    /// only that it happened. [`Spawned::drain_logs`] returns it once the pipes are read.
    last_error: Arc<Mutex<Option<String>>>,
}

/// Starts `command` with its output piped into Irori's log. `kill_on_drop` means dropping the
/// returned `Child` — the extension's own process exiting normally, in particular — takes it down
/// too, the same protection `irori-protocol::process::spawn` already gives every extension's own
/// process. A parent killed outright (`SIGKILL`, never caught) can still orphan it; a known,
/// accepted limit of process-based supervision without platform-specific code this project's own
/// `unsafe_code = "forbid"` rules out.
///
/// `GIT_CEILING_DIRECTORIES` is set so git, run by the program from here, never finds a
/// repository around the extension's directory ([`git_ceiling`]).
pub fn spawn(mut command: Command, style: LogStyle) -> Result<Spawned, String> {
    if let Some(ceiling) = std::env::current_dir()
        .ok()
        .and_then(|here| git_ceiling(&here, std::env::var_os("GIT_CEILING_DIRECTORIES")))
    {
        command.env("GIT_CEILING_DIRECTORIES", ceiling);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("couldn't start {}: {e}", style.name))?;
    let stdout = child.stdout.take().expect("piped above");
    let stderr = child.stderr.take().expect("piped above");
    let last_error = Arc::new(Mutex::new(None));
    let logs = [
        tokio::spawn(log_lines(stdout, false, style, Arc::clone(&last_error))),
        tokio::spawn(log_lines(stderr, true, style, Arc::clone(&last_error))),
    ];
    Ok(Spawned {
        child,
        logs,
        last_error,
    })
}

/// What to set `GIT_CEILING_DIRECTORIES` to so git, run from `here`, never finds a repository
/// above it.
///
/// Zigbee2MQTT asks `git rev-parse HEAD` what commit it is, from whatever directory it was
/// started in, and compares the answer with the hash its published build was made at. Installed
/// from npm there's no repository and the check is skipped — unless the extension's directory
/// happens to sit *inside* someone else's repository (a data directory in a checkout, a home
/// directory kept in git). Then git answers with that repository's commit, Zigbee2MQTT decides
/// its build is stale, and it tries to rebuild itself with a `pnpm` that isn't on `PATH`,
/// failing every start with `pnpm: command not found`.
///
/// A ceiling names a directory git won't climb *into*, so it's `here`'s parent, with symlinks
/// resolved since git compares against the real path. Whatever was already set is kept.
fn git_ceiling(here: &Path, existing: Option<OsString>) -> Option<OsString> {
    let here = here.canonicalize().ok()?;
    let mut ceiling = here.parent()?.as_os_str().to_owned();
    if let Some(existing) = existing.filter(|existing| !existing.is_empty()) {
        ceiling.push(":");
        ceiling.push(existing);
    }
    Some(ceiling)
}

impl Spawned {
    /// Gives the log-forwarding tasks a bounded chance to finish draining and logging whatever
    /// the program wrote right before exiting — its own crash reason, almost always, since it
    /// logs that before exiting rather than after. Without this, the caller's own process can
    /// exit (`std::process::exit`, after a failed `run`) before these fire-and-forget
    /// `tokio::spawn`s ever get scheduled to read what's still sitting in the pipe, silently
    /// losing exactly the diagnostic this exists to surface.
    ///
    /// Returns that reason, read after the tasks have finished. Reading it at the moment
    /// `child.wait` returns can still be empty: the last line is often still in the pipe.
    ///
    /// Not a hang risk in the ordinary case: each task returns on its own once the child's exit
    /// closes its end of the pipe (`next_line` sees end of file). The timeout only guards a
    /// program that somehow leaves a grandchild alive holding the pipe open.
    pub async fn drain_logs(self) -> Option<String> {
        let Self {
            logs: [stdout, stderr],
            last_error,
            // Already signalled by the caller. Dropping it here is what `kill_on_drop` does
            // for a child that is still alive on the way out.
            child: _,
        } = self;
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            let _ = stdout.await;
            let _ = stderr.await;
        })
        .await;
        last_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

async fn log_lines(
    reader: impl AsyncRead + Unpin,
    from_stderr: bool,
    style: LogStyle,
    last_error: Arc<Mutex<Option<String>>>,
) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let said = (style.error)(&line)
                    .or_else(|| {
                        (from_stderr && !(style.ordinary)(&line)).then(|| line.trim().to_owned())
                    })
                    .filter(|said| !said.is_empty());
                if let Some(said) = said {
                    *last_error
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(shorten(&said).to_owned());
                    tracing::error!(from = style.name, "{}", shorten(&line));
                } else {
                    // Discovery configs are tens of kilobytes. Writing one of those through to
                    // stderr in full fills the pipe and blocks this task, which is the same
                    // runtime that has to keep reading the program and answering the page.
                    tracing::info!(from = style.name, "{}", shorten(&line));
                }
            }
            Ok(None) | Err(_) => return,
        }
    }
}

/// Keeps a log line short enough that forwarding it can't fill the stderr pipe.
fn shorten(line: &str) -> &str {
    const MAX: usize = 400;
    if line.len() <= MAX {
        return line;
    }
    let mut end = MAX;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    &line[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zigbee2MQTT's own level marker, for the tests: `… error: <sentence>`.
    fn after_error(line: &str) -> Option<String> {
        line.split_once("error:")
            .map(|(_, rest)| rest.trim().to_owned())
    }

    const STYLE: LogStyle = LogStyle {
        name: "test",
        error: after_error,
        ordinary: |line| line.contains("warn:"),
    };

    fn command(program: &str, arg: &Path) -> Command {
        let mut command = Command::new(program);
        command.arg(arg);
        command
    }

    /// Real git, in a real repository with the extension's directory inside it — the layout that
    /// made Zigbee2MQTT take the surrounding checkout's commit for its own.
    #[test]
    fn git_run_from_the_extension_directory_cannot_see_a_repository_around_it() {
        let repo = tempfile::tempdir().expect("can create a temp dir");
        let here = repo.path().join("data/extensions/zigbee");
        std::fs::create_dir_all(&here).expect("can create the nested directory");
        let git = |args: &[&str], ceiling: Option<&OsString>| {
            let mut command = std::process::Command::new("git");
            command.args(args).current_dir(&here);
            command
                .env_remove("GIT_DIR")
                .env_remove("GIT_CEILING_DIRECTORIES");
            if let Some(ceiling) = ceiling {
                command.env("GIT_CEILING_DIRECTORIES", ceiling);
            }
            command.output()
        };
        let Ok(init) = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(repo.path())
            .output()
        else {
            return; // no git on this machine: nothing for a program to be misled by either
        };
        assert!(init.status.success(), "git init failed");
        let inside = &["rev-parse", "--is-inside-work-tree"];
        assert!(git(inside, None).expect("git runs").status.success());

        let ceiling = git_ceiling(&here, None).expect("a nested directory has a parent");
        assert!(
            !git(inside, Some(&ceiling))
                .expect("git runs")
                .status
                .success()
        );
    }

    #[test]
    fn a_ceiling_already_set_is_kept_after_ours() {
        let dir = tempfile::tempdir().expect("can create a temp dir");
        let ours = git_ceiling(dir.path(), None).expect("a temp dir has a parent");
        let both = git_ceiling(dir.path(), Some("/somewhere/else".into())).expect("still some");
        let mut expected = ours.clone();
        expected.push(":/somewhere/else");
        assert_eq!(both, expected);
        assert_eq!(git_ceiling(dir.path(), Some(OsString::new())), Some(ours));
    }

    #[test]
    fn a_long_line_is_shortened_before_it_is_forwarded() {
        let line = format!("info: {}", "x".repeat(2_000));
        assert!(shorten(&line).len() <= 400);
        assert_eq!(shorten("short"), "short");
        // Never cut inside a character.
        let wide = "é".repeat(300);
        assert!(shorten(&wide).len() <= 400);
    }

    /// The property `drain_logs` exists for: once the child (and so its pipes) has actually
    /// closed, draining returns almost immediately rather than waiting out its own timeout — the
    /// forwarding tasks see end of file and stop on their own. A real, trivial process, not a
    /// mock: what's under test is genuine OS pipe behavior across a process exit.
    #[tokio::test]
    async fn drain_logs_returns_promptly_once_the_child_has_exited() {
        let mut spawned = spawn(command("/bin/echo", Path::new("hello")), STYLE)
            .expect("spawns a trivial real process");
        spawned.child.wait().await.expect("echo exits");

        let started = std::time::Instant::now();
        let said = spawned.drain_logs().await;

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "should drain almost instantly once the child's pipes have closed, not wait out the \
             timeout meant for a stuck grandchild"
        );
        assert_eq!(said, None, "an ordinary line on stdout is not an error");
    }

    /// The reason is what `drain_logs` returns, not whatever `last_error` held at the moment
    /// `wait` completed. The line is written and the process exits in the same breath; the
    /// forwarding task may not have read it until it is asked to finish.
    #[tokio::test]
    async fn drain_logs_returns_the_error_line_written_just_before_exit() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let script = dir.path().join("die.sh");
        std::fs::write(
            &script,
            "echo '[2026-09-24 00:56:35] error: radio is not answering'\nexit 1\n",
        )
        .expect("write the script");

        let mut spawned = spawn(command("/bin/sh", &script), STYLE).expect("spawns");
        spawned.child.wait().await.expect("the script exits");
        let said = spawned.drain_logs().await;

        assert_eq!(said.as_deref(), Some("radio is not answering"));
    }

    /// Anything on stderr counts, whole, when the program's own style doesn't recognise it: a
    /// Node crash prints its stack there, not in the program's log format.
    #[tokio::test]
    async fn a_line_on_stderr_is_kept_as_the_reason_even_unmarked() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let script = dir.path().join("die.sh");
        std::fs::write(&script, "echo 'Error: Cannot find module x' >&2\nexit 1\n")
            .expect("write the script");

        let mut spawned = spawn(command("/bin/sh", &script), STYLE).expect("spawns");
        spawned.child.wait().await.expect("the script exits");
        assert_eq!(
            spawned.drain_logs().await.as_deref(),
            Some("Error: Cannot find module x")
        );
    }

    /// A warning the program writes to stderr in its own format isn't the reason it stopped.
    #[tokio::test]
    async fn the_programs_own_warning_on_stderr_is_not_the_reason() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let script = dir.path().join("die.sh");
        std::fs::write(&script, "echo 'warn: no Bluetooth here' >&2\nexit 1\n")
            .expect("write the script");

        let mut spawned = spawn(command("/bin/sh", &script), STYLE).expect("spawns");
        spawned.child.wait().await.expect("the script exits");
        assert_eq!(spawned.drain_logs().await, None);
    }
}
