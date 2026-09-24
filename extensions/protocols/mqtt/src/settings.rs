//! What the MQTT protocol can be told: which broker to connect to, and its credentials.
//!
//! Non-secret fields (`host`, `port`, `tls`, `client_id`, `discovery_prefix`) live in
//! `extensions/mqtt.toml`; `username`/`password` live in `secrets.toml`'s `[mqtt]` table
//! (`docs/specs/config.md` §3.4/§3.6 — the core joins the two tables before this type ever sees
//! them). `Secret`'s `writeOnly` schema is how the generic settings form (Part 2) knows to render
//! a field as a password input that's never sent back.

use std::fmt;
use std::net::IpAddr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// The broker's hostname or IP address, on the local network.
    ///
    /// This extension declares `lan` and nothing else, so the address is checked here: a private,
    /// loopback, or link-local IP, or a name that only exists on the local network (a single
    /// label, or one ending in `.local`, `.home.arpa`, `.internal`, `.lan`, `.home`, or
    /// `localhost`). A public address or an internet hostname is refused. JSON Schema can't
    /// express that, so this check is what actually enforces it.
    #[serde(deserialize_with = "local_host")]
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

/// Names that never resolve on the public internet. The same set `network` refuses, so a local
/// name can't be declared as an internet host and an internet host can't be smuggled in as `lan`.
const LOCAL_DOMAINS: [&str; 6] = ["local", "localhost", "home.arpa", "internal", "lan", "home"];

fn local_host<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let host = String::deserialize(deserializer)?;
    ensure_lan_host(&host).map_err(serde::de::Error::custom)?;
    Ok(host)
}

/// Whether `host` is somewhere `lan` allows this extension to connect.
fn ensure_lan_host(host: &str) -> Result<(), String> {
    let host = host.trim();
    if host.is_empty() {
        return Err("the broker host is empty".into());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return if is_lan_ip(ip) {
            Ok(())
        } else {
            Err(format!(
                "`{host}` is not on the local network. This extension can only reach a broker there."
            ))
        };
    }
    let name = host.trim_end_matches('.').to_ascii_lowercase();
    if is_lan_name(&name) {
        Ok(())
    } else {
        Err(format!(
            "`{host}` is not on the local network. This extension can only reach a broker there."
        ))
    }
}

fn is_lan_ip(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unicast_link_local() || is_unique_local(v6),
    }
}

/// fc00::/7, the IPv6 addresses meant for one network and not the internet.
fn is_unique_local(ip: std::net::Ipv6Addr) -> bool {
    ip.segments()[0] & 0xfe00 == 0xfc00
}

fn is_lan_name(name: &str) -> bool {
    if name.is_empty() || name.contains(':') {
        return false;
    }
    let label_ok = |label: &str| {
        !label.is_empty()
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    if !name.split('.').all(label_ok) {
        return false;
    }
    if !name.contains('.') {
        return true;
    }
    LOCAL_DOMAINS
        .iter()
        .any(|domain| name == *domain || name.ends_with(&format!(".{domain}")))
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
    fn the_broker_host_has_to_be_on_the_local_network() {
        for host in [
            "192.168.1.10",
            "10.0.0.2",
            "127.0.0.1",
            "169.254.1.1",
            "::1",
            "fd12::1",
            "localhost",
            "test-broker",
            "nas.local",
            "mqtt.home.arpa",
        ] {
            serde_json::from_value::<Settings>(serde_json::json!({ "host": host }))
                .unwrap_or_else(|e| panic!("{host} should be local: {e}"));
        }
        for host in [
            "8.8.8.8",
            "1.1.1.1",
            "2001:db8::1",
            "broker.example.com",
            "",
        ] {
            let error = serde_json::from_value::<Settings>(serde_json::json!({ "host": host }))
                .expect_err(host);
            assert!(
                error.to_string().contains("local network") || host.is_empty(),
                "{host}: {error}"
            );
        }
    }

    #[test]
    fn a_secret_never_shows_up_in_debug_output() {
        let secret = Secret("hunter2".to_owned());
        assert!(!format!("{secret:?}").contains("hunter2"));
    }
}
