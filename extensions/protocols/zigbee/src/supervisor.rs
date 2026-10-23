//! Spawns Zigbee2MQTT through `irori-node-runtime`, which pipes its own log lines into Irori's.
//! What's Zigbee2MQTT's own here is how its log says something went wrong.

use std::path::Path;

use irori_node_runtime::child::{self, LogStyle};
use tokio::process::Command;

pub use irori_node_runtime::child::Spawned;

const STYLE: LogStyle = LogStyle {
    name: "zigbee2mqtt",
    error: error_sentence,
    // Zigbee2MQTT writes its whole log to stdout; on stderr is only what crashed it.
    ordinary: |_| false,
};

/// Starts Zigbee2MQTT, pointed at `data_dir` for its own `configuration.yaml` (Zigbee2MQTT's
/// own convention, `ZIGBEE2MQTT_DATA`).
pub fn spawn(node: &Path, entry: &Path, data_dir: &Path) -> Result<Spawned, String> {
    let mut command = Command::new(node);
    command.arg(entry).env("ZIGBEE2MQTT_DATA", data_dir);
    child::spawn(command, STYLE)
}

/// The sentence of a line Zigbee2MQTT calls an error, if it is one.
fn error_sentence(line: &str) -> Option<String> {
    says_error(line).then(|| tidy(line))
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
/// extension's card when it fails (ROADMAP D47). Matched on Zigbee2MQTT's own level marker,
/// which is its timestamp followed by `error:`.
fn says_error(line: &str) -> bool {
    line.contains("error:") || line.contains("Error:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zigbee2mqtt_error_line_is_tidied_down_to_its_sentence() {
        let line = "[2026-09-24 00:56:35] error: \tz2m: Error: Failed to start EZSP layer \
                    with status=HOST_FATAL_ERROR.";
        assert_eq!(
            error_sentence(line).as_deref(),
            Some("Failed to start EZSP layer with status=HOST_FATAL_ERROR.")
        );
    }

    #[test]
    fn ordinary_zigbee2mqtt_chatter_isnt_mistaken_for_an_error() {
        assert_eq!(
            error_sentence("[2026-09-24 00:56:30] info: \tz2m: Starting Zigbee2MQTT"),
            None
        );
    }

    /// The whole path with Zigbee2MQTT's own format: written just before exit, read back as the
    /// reason.
    #[tokio::test]
    async fn the_reason_zigbee2mqtt_gives_is_what_the_exit_says() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let script = dir.path().join("die.sh");
        std::fs::write(
            &script,
            "echo '[2026-09-24 00:56:35] error:\tz2m: Error: radio is not answering'\nexit 1\n",
        )
        .expect("write the script");

        let mut spawned = spawn(Path::new("/bin/sh"), &script, Path::new("/tmp")).expect("spawns");
        spawned.child.wait().await.expect("the script exits");
        assert_eq!(
            spawned.drain_logs().await.as_deref(),
            Some("radio is not answering")
        );
    }
}
