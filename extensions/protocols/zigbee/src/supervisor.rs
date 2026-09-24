//! Spawns Zigbee2MQTT and pipes its own log lines into Irori's, so its output is visible from
//! the same place every other extension's is — not a separate log file nobody thinks to check.
//!
//! Note when testing a change here: `install` copies this extension's files into
//! `/var/lib/irori/extensions/<id>/`, and that copy is what the supervisor actually runs.
//! Rebuilding the dev container's image alone leaves it stale — reinstall the extension (through
//! the UI, or `DELETE`/`POST` on its `/api/dev/extensions/{id}` routes) to pick up a new binary
//! or schema.

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
    /// only that it happened — see [`Spawned::last_error`].
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
    let mut child = Command::new(node)
        .arg(entry)
        .env("ZIGBEE2MQTT_DATA", data_dir)
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

impl Spawned {
    /// The last thing Zigbee2MQTT called an error, if it called anything one.
    ///
    /// `exited exit status: 1` is not a reason anyone can act on, and this extension is the only
    /// thing that ever sees Zigbee2MQTT's own account of why it stopped — "Failed to start EZSP
    /// layer with status=HOST_FATAL_ERROR", meaning the radio isn't answering. Reporting that
    /// alongside the status is this extension's half of ROADMAP D47.
    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Gives the log-forwarding tasks a bounded chance to finish draining and logging whatever
    /// Zigbee2MQTT wrote right before exiting — its own crash reason, almost always, since it
    /// logs that before exiting rather than after. Without this, the caller's own process can
    /// exit (`std::process::exit`, after a failed `run` — `main.rs`) before these fire-and-forget
    /// `tokio::spawn`s ever get scheduled to read what's still sitting in the pipe, silently
    /// losing exactly the diagnostic this exists to surface.
    ///
    /// Not a hang risk in the ordinary case: each task returns on its own once the child's exit
    /// closes its end of the pipe (`next_line` sees end of file). The timeout only guards a
    /// Zigbee2MQTT that somehow leaves a grandchild alive holding the pipe open.
    pub async fn drain_logs(self) {
        let [stdout, stderr] = self.logs;
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            let _ = stdout.await;
            let _ = stderr.await;
        })
        .await;
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
                tracing::error!(target: "zigbee2mqtt", "{line}");
            }
            Ok(Some(line)) => tracing::info!(target: "zigbee2mqtt", "{line}"),
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

#[cfg(test)]
mod tests {
    use super::*;

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
        spawned.drain_logs().await;

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "should drain almost instantly once the child's pipes have closed, not wait out the \
             timeout meant for a stuck grandchild"
        );
    }
}
