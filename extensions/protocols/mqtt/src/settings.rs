//! What the MQTT protocol can be told: which broker to connect to, and its credentials.
//!
//! Non-secret fields (`host`, `port`, `tls`, `client_id`, `discovery_prefix`) live in
//! `extensions/mqtt.toml`; `username`/`password` live in `secrets.toml`'s `[mqtt]` table
//! (`docs/specs/config.md` §3.4/§3.6 — the core joins the two tables before this type ever sees
//! them). `Secret`'s `writeOnly` schema is how the generic settings form (Part 2) knows to render
//! a field as a password input that's never sent back.

use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// The broker's hostname or IP address.
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Connect over TLS.
    #[serde(default)]
    pub tls: bool,
    /// Sent as the MQTT client id. A generated one is used when this is left out.
    #[serde(default)]
    pub client_id: Option<String>,
    /// The topic prefix Home Assistant MQTT Discovery configs are published under.
    #[serde(default = "default_discovery_prefix")]
    pub discovery_prefix: String,
    #[serde(default)]
    pub username: Option<Secret>,
    #[serde(default)]
    pub password: Option<Secret>,
}

fn default_port() -> u16 {
    1883
}

fn default_discovery_prefix() -> String {
    "homeassistant".to_owned()
}

/// A credential: never printed, never sent back once given.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// For handing to the broker connection, and nowhere else.
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
    fn settings_load_with_only_a_host_given() {
        let settings: Settings =
            serde_json::from_value(serde_json::json!({ "host": "192.168.1.10" })).expect("valid");
        assert_eq!(settings.port, 1883);
        assert!(!settings.tls);
        assert_eq!(settings.discovery_prefix, "homeassistant");
        assert!(settings.username.is_none());
    }

    #[test]
    fn a_secret_never_shows_up_in_debug_output() {
        let secret = Secret("hunter2".to_owned());
        assert!(!format!("{secret:?}").contains("hunter2"));
    }
}
