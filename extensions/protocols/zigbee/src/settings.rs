//! What the Zigbee extension can be told: the dongle, the Zigbee network, and (rarely) which
//! Zigbee2MQTT version to pin to.
//!
//! `network_key`'s `writeOnly` schema is how the generic settings form knows to treat it as a
//! secret (`extensions/mqtt.toml`'s `Secret` documents the same pattern; duplicated here rather
//! than shared, since it's a handful of lines and the two protocols have no other reason to
//! depend on each other).

use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// The dongle's serial device, e.g. `/dev/ttyUSB0` (Linux) or `/dev/cu.usbmodem…` (macOS) —
    /// or `tcp://host:port` for a dongle reached over the network rather than plugged directly
    /// into this machine (Zigbee2MQTT's own TCP adapter support), which is how to use one on a
    /// Mac running Irori in `dev/pi`'s container: Docker Desktop can't pass a USB device through
    /// to it, so bridge the dongle's serial port to TCP on the host first (`dev/README.md`).
    /// The Settings form offers what's plugged in right now as suggestions (`format` below), but
    /// always accepts a typed path or `tcp://` address too.
    #[schemars(extend("format" = "serial-port"))]
    pub serial_port: String,
    /// The dongle's radio chip family. Most Silicon Labs–based dongles (Sonoff, SLZB, most
    /// "zigbee 3.0 usb dongle plus" boards) are `ember`; older ones may be `zstack` (Texas
    /// Instruments) or `deconz` (ConBee/RaspBee).
    #[serde(default)]
    pub adapter: Adapter,
    /// The Zigbee channel, 11-26. Left to Zigbee2MQTT's own default (25) if omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    /// The Zigbee network's PAN id. Left to Zigbee2MQTT's own default if omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pan_id: Option<u16>,
    /// The network's encryption key. Left out, Zigbee2MQTT generates and keeps its own on first
    /// run — the simpler default for a new network. Only needed to join an existing one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_key: Option<Secret>,
    /// Pins Zigbee2MQTT to one version. Left out, the first start installs the current release
    /// and remembers exactly which one — an update after that is a deliberate choice
    /// (re-installing with a version given here), never something that happens on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zigbee2mqtt_version: Option<String>,
    /// The port of the embedded broker this extension runs for its own Zigbee2MQTT to publish
    /// into. Not Irori's own network-facing port; loopback only. Change it only if something
    /// else on this machine already uses the default.
    #[serde(default = "default_broker_port")]
    pub broker_port: u16,
}

fn default_broker_port() -> u16 {
    17_883
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Adapter {
    #[default]
    Ember,
    Zstack,
    Deconz,
    Zboss,
}

impl Adapter {
    /// Zigbee2MQTT's own name for it, in `configuration.yaml`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ember => "ember",
            Self::Zstack => "zstack",
            Self::Deconz => "deconz",
            Self::Zboss => "zboss",
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_load_with_only_a_serial_port_given() {
        let settings: Settings =
            serde_json::from_value(serde_json::json!({ "serial_port": "/dev/ttyUSB0" }))
                .expect("valid");
        assert_eq!(settings.adapter, Adapter::Ember);
        assert_eq!(settings.broker_port, 17_883);
        assert!(settings.network_key.is_none());
    }

    #[test]
    fn a_network_key_never_shows_up_in_debug_output() {
        let secret = Secret("0123456789abcdef".to_owned());
        assert!(!format!("{secret:?}").contains("0123456789abcdef"));
    }
}
