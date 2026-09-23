//! Spawns Zigbee2MQTT and pipes its own log lines into Irori's, so its output is visible from
//! the same place every other extension's is — not a separate log file nobody thinks to check.

use std::path::Path;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};

/// Starts Zigbee2MQTT, pointed at `data_dir` for its own `configuration.yaml` (Zigbee2MQTT's
/// own convention, `ZIGBEE2MQTT_DATA`). `kill_on_drop` means dropping the returned `Child` —
/// this extension's own process exiting normally, in particular — takes it down too, the same
/// protection `irori-protocol::process::spawn` already gives every extension's own process.
/// A parent killed outright (`SIGKILL`, never caught) can still orphan it; a known, accepted
/// limit of process-based supervision without platform-specific code this project's own
/// `unsafe_code = "forbid"` rules out.
pub fn spawn(node: &Path, entry: &Path, data_dir: &Path) -> Result<Child, String> {
    let mut child = Command::new(node)
        .arg(entry)
        .env("ZIGBEE2MQTT_DATA", data_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("couldn't start Zigbee2MQTT: {e}"))?;
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(log_lines(stdout, false));
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(log_lines(stderr, true));
    }
    Ok(child)
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
