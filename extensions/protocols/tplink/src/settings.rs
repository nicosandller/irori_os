//! What the TP-Link extension can be told. The whole table lives in `secrets.toml`, because the
//! account password is in it; no refusal ever repeats a value.
//!
//! Left without known hosts, discovery is a broadcast on this network only. Plugs on another
//! network (an IoT VLAN) answer when asked by address, so they go in `known_hosts`.

use std::net::{IpAddr, Ipv6Addr};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};

/// Local names. The same set Cast and MQTT accept, so a public name can't be written down as local.
const LOCAL_DOMAINS: [&str; 6] = ["local", "localhost", "home.arpa", "internal", "lan", "home"];

/// How often devices are read, when the settings don't say.
pub const DEFAULT_POLL_SECONDS: u32 = 15;

#[derive(Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Settings {
    /// The email of the TP-Link (Tapo or Kasa) account the devices were set up with. Tapo plugs
    /// and recent Kasa firmware check every local connection against it.
    #[serde(default, deserialize_with = "account")]
    pub username: Option<String>,
    /// That account's password.
    #[serde(default, deserialize_with = "account")]
    pub password: Option<String>,
    /// Devices to ask by address, as well as anything that answers the broadcast. Each one has to
    /// be a private, loopback, or link-local address, or a name under `.local`, `.home.arpa`,
    /// `.internal`, `.lan`, or `.home`. A port, a bare name, a public address, or an internet
    /// hostname is refused.
    #[serde(default, deserialize_with = "known_hosts")]
    pub known_hosts: Vec<String>,
    /// Seconds between readings of each device, 5 to 300. 15 when left out.
    #[serde(default, deserialize_with = "poll_seconds")]
    #[schemars(range(min = 5, max = 300))]
    pub poll_seconds: Option<u32>,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("username", &self.username.as_ref().map(|_| "…"))
            .field("password", &self.password.as_ref().map(|_| "…"))
            .field("known_hosts", &self.known_hosts)
            .field("poll_seconds", &self.poll_seconds)
            .finish()
    }
}

impl Settings {
    /// The account, when both halves are there.
    pub fn account(&self) -> Option<(&str, &str)> {
        match (&self.username, &self.password) {
            (Some(username), Some(password)) => Some((username, password)),
            _ => None,
        }
    }

    pub fn poll(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(self.poll_seconds.unwrap_or(DEFAULT_POLL_SECONDS)))
    }
}

fn account<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    let value = Option::<String>::deserialize(deserializer)?;
    match value {
        Some(value) if value.is_empty() => Ok(None),
        Some(value) if value.chars().any(char::is_control) => Err(serde::de::Error::custom(
            "the TP-Link account can't contain control characters",
        )),
        other => Ok(other),
    }
}

fn poll_seconds<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u32>, D::Error> {
    let value = Option::<u32>::deserialize(deserializer)?;
    match value {
        Some(seconds) if !(5..=300).contains(&seconds) => Err(serde::de::Error::custom(
            "poll_seconds is between 5 and 300",
        )),
        other => Ok(other),
    }
}

fn known_hosts<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let hosts = Vec::<String>::deserialize(deserializer)?;
    let mut out = Vec::with_capacity(hosts.len());
    for host in hosts {
        let host = normalize_host(&host).map_err(serde::de::Error::custom)?;
        if !out.contains(&host) {
            out.push(host);
        }
    }
    Ok(out)
}

/// The host as it will be dialled, or why it can't be.
fn normalize_host(host: &str) -> Result<String, String> {
    let host = host.trim().trim_end_matches('.').trim();
    if host.is_empty() {
        return Err("a known host is empty".into());
    }
    if let Some(ip) = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
    {
        return lan_ip(ip);
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return if is_lan_ip(ip) {
            Ok(ip.to_string())
        } else {
            Err("a known host is not on the local network".into())
        };
    }
    if host.contains(':') {
        return Err("a known host can't include a port".into());
    }
    let name = host.to_ascii_lowercase();
    if is_lan_name(&name) {
        Ok(name)
    } else {
        Err("a known host is not on the local network".into())
    }
}

fn lan_ip(text: &str) -> Result<String, String> {
    let Ok(ip) = text.parse::<IpAddr>() else {
        return Err("a known host is not on the local network".into());
    };
    if is_lan_ip(ip) {
        Ok(ip.to_string())
    } else {
        Err("a known host is not on the local network".into())
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
fn is_unique_local(ip: Ipv6Addr) -> bool {
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
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    if !name.split('.').all(label_ok) {
        return false;
    }
    LOCAL_DOMAINS
        .iter()
        .any(|domain| name == *domain || name.ends_with(&format!(".{domain}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(value: serde_json::Value) -> Result<Settings, String> {
        serde_json::from_value(value).map_err(|error| error.to_string())
    }

    #[test]
    fn empty_settings_listen_for_the_broadcast_only() {
        let settings = load(serde_json::json!({})).expect("empty settings are valid");
        assert!(settings.known_hosts.is_empty());
        assert!(settings.account().is_none());
        assert_eq!(settings.poll().as_secs(), 15);
    }

    #[test]
    fn the_account_needs_both_halves() {
        let half = load(serde_json::json!({ "username": "me@example.com" })).expect("valid");
        assert!(half.account().is_none());
        let both = load(serde_json::json!({ "username": "me@example.com", "password": "pw" }))
            .expect("valid");
        assert_eq!(both.account(), Some(("me@example.com", "pw")));
    }

    #[test]
    fn debug_output_never_shows_the_account() {
        let settings = load(serde_json::json!({
            "username": "me@example.com", "password": "hunter2"
        }))
        .expect("valid");
        let shown = format!("{settings:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(!shown.contains("me@example.com"), "{shown}");
    }

    #[test]
    fn a_bad_password_is_refused_without_being_repeated() {
        let error = load(serde_json::json!({ "password": "pw\u{7}secret" })).expect_err("control");
        assert!(!error.contains("secret"), "{error}");
    }

    #[test]
    fn a_known_host_has_to_be_on_the_local_network() {
        for host in ["192.168.1.21", "192.168.1.10", "plug.home.arpa", "fd12::1"] {
            let settings = load(serde_json::json!({ "known_hosts": [host] }))
                .unwrap_or_else(|error| panic!("{host} should be local: {error}"));
            assert_eq!(settings.known_hosts.len(), 1, "{host}");
        }
        for host in ["8.8.8.8", "plug.example.com", "plug", "192.168.1.21:80", ""] {
            let error = load(serde_json::json!({ "known_hosts": [host] }))
                .expect_err("the host should be refused");
            assert!(!error.contains(host) || host.is_empty(), "{error}");
        }
    }

    #[test]
    fn polling_stays_between_5_and_300_seconds() {
        assert!(load(serde_json::json!({ "poll_seconds": 4 })).is_err());
        assert!(load(serde_json::json!({ "poll_seconds": 301 })).is_err());
        let settings = load(serde_json::json!({ "poll_seconds": 30 })).expect("valid");
        assert_eq!(settings.poll().as_secs(), 30);
    }
}
