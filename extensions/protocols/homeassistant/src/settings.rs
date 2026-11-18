//! What the Home Assistant extension can be told: where Home Assistant is, the access token it
//! signs in with, and which of its integrations and areas to bring over.
//!
//! `url` and `token` are required, so until both are given the extension waits in "needs setup"
//! instead of starting and failing (`missing_required` in the core). The token is a secret
//! (`writeOnly`): kept in `secrets.toml`, never sent back to a page, never printed.
//!
//! The manifest asks for `lan` and nothing else, so the address is checked here by what is
//! written, without looking it up, the way the MQTT extension checks its broker: a private,
//! loopback or link-local IP, `localhost`, or a name under `.local`, `.home.arpa`, `.internal`,
//! `.lan` or `.home`.

use std::net::IpAddr;

use irori_cloud::Secret;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};

#[derive(Debug, Clone, PartialEq, Eq, JsonSchema)]
pub struct Settings {
    /// Home Assistant's local address, as you open it in a browser on this network:
    /// `http://homeassistant.local:8123`, or `http://` and its IP address.
    #[schemars(title = "Home Assistant address")]
    pub url: String,
    /// A long-lived access token of an administrator: in Home Assistant, your profile →
    /// Security → Long-lived access tokens → Create token. An administrator's, because only
    /// they can read Home Assistant's devices, areas and entities.
    #[schemars(title = "Access token")]
    pub token: Secret,
    /// Also bring over every entity of these integrations (`hue`, `zha`, `mqtt`…), even ones
    /// Irori leaves out by default: those on no device, hidden ones, and devices' settings and
    /// diagnostics. Disabled entities never come over.
    #[schemars(
        title = "Also bring over these integrations",
        default = "nothing",
        extend("x-irori-group" = "What comes over")
    )]
    pub include_integrations: Vec<String>,
    /// Never bring over entities of these integrations.
    #[schemars(
        title = "Leave out these integrations",
        default = "nothing",
        extend("x-irori-group" = "What comes over")
    )]
    pub exclude_integrations: Vec<String>,
    /// Also bring over every entity in these areas (by name), as for integrations above.
    #[schemars(
        title = "Also bring over these areas",
        default = "nothing",
        extend("x-irori-group" = "What comes over")
    )]
    pub include_areas: Vec<String>,
    /// Never bring over entities in these areas (by name).
    #[schemars(
        title = "Leave out these areas",
        default = "nothing",
        extend("x-irori-group" = "What comes over")
    )]
    pub exclude_areas: Vec<String>,
}

fn nothing() -> Vec<String> {
    Vec::new()
}

impl Settings {
    /// Where the WebSocket API is: `ws://…/api/websocket`.
    pub fn websocket_url(&self) -> String {
        let base = self.url.trim_end_matches('/');
        format!(
            "ws://{}/api/websocket",
            base.strip_prefix("http://").unwrap_or(base)
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    url: String,
    token: Secret,
    #[serde(default)]
    include_integrations: Vec<String>,
    #[serde(default)]
    exclude_integrations: Vec<String>,
    #[serde(default)]
    include_areas: Vec<String>,
    #[serde(default)]
    exclude_areas: Vec<String>,
}

impl<'de> Deserialize<'de> for Settings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Raw::deserialize(deserializer)?;
        let url = local_url(&raw.url).map_err(serde::de::Error::custom)?;
        if raw.token.expose().is_empty() {
            return Err(serde::de::Error::custom("the access token is empty"));
        }
        let tidy = |list: Vec<String>| -> Vec<String> {
            list.into_iter()
                .map(|item| item.trim().to_owned())
                .filter(|item| !item.is_empty())
                .collect()
        };
        Ok(Settings {
            url,
            token: raw.token,
            include_integrations: tidy(raw.include_integrations),
            exclude_integrations: tidy(raw.exclude_integrations),
            include_areas: tidy(raw.include_areas),
            exclude_areas: tidy(raw.exclude_areas),
        })
    }
}

/// `http://host[:port][/]`, with a host on the local network. Returned without a trailing `/`.
///
/// Not `https://`: a certificate is only trusted when a public authority issued it for the name
/// used, and none is ever issued for a local name or a private address, so it could never connect.
fn local_url(url: &str) -> Result<String, String> {
    let url = url.trim().trim_end_matches('/');
    if url.starts_with("https://") {
        return Err("use http:// and Home Assistant's local address, like http://homeassistant.local:8123: a certificate can't be checked for a local address, so https:// would never connect".into());
    }
    let rest = url
        .strip_prefix("http://")
        .ok_or("the address starts with http://, like http://homeassistant.local:8123")?;
    if rest.contains(['/', '?', '#', '@']) {
        return Err(
            "the address is only Home Assistant's host and port, like http://homeassistant.local:8123"
                .into(),
        );
    }
    let host = match rest.strip_prefix('[') {
        // An IPv6 address, `[fd00::1]:8123`.
        Some(v6) => {
            let (host, after) = v6
                .split_once(']')
                .ok_or("the address has an unclosed `[`")?;
            check_port(after.strip_prefix(':'), after.is_empty())?;
            host
        }
        None => match rest.rsplit_once(':') {
            Some((host, port)) => {
                check_port(Some(port), false)?;
                host
            }
            None => rest,
        },
    };
    ensure_lan_host(host)?;
    Ok(url.to_owned())
}

fn check_port(port: Option<&str>, none_is_fine: bool) -> Result<(), String> {
    match port {
        Some(port) if port.parse::<u16>().is_ok_and(|port| port > 0) => Ok(()),
        None if none_is_fine => Ok(()),
        _ => Err("the address's port isn't a number from 1 to 65535".into()),
    }
}

/// Names that never resolve on the public internet. The same set the core's `network`
/// permission refuses, so a local name can't pass as an internet host or the other way round.
const LOCAL_DOMAINS: [&str; 6] = ["local", "localhost", "home.arpa", "internal", "lan", "home"];

fn ensure_lan_host(host: &str) -> Result<(), String> {
    if host.is_empty() {
        return Err("the address has no host".into());
    }
    let local = match host.parse::<IpAddr>() {
        Ok(ip) => is_lan_ip(ip),
        Err(_) => is_lan_name(&host.trim_end_matches('.').to_ascii_lowercase()),
    };
    if local {
        Ok(())
    } else {
        Err(format!(
            "`{host}` is not on the local network. This extension only reaches a Home Assistant there: use its local address, such as http://homeassistant.local:8123 or its IP address."
        ))
    }
}

fn is_lan_ip(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.is_unicast_link_local() || v6.segments()[0] & 0xfe00 == 0xfc00
        }
    }
}

fn is_lan_name(name: &str) -> bool {
    let label_ok = |label: &str| {
        !label.is_empty()
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    if name.is_empty() || !name.split('.').all(label_ok) {
        return false;
    }
    // A bare label (`homeassistant`) can be completed by a search domain, or mapped in
    // `/etc/hosts`, to a public address; only names defined to be local are taken unresolved.
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
    fn the_address_and_token_are_required_so_the_extension_waits_for_setup() {
        let schema = serde_json::to_value(schemars::schema_for!(Settings)).expect("a schema");
        assert_eq!(
            schema.get("required"),
            Some(&serde_json::json!(["url", "token"])),
            "{schema:#}"
        );
        assert_eq!(
            schema["properties"]["token"]["$ref"],
            serde_json::json!("#/$defs/Secret")
        );
        assert_eq!(
            schema["$defs"]["Secret"]["writeOnly"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn a_local_address_is_taken_and_becomes_the_websocket_url() {
        let settings = load(serde_json::json!({
            "url": " http://homeassistant.local:8123/ ",
            "token": " abc\n",
        }))
        .expect("valid");
        assert_eq!(settings.url, "http://homeassistant.local:8123");
        assert_eq!(
            settings.websocket_url(),
            "ws://homeassistant.local:8123/api/websocket"
        );
        assert_eq!(settings.token.expose(), "abc");
        assert!(!format!("{settings:?}").contains("abc"));
        for (url, ws) in [
            (
                "http://192.168.1.20:8123",
                "ws://192.168.1.20:8123/api/websocket",
            ),
            ("http://127.0.0.1:9", "ws://127.0.0.1:9/api/websocket"),
            ("http://[fd00::5]:8123", "ws://[fd00::5]:8123/api/websocket"),
            ("http://ha.home.arpa", "ws://ha.home.arpa/api/websocket"),
        ] {
            let settings = load(serde_json::json!({ "url": url, "token": "t" })).expect(url);
            assert_eq!(settings.websocket_url(), ws);
        }
    }

    #[test]
    fn an_address_off_the_local_network_is_refused() {
        for url in [
            "https://example.ui.nabu.casa",
            "https://homeassistant.local:8123",
            "https://192.168.1.20:8123",
            "http://8.8.8.8:8123",
            "http://homeassistant:8123",
            "homeassistant.local:8123",
            "ws://homeassistant.local:8123",
            "http://homeassistant.local:8123/lovelace",
            "http://user@homeassistant.local",
            "http://homeassistant.local:0",
        ] {
            assert!(
                load(serde_json::json!({ "url": url, "token": "t" })).is_err(),
                "{url} was taken"
            );
        }
    }

    #[test]
    fn an_empty_token_is_refused_without_echoing_it() {
        let error = load(serde_json::json!({ "url": "http://127.0.0.1:8123", "token": "  " }))
            .expect_err("empty");
        assert!(error.contains("token"), "{error}");
    }

    #[test]
    fn the_lists_are_tidied() {
        let settings = load(serde_json::json!({
            "url": "http://127.0.0.1:8123",
            "token": "t",
            "include_integrations": [" hue ", ""],
            "exclude_areas": ["Garage"],
        }))
        .expect("valid");
        assert_eq!(settings.include_integrations, ["hue"]);
        assert_eq!(settings.exclude_areas, ["Garage"]);
        assert!(settings.exclude_integrations.is_empty());
    }
}
