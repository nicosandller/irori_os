//! What the Matter extension can be told. Everything has a default: installed and started with
//! no settings at all, it runs the Matter server it was tested with, on its own port.
//!
//! The Wi-Fi password and the Thread dataset are secrets: kept in `secrets.toml`, never sent
//! back to a page, never printed (`docs/specs/config.md` §3.4).

use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer};

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
    /// Offer each device's firmware updates, from the Matter DCL (the CSA's register of devices
    /// and their firmware). An update is only installed when someone installs it.
    #[serde(default = "yes")]
    pub firmware_updates: bool,
    /// Add new devices over Bluetooth: one fresh out of its box that isn't on a network yet.
    /// Irori hands it the Wi-Fi or Thread network below. On Linux the Matter server's Node.js
    /// needs `cap_net_raw` and `cap_net_admin` to reach the adapter; on a Mac, Bluetooth
    /// permission for the app Irori runs in.
    #[serde(default)]
    pub bluetooth: bool,
    /// Which Bluetooth adapter, on Linux: 0 for `hci0`. A Mac has one, and ignores this.
    #[serde(default)]
    pub bluetooth_adapter: u8,
    /// The Wi-Fi network a new Wi-Fi device joins when it's added over Bluetooth. 2.4 GHz: most
    /// Matter devices can't join a 5 GHz network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(title = "Wi-Fi network")]
    pub wifi_ssid: Option<String>,
    /// That Wi-Fi network's password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(title = "Wi-Fi password")]
    pub wifi_password: Option<Secret>,
    /// The Thread network a new Thread device joins when it's added over Bluetooth: the
    /// operational dataset, as hex, from the border router's app (Home Assistant's Thread
    /// settings, or `ot-ctl dataset active -x`). Apple's and Google's border routers keep theirs
    /// to themselves; add their Thread devices in their own apps first, then share them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_dataset: Option<Secret>,
}

fn yes() -> bool {
    true
}

/// A credential: never printed, never sent back once given.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Secret)
    }
}

impl JsonSchema for Secret {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Secret".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "writeOnly": true,
        })
    }
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
            firmware_updates: true,
            bluetooth: false,
            bluetooth_adapter: 0,
            wifi_ssid: None,
            wifi_password: None,
            thread_dataset: None,
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
        if let Some(ssid) = &self.wifi_ssid
            && (ssid.is_empty() || ssid.len() > 32)
        {
            return Err("a Wi-Fi network's name is 1 to 32 characters".to_owned());
        }
        if let Some(password) = &self.wifi_password
            && self.wifi_ssid.is_none()
            && !password.expose().is_empty()
        {
            return Err("a Wi-Fi password needs the network's name, `wifi_ssid`, too".to_owned());
        }
        if let Some(dataset) = &self.thread_dataset {
            let hex = dataset.expose();
            let fine = !hex.is_empty()
                && hex.len() % 2 == 0
                && hex.len() <= 508
                && hex.chars().all(|c| c.is_ascii_hexdigit());
            if !fine {
                return Err(
                    "the Thread dataset is the border router's active dataset as hex, e.g. \
                     `0e080000000000010000…`"
                        .to_owned(),
                );
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
        assert!(with(serde_json::json!({"thread_dataset": "not hex"})).is_err());
        assert!(with(serde_json::json!({"wifi_password": "hunter22"})).is_err());
        assert_eq!(
            with(serde_json::json!({
                "bluetooth": true,
                "wifi_ssid": "Home",
                "wifi_password": "hunter22",
                "thread_dataset": "0e08000000000001"
            })),
            Ok(())
        );
    }

    #[test]
    fn a_secret_never_shows_up_in_debug_output() {
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "wifi_ssid": "Home",
            "wifi_password": "hunter22",
            "thread_dataset": "0e08000000000001"
        }))
        .expect("parses");
        let printed = format!("{settings:?}");
        assert!(
            !printed.contains("hunter22") && !printed.contains("0e0800"),
            "{printed}"
        );
    }
}
