//! What the AirGradient extension can be told, in `extensions/airgradient.toml`. No refusal ever
//! repeats a value.
//!
//! Monitors aren't searched for: each one is listed in `known_hosts`, by address or by the
//! `.local` name it gives itself.

use std::net::{IpAddr, Ipv6Addr};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};

/// Local names. The same set TP-Link, Cast and MQTT accept, so a public name can't be written
/// down as local.
const LOCAL_DOMAINS: [&str; 6] = ["local", "localhost", "home.arpa", "internal", "lan", "home"];

/// How often monitors are read, when the settings don't say.
pub const DEFAULT_POLL_SECONDS: u32 = 60;

/// The most monitors read. A home has a handful; a longer list is a mistake.
pub const MAX_HOSTS: usize = 32;

/// A name is at most this long in the DNS.
const MAX_HOST_LEN: usize = 253;

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Settings {
    /// The monitors to read, at most 32. Each one is a private, loopback, or link-local IPv4
    /// address, or a name under `.local`, `.home.arpa`, `.internal`, `.lan`, or `.home`, such as
    /// the `airgradient_<serial number>.local` a monitor gives itself. A port, a bare name, an
    /// IPv6 address, a public address, or an internet hostname is refused.
    #[serde(default, deserialize_with = "known_hosts")]
    #[schemars(length(max = 32))]
    pub known_hosts: Vec<String>,
    /// Seconds between readings of each monitor, 10 to 3600. 60 when left out.
    #[serde(default, deserialize_with = "poll_seconds")]
    #[schemars(range(min = 10, max = 3600))]
    pub poll_seconds: Option<u32>,
}

impl Settings {
    pub fn poll(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(self.poll_seconds.unwrap_or(DEFAULT_POLL_SECONDS)))
    }
}

fn poll_seconds<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u32>, D::Error> {
    let value = Option::<u32>::deserialize(deserializer)?;
    match value {
        Some(seconds) if !(10..=3600).contains(&seconds) => Err(serde::de::Error::custom(
            "poll_seconds is between 10 and 3600",
        )),
        other => Ok(other),
    }
}

fn known_hosts<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let hosts = Vec::<String>::deserialize(deserializer)?;
    let mut out = Vec::new();
    for host in hosts {
        let host = normalize_host(&host).map_err(serde::de::Error::custom)?;
        if !out.contains(&host) {
            out.push(host);
        }
        if out.len() > MAX_HOSTS {
            return Err(serde::de::Error::custom(
                "known_hosts lists at most 32 monitors",
            ));
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
    if host.len() > MAX_HOST_LEN {
        return Err("a known host is too long".into());
    }
    if let Ok(ip) = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    {
        return match ip {
            IpAddr::V6(_) => Err("a known host has to be an IPv4 address or a name".into()),
            IpAddr::V4(_) if is_lan_ip(ip) => Ok(ip.to_string()),
            IpAddr::V4(_) => Err("a known host is not on the local network".into()),
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

/// Whether an address is on the local network. Asked of every address a name resolves to, before
/// anything is sent there.
pub(crate) fn is_lan_ip(ip: IpAddr) -> bool {
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
    // A bare `local` or `lan` names no monitor: there has to be a name under it.
    name == "localhost"
        || LOCAL_DOMAINS
            .iter()
            .any(|domain| name.ends_with(&format!(".{domain}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(value: serde_json::Value) -> Result<Settings, String> {
        serde_json::from_value(value).map_err(|error| error.to_string())
    }

    #[test]
    fn empty_settings_read_nothing_once_a_minute() {
        let settings = load(serde_json::json!({})).expect("empty settings are valid");
        assert!(settings.known_hosts.is_empty());
        assert_eq!(settings.poll().as_secs(), 60);
    }

    #[test]
    fn a_known_host_has_to_be_on_the_local_network() {
        for host in [
            "10.0.0.21",
            "127.0.0.1",
            "169.254.3.4",
            "monitor.home.arpa",
            "airgradient_0a1b2c3d4e5f.local",
            "AirGradient_0A1B2C3D4E5F.local.",
        ] {
            let settings = load(serde_json::json!({ "known_hosts": [host] }))
                .unwrap_or_else(|error| panic!("{host} should be local: {error}"));
            assert_eq!(settings.known_hosts.len(), 1, "{host}");
        }
        for host in [
            // A documentation address stands in for any public one.
            "192.0.2.10",
            "monitor.example.com",
            "monitor",
            "10.0.0.21:80",
            "http://10.0.0.21",
            "10.0.0.21/measures/current",
            "",
            "fd12::1",
            "[fd12::1]",
        ] {
            let error = load(serde_json::json!({ "known_hosts": [host] }))
                .expect_err("the host should be refused");
            assert!(!error.contains(host) || host.is_empty(), "{error}");
        }
    }

    #[test]
    fn a_local_domain_on_its_own_names_no_monitor() {
        for host in ["local", "lan", "home.arpa"] {
            assert!(
                load(serde_json::json!({ "known_hosts": [host] })).is_err(),
                "{host}"
            );
        }
    }

    #[test]
    fn a_name_is_kept_in_one_spelling_and_listed_once() {
        let settings = load(serde_json::json!({
            "known_hosts": ["Monitor.LAN", "monitor.lan.", " monitor.lan "]
        }))
        .expect("valid");
        assert_eq!(settings.known_hosts, ["monitor.lan"]);
    }

    #[test]
    fn more_than_32_monitors_is_refused() {
        let hosts = |count: usize| -> Vec<String> {
            (0..count).map(|n| format!("monitor{n}.lan")).collect()
        };
        assert_eq!(
            load(serde_json::json!({ "known_hosts": hosts(32) }))
                .expect("32 fit")
                .known_hosts
                .len(),
            32
        );
        assert!(load(serde_json::json!({ "known_hosts": hosts(33) })).is_err());
    }

    #[test]
    fn polling_stays_between_10_and_3600_seconds() {
        assert!(load(serde_json::json!({ "poll_seconds": 9 })).is_err());
        assert!(load(serde_json::json!({ "poll_seconds": 3601 })).is_err());
        let settings = load(serde_json::json!({ "poll_seconds": 30 })).expect("valid");
        assert_eq!(settings.poll().as_secs(), 30);
    }

    #[test]
    fn an_unknown_setting_is_refused() {
        assert!(load(serde_json::json!({ "led_bar": "off" })).is_err());
    }
}
