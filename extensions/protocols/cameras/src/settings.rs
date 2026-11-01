//! What the Cameras extension can be told.
//!
//! `hosts` is plain settings: cameras to ask directly, for a network where the discovery
//! multicast doesn't reach. Each camera's login lives in `secrets.toml`, in this extension's own
//! table, written there by the Add device screen (`docs/specs/protocols.md` §6.6):
//!
//! ```toml
//! [cameras.cameras."05121508-0115-0400-0613-ec71dbc9da16"]
//! username = "admin"
//! password = "…"
//! ```
//!
//! Keyed by the id the camera announces itself with, which is the same handle the device has
//! once it's added. Nothing here ever prints a password: [`Login`]'s `Debug` leaves it out, and a
//! login that can't be read is described without being quoted.

use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, Ipv6Addr};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};

/// Local names. The same set Cast and MQTT accept, so a public name can't pass as local.
const LOCAL_DOMAINS: [&str; 6] = ["local", "localhost", "home.arpa", "internal", "lan", "home"];

/// The longest a username or password may be. Cameras take far less; this only bounds memory.
const LOGIN_MAX: usize = 255;

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Settings {
    /// Cameras to ask directly, as well as any that answer on the network. For a network where
    /// discovery doesn't reach, such as a container's. Each one is a private, loopback or
    /// link-local address, or a name under `.local`, `.home.arpa`, `.internal`, `.lan` or
    /// `.home`. A port, a public address or an internet hostname is refused.
    #[serde(default, deserialize_with = "hosts")]
    pub hosts: Vec<String>,
    /// Each camera's login, by its id. Given on the Add device screen, never in this form.
    #[serde(default, deserialize_with = "logins")]
    #[schemars(skip)]
    pub cameras: BTreeMap<String, GivenLogin>,
    /// Where a Reolink's own API is: its HTTPS port unless a test says otherwise. Never read
    /// from the settings.
    #[serde(skip)]
    #[schemars(skip)]
    pub reolink_port: Option<u16>,
}

/// A camera's username and password.
#[derive(Clone, PartialEq, Eq)]
pub struct Login {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for Login {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Login")
            .field("username", &self.username)
            .field("password", &"…")
            .finish()
    }
}

/// A login as it was given: usable, or why not. One camera's unreadable login is that camera's
/// problem; refusing the whole table would stop every camera over it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GivenLogin(pub Result<Login, String>);

fn logins<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, GivenLogin>, D::Error> {
    let raw = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .map(|(id, value)| (id, GivenLogin(login(&value))))
        .collect())
}

/// Reads one login without ever quoting what it got.
fn login(value: &serde_json::Value) -> Result<Login, String> {
    let Some(table) = value.as_object() else {
        return Err("a camera's login is a username and a password".into());
    };
    let text = |key: &str| -> Result<String, String> {
        match table.get(key).and_then(serde_json::Value::as_str) {
            Some("") => Err(format!("the camera's {key} is empty")),
            Some(text) if text.len() > LOGIN_MAX => {
                Err(format!("the camera's {key} is over {LOGIN_MAX} bytes"))
            }
            Some(text) if text.chars().any(char::is_control) => {
                Err(format!("the camera's {key} has a control character in it"))
            }
            Some(text) => Ok(text.to_owned()),
            None => Err(format!("the camera's login has no {key}")),
        }
    };
    Ok(Login {
        username: text("username")?.trim().to_owned(),
        password: text("password")?,
    })
}

fn hosts<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
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

/// The host as it will be dialled, or why it can't be. Never repeats what was written.
fn normalize_host(host: &str) -> Result<String, String> {
    let host = host.trim().trim_end_matches('.').trim();
    if host.is_empty() {
        return Err("a camera host is empty".into());
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
            Err("a camera host is not on the local network".into())
        };
    }
    if host.contains(':') {
        return Err("a camera host can't include a port".into());
    }
    let name = host.to_ascii_lowercase();
    if is_lan_name(&name) {
        Ok(name)
    } else {
        Err("a camera host is not on the local network".into())
    }
}

fn lan_ip(text: &str) -> Result<String, String> {
    match text.parse::<IpAddr>() {
        Ok(ip) if is_lan_ip(ip) => Ok(ip.to_string()),
        _ => Err("a camera host is not on the local network".into()),
    }
}

/// Whether an address is on this network: what any address a camera hands out has to be, so a
/// camera can't point this extension at the internet.
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

pub(crate) fn is_lan_name(name: &str) -> bool {
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
    fn empty_settings_listen_on_the_network_only() {
        let settings = load(serde_json::json!({})).expect("empty settings are valid");
        assert!(settings.hosts.is_empty());
        assert!(settings.cameras.is_empty());
    }

    #[test]
    fn a_host_has_to_be_on_the_local_network() {
        let settings = load(serde_json::json!({
            "hosts": ["192.168.68.63", "cam.local", "[fe80::1]", "192.168.68.63"]
        }))
        .expect("local hosts");
        assert_eq!(settings.hosts, ["192.168.68.63", "cam.local", "fe80::1"]);
        for host in ["8.8.8.8", "camera.example.com", "192.168.68.63:8000", ""] {
            let error = load(serde_json::json!({ "hosts": [host] })).expect_err(host);
            assert!(host.is_empty() || !error.contains(host), "{error}");
        }
    }

    #[test]
    fn a_login_is_read_without_its_password_ever_showing() {
        let settings = load(serde_json::json!({
            "cameras": {
                "cam-1": { "username": " admin ", "password": "hunter2!" },
                "cam-2": { "username": "admin" },
                "cam-3": "hunter2!",
                "cam-4": { "username": "admin", "password": 12345 },
            }
        }))
        .expect("one bad login doesn't refuse the rest");
        let good = settings.cameras["cam-1"].0.as_ref().expect("a login");
        assert_eq!(good.username, "admin");
        assert_eq!(good.password, "hunter2!");
        let shown = format!("{settings:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(!shown.contains("12345"), "{shown}");
        assert_eq!(
            settings.cameras["cam-2"].0,
            Err("the camera's login has no password".into())
        );
        assert!(settings.cameras["cam-3"].0.is_err());
        assert!(settings.cameras["cam-4"].0.is_err());
    }
}
