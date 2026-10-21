//! What the Matter extension can be told. Everything has a default: installed and started with
//! no settings at all, it runs the Matter server it was tested with, on its own port.

use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Pins the Open Home Foundation Matter Server to one version, e.g. `1.5.0`. Left out, the
    /// first start installs the version this extension was tested with, and keeps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matter_server_version: Option<String>,
    /// The port the Matter server listens on for this extension. It only listens on this
    /// machine's loopback address, never the network. Change it only if something else here
    /// already uses it. Not 5580, which a Home Assistant Matter server on this machine may use.
    #[serde(default = "default_port")]
    #[schemars(range(min = 1))]
    pub port: u16,
    /// The network interface Matter devices are on, e.g. `eth0` or `wlan0`, for a machine with
    /// more than one. Left out, the server picks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_interface: Option<String>,
    /// Accept devices made with development certificates: samples from Matter SDKs, the dev lab's
    /// emulated devices, a prototype. A shop-bought device never needs this. Turn it on only for
    /// devices you made or trust.
    #[serde(default)]
    pub allow_test_devices: bool,
}

pub fn default_port() -> u16 {
    15_580
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            matter_server_version: None,
            port: default_port(),
            primary_interface: None,
            allow_test_devices: false,
        }
    }
}

impl Settings {
    /// Refuses settings that would only fail later, out of sight: a version that isn't one
    /// (it lands in an npm package spec), port 0, an interface name the server would choke on.
    pub fn check(&self) -> Result<(), String> {
        if let Some(version) = &self.matter_server_version {
            irori_node_runtime::install::checked_version(version, "Matter server")?;
        }
        if self.port == 0 {
            return Err(
                "`port` can't be 0: the extension has to know which port the Matter server is on"
                    .to_owned(),
            );
        }
        if let Some(interface) = &self.primary_interface {
            let fine = !interface.is_empty()
                && interface.len() <= 15
                && interface
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
            if !fine {
                return Err(format!(
                    "`{interface}` isn't a network interface name like `eth0` or `wlan0`"
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_given_is_everything_default() {
        let settings: Settings = serde_json::from_value(serde_json::json!({})).expect("parses");
        assert_eq!(settings, Settings::default());
        assert_eq!(settings.check(), Ok(()));
    }

    #[test]
    fn what_would_only_fail_later_is_refused_now() {
        let with = |value: serde_json::Value| {
            serde_json::from_value::<Settings>(value)
                .expect("parses")
                .check()
        };
        assert!(
            with(serde_json::json!({"matter_server_version": "npm:evil@1"}))
                .expect_err("not a version")
                .contains("Matter server version")
        );
        assert!(with(serde_json::json!({"port": 0})).is_err());
        assert!(with(serde_json::json!({"primary_interface": "eth0; rm -rf /"})).is_err());
        assert!(with(serde_json::json!({"primary_interface": ""})).is_err());
        assert_eq!(
            with(serde_json::json!({
                "matter_server_version": "1.5.0",
                "port": 15581,
                "primary_interface": "enp1s0",
                "allow_test_devices": true
            })),
            Ok(())
        );
        assert!(
            serde_json::from_value::<Settings>(serde_json::json!({"server_url": "ws://x"}))
                .is_err(),
            "an unknown setting is a typo"
        );
    }
}
