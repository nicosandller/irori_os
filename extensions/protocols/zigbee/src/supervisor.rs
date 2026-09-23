//! Spawns Zigbee2MQTT and pipes its own log lines into Irori's, so its output is visible from
//! the same place every other extension's is — not a separate log file nobody thinks to check.
//!
//! One thing this can't fix: on POSIX, Node writes to a piped stdout/stderr asynchronously, and
//! a hard `process.exit()` — how Zigbee2MQTT reacts to a fatal adapter error, e.g. a missing
//! serial device — can win the race against those writes actually landing in the pipe. When
//! that happens the only line we ever see is our own supervisor's "exited: exit status: 1";
//! Zigbee2MQTT's own explanation never left its process. `drain_logs` below closes the *other*
//! gap (reading whatever did make it into the pipe before our own process exits), but it can't
//! recover bytes Node itself never wrote.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

/// Zigbee2MQTT, and the two background tasks forwarding its stdout/stderr into Irori's own log.
pub struct Spawned {
    pub child: Child,
    logs: [JoinHandle<()>; 2],
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
    let logs = [
        tokio::spawn(log_lines(stdout, false)),
        tokio::spawn(log_lines(stderr, true)),
    ];
    Ok(Spawned { child, logs })
}

impl Spawned {
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

async fn log_lines(reader: impl AsyncRead + Unpin, from_stderr: bool) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) if from_stderr => tracing::warn!(target: "zigbee2mqtt", "{line}"),
            Ok(Some(line)) => tracing::info!(target: "zigbee2mqtt", "{line}"),
            Ok(None) | Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property `drain_logs` exists for: once the child (and so its pipes) has actually
    /// closed, draining returns almost immediately rather than waiting out its own timeout — the
    /// forwarding tasks see end of file and stop on their own. A real, trivial process, not a
    /// mock: what's under test is genuine OS pipe behavior across a process exit.
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
