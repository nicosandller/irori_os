//! What Google Cast can be told. Nothing here is a secret: there is no account.
//!
//! Left empty, discovery is mDNS only. A known host is checked by what is written, not by looking
//! it up, and a refusal never repeats the address.

use std::net::{IpAddr, Ipv6Addr};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};

/// Local names. The same set MQTT accepts, so a public name can't be written down as local.
const LOCAL_DOMAINS: [&str; 6] = ["local", "localhost", "home.arpa", "internal", "lan", "home"];

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Settings {
    /// Receivers to connect to by address, as well as anything found on the network. Empty means
    /// the network announcement is the whole list. Each one has to be a private, loopback, or
    /// link-local address, or a name under `.local`, `.home.arpa`, `.internal`, `.lan`, or
    /// `.home`. A port, a bare name, a public address, or an internet hostname is refused.
    #[serde(default, deserialize_with = "known_hosts")]
    pub known_hosts: Vec<String>,
    /// Which receivers to adopt, as 32 lowercase hex characters with no dashes. Empty adopts
    /// every one that announces itself.
    #[serde(default, deserialize_with = "uuids")]
    pub uuids: Vec<String>,
    /// Receivers whose HDMI standby and active-input flags are ignored, each a device id or the
    /// receiver's exact name. A speaker already ignores those flags.
    #[serde(default, deserialize_with = "ignore_cec")]
    pub ignore_cec: Vec<String>,
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

fn uuids<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let values = Vec::<String>::deserialize(deserializer)?;
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(serde::de::Error::custom(
                "a Cast device id is 32 lowercase hex characters",
            ));
        }
        if !out.contains(&value) {
            out.push(value);
        }
    }
    Ok(out)
}

fn ignore_cec<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let values = Vec::<String>::deserialize(deserializer)?;
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let value = value.trim();
        if value.is_empty() {
            return Err(serde::de::Error::custom("an ignore_cec entry is blank"));
        }
        if value.contains('*') {
            return Err(serde::de::Error::custom(
                "ignore_cec doesn't take a wildcard",
            ));
        }
        let stored = uuid_of(value).unwrap_or_else(|| value.to_owned());
        if !out.contains(&stored) {
            out.push(stored);
        }
    }
    Ok(out)
}

/// A device id written with an optional `uuid:` prefix and dashes, in either case. Anything else
/// is a name.
pub(crate) fn uuid_of(value: &str) -> Option<String> {
    let value = value.trim();
    let value = match value.get(..5) {
        Some(prefix) if prefix.eq_ignore_ascii_case("uuid:") => &value[5..],
        _ => value,
    };
    let stripped: String = value.chars().filter(|c| *c != '-').collect();
    if stripped.len() == 32 && stripped.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Some(stripped.to_ascii_lowercase())
    } else {
        None
    }
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
    fn empty_settings_listen_for_announcements_only() {
        let settings = load(serde_json::json!({})).expect("empty settings are valid");
        assert!(settings.known_hosts.is_empty());
        assert!(settings.uuids.is_empty());
        assert!(settings.ignore_cec.is_empty());
    }

    #[test]
    fn a_known_host_has_to_be_on_the_local_network() {
        for host in [
            "192.168.1.10",
            "10.0.0.2",
            "127.0.0.1",
            "169.254.1.1",
            "::1",
            "[::1]",
            "fd12::1",
            "fe80::1",
            "localhost",
            "tv.local",
            "cast.home.arpa",
            "Living-Room.LAN",
        ] {
            let settings = load(serde_json::json!({ "known_hosts": [host] }))
                .unwrap_or_else(|error| panic!("{host} should be local: {error}"));
            assert_eq!(settings.known_hosts.len(), 1, "{host}");
        }
    }

    #[test]
    fn a_public_or_ported_host_is_refused_without_being_repeated() {
        for host in [
            "8.8.8.8",
            "1.1.1.1",
            "2001:db8::1",
            "chromecast.example.com",
            "livingroom",
            "192.168.1.10:8008",
            "[::1]:8008",
            "",
        ] {
            let error = load(serde_json::json!({ "known_hosts": [host] }))
                .expect_err("the host should be refused");
            assert!(!error.contains(host) || host.is_empty(), "{error}");
            assert!(!error.contains('`'), "{error}");
        }
    }

    #[test]
    fn device_ids_stay_lowercase_hex() {
        let settings = load(serde_json::json!({
            "uuids": ["00112233445566778899aabbccddeeff"]
        }))
        .expect("lowercase id");
        assert_eq!(settings.uuids, ["00112233445566778899aabbccddeeff"]);

        let upper = "00112233445566778899AABBCCDDEEFF";
        let error = load(serde_json::json!({ "uuids": [upper] })).expect_err("upper case");
        assert!(!error.contains(upper), "{error}");
        assert!(!error.contains(&upper.to_ascii_lowercase()), "{error}");
    }

    #[test]
    fn ignore_cec_takes_an_id_or_an_exact_name() {
        let settings = load(serde_json::json!({
            "ignore_cec": ["00112233-4455-6677-8899-AABBCCDDEEFF", "Living room"]
        }))
        .expect("valid");
        assert_eq!(
            settings.ignore_cec,
            ["00112233445566778899aabbccddeeff", "Living room"]
        );

        let wildcard = load(serde_json::json!({ "ignore_cec": ["*"] })).expect_err("wildcard");
        assert!(wildcard.contains("wildcard"), "{wildcard}");
        assert!(!wildcard.contains('`'), "{wildcard}");

        let blank = load(serde_json::json!({ "ignore_cec": ["  "] })).expect_err("blank");
        assert!(blank.contains("blank"), "{blank}");
    }
}
