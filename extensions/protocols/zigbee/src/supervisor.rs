//! Spawns Zigbee2MQTT and pipes its own log lines into Irori's, so its output is visible from
//! the same place every other extension's is — not a separate log file nobody thinks to check.
//!
//! Note when testing a change here: `install` copies this extension's files into
//! `/var/lib/irori/extensions/<id>/`, and that copy is what the supervisor actually runs.
//! Rebuilding the dev container's image alone leaves it stale — reinstall the extension (through
//! the UI, or `DELETE`/`POST` on its `/api/dev/extensions/{id}` routes) to pick up a new binary
//! or schema.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

/// Zigbee2MQTT, and the two background tasks forwarding its stdout/stderr into Irori's own log.
pub struct Spawned {
    pub child: Child,
    logs: [JoinHandle<()>; 2],
    /// The last line Zigbee2MQTT called an error, kept so the exit can say *why* rather than
    /// only that it happened. [`Spawned::drain_logs`] returns it once the pipes are read.
    last_error: Arc<Mutex<Option<String>>>,
}

/// Starts Zigbee2MQTT, pointed at `data_dir` for its own `configuration.yaml` (Zigbee2MQTT's
/// own convention, `ZIGBEE2MQTT_DATA`). `kill_on_drop` means dropping the returned `Child` —
/// this extension's own process exiting normally, in particular — takes it down too, the same
/// protection `irori-protocol::process::spawn` already gives every extension's own process.
/// A parent killed outright (`SIGKILL`, never caught) can still orphan it; a known, accepted
/// limit of process-based supervision without platform-specific code this project's own
/// `unsafe_code = "forbid"` rules out.
pub fn spawn(node: &Path, entry: &Path, data_dir: &Path) -> Result<Spawned, String> {
    let mut command = Command::new(node);
    command.arg(entry).env("ZIGBEE2MQTT_DATA", data_dir);
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
        .map_err(|e| format!("couldn't start Zigbee2MQTT: {e}"))?;
    let stdout = child.stdout.take().expect("piped above");
    let stderr = child.stderr.take().expect("piped above");
    let last_error = Arc::new(Mutex::new(None));
    let logs = [
        tokio::spawn(log_lines(stdout, false, Arc::clone(&last_error))),
        tokio::spawn(log_lines(stderr, true, Arc::clone(&last_error))),
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
/// from npm there's no repository and the check is skipped — unless this extension's directory
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
    /// Zigbee2MQTT wrote right before exiting — its own crash reason, almost always, since it
    /// logs that before exiting rather than after. Without this, the caller's own process can
    /// exit (`std::process::exit`, after a failed `run` — `main.rs`) before these fire-and-forget
    /// `tokio::spawn`s ever get scheduled to read what's still sitting in the pipe, silently
    /// losing exactly the diagnostic this exists to surface.
    ///
    /// Returns that reason, read after the tasks have finished. Reading it at the moment
    /// `child.wait` returns can still be empty: the last line is often still in the pipe.
    ///
    /// Not a hang risk in the ordinary case: each task returns on its own once the child's exit
    /// closes its end of the pipe (`next_line` sees end of file). The timeout only guards a
    /// Zigbee2MQTT that somehow leaves a grandchild alive holding the pipe open.
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
    last_error: Arc<Mutex<Option<String>>>,
) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) if from_stderr || says_error(&line) => {
                *last_error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tidy(&line));
                tracing::error!(target: "zigbee2mqtt", "{}", shorten(&line));
            }
            // Discovery configs are tens of kilobytes. Writing one of those through to stderr
            // in full fills the pipe and blocks this task, which is the same runtime that has
            // to keep reading Zigbee2MQTT and answering permit-join.
            Ok(Some(line)) => tracing::info!(target: "zigbee2mqtt", "{}", shorten(&line)),
            Ok(None) | Err(_) => return,
        }
    }
}

/// Zigbee2MQTT's own line with its timestamp, level and tabs taken off, so what lands on the card
/// is the sentence and not the log formatting around it.
fn tidy(line: &str) -> String {
    let after_level = line
        .split_once("error:")
        .or_else(|| line.split_once("Error:"))
        .map_or(line, |(_, rest)| rest);
    after_level
        .trim()
        .trim_start_matches("z2m:")
        .trim()
        .trim_start_matches("Error:")
        .trim()
        .to_owned()
}

/// Whether Zigbee2MQTT is calling this line of its own an error.
///
/// It writes its whole log, errors included, to stdout — so forwarding stdout wholesale at info
/// would bury the one line that says why it couldn't start. That line is what reaches the
/// extension's card when it fails (ROADMAP D47): "Failed to start EZSP layer" is an answer,
/// "exited exit status: 1" is not. Matched on Zigbee2MQTT's own level marker, which is its
/// timestamp followed by `error:`.
fn says_error(line: &str) -> bool {
    line.contains("error:") || line.contains("Error:")
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

    /// Real git, in a real repository with this extension's directory inside it — the layout
    /// that made Zigbee2MQTT take the surrounding checkout's commit for its own.
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
            return; // no git on this machine: nothing for Zigbee2MQTT to be misled by either
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

    /// The property `drain_logs` exists for: once the child (and so its pipes) has actually
    /// closed, draining returns almost immediately rather than waiting out its own timeout — the
    /// forwarding tasks see end of file and stop on their own. A real, trivial process, not a
    /// mock: what's under test is genuine OS pipe behavior across a process exit.
    #[test]
    fn a_zigbee2mqtt_error_line_is_tidied_down_to_its_sentence() {
        let line = "[2026-09-24 00:56:35] error: \tz2m: Error: Failed to start EZSP layer \
                    with status=HOST_FATAL_ERROR.";
        assert!(says_error(line));
        assert_eq!(
            tidy(line),
            "Failed to start EZSP layer with status=HOST_FATAL_ERROR."
        );
    }

    #[test]
    fn a_discovery_payload_is_shortened_before_it_is_forwarded() {
        let line = format!(
            "[2026-10-01 01:37:58] info: z2m:mqtt: {}",
            "x".repeat(2_000)
        );
        assert!(shorten(&line).len() <= 400);
        assert!(shorten("short").len() < 400);
    }

    #[test]
    fn ordinary_zigbee2mqtt_chatter_isnt_mistaken_for_an_error() {
        assert!(!says_error(
            "[2026-09-24 00:56:30] info: \tz2m: Starting Zigbee2MQTT"
        ));
    }

    #[tokio::test]
    async fn drain_logs_returns_promptly_once_the_child_has_exited() {
        let mut spawned = spawn(
            Path::new("/bin/echo"),
            Path::new("hello"),
            Path::new("/tmp"),
        )
        .expect("spawns a trivial real process");
        spawned.child.wait().await.expect("echo exits");

        let started = std::time::Instant::now();
        let _ = spawned.drain_logs().await;

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "should drain almost instantly once the child's pipes have closed, not wait out the \
             timeout meant for a stuck grandchild"
        );
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
            "echo '[2026-09-24 00:56:35] error:\tz2m: Error: radio is not answering'\nexit 1\n",
        )
        .expect("write the script");

        let mut spawned = spawn(Path::new("/bin/sh"), &script, Path::new("/tmp")).expect("spawns");
        spawned.child.wait().await.expect("the script exits");
        let said = spawned.drain_logs().await;

        assert_eq!(said.as_deref(), Some("radio is not answering"));
    }
}
