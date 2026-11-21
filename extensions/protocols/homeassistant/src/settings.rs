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
//! `.lan` or `.home`. A public name is taken only over `https://` and when the owner says it
//! resolves locally (split DNS); `transport.rs` then connects only to a local answer.
//!
//! The token is tied to the address (`x-irori-secret-for`): the core clears it when the address
//! changes, so it's never sent to a host it wasn't entered for.
//!
//! Commands are off until the owner turns them on, so a first try only reads.

use std::net::IpAddr;

use irori_cloud::Secret;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};

#[derive(Debug, Clone, PartialEq, Eq, JsonSchema)]
pub struct Settings {
    /// Home Assistant's local address, as you open it in a browser on this network:
    /// `https://homeassistant.local:8123`, or `http://` if it doesn't use a certificate.
    #[schemars(
        title = "Home Assistant address",
        extend("x-irori-warn" = {
            "starts_with": "http://",
            "text": "Not encrypted: your Home Assistant administrator's access token travels unencrypted on your local network. Use https:// if your Home Assistant has a certificate."
        })
    )]
    pub url: String,
    /// A long-lived access token of an administrator: in Home Assistant, your profile →
    /// Security → Long-lived access tokens → Create token. An administrator's, because only
    /// they can read Home Assistant's devices, areas and entities.
    #[schemars(title = "Access token", extend("x-irori-secret-for" = "url"))]
    pub token: Secret,
    /// Off, Irori only reads Home Assistant and answers every command "read-only". On, Irori can
    /// switch, unlock, disarm and send commands through Home Assistant, and Automations flows
    /// can do the same.
    #[schemars(title = "Let Irori control devices", default = "off")]
    pub commands: bool,
    /// For a certificate no public authority issued, like Home Assistant's self-signed one: its
    /// SHA-256 fingerprint, or your certificate or CA certificate as PEM. Enter `none` to stop
    /// trusting it.
    #[schemars(
        title = "Trust this certificate",
        default = "no_text",
        extend("x-irori-group" = "Secure connection")
    )]
    pub certificate: String,
    /// Turn on if this `https://` address is a public name that your network's DNS points at
    /// Home Assistant's local address. Irori then connects only if it resolves to a local one.
    #[schemars(
        title = "This name resolves locally",
        default = "off",
        extend("x-irori-group" = "Secure connection")
    )]
    pub resolves_locally: bool,
    /// Also bring over every entity of these integrations (`hue`, `zha`, `mqtt`…), even ones
    /// Irori leaves out by default: those on no device, and hidden ones. Disabled entities never
    /// come over.
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

fn off() -> bool {
    false
}

fn no_text() -> String {
    String::new()
}

/// Which certificates an `https://` connection trusts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    /// Those a public authority issued for the name used.
    Public,
    /// Exactly the certificate with this SHA-256 fingerprint.
    Fingerprint([u8; 32]),
    /// These certificates (DER), and what chains to them for the name used.
    Certificates(Vec<Vec<u8>>),
}

/// The most text the certificate setting takes: a chain of a few certificates as PEM.
const CERTIFICATE_MOST: usize = 32 * 1024;
const PEM_BEGIN: &str = "-----BEGIN CERTIFICATE-----";
const PEM_END: &str = "-----END CERTIFICATE-----";

fn parse_trust(text: &str) -> Result<Trust, String> {
    let text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("none") {
        return Ok(Trust::Public);
    }
    if text.len() > CERTIFICATE_MOST {
        return Err("that certificate is too long".into());
    }
    if text.contains("-----BEGIN") {
        return pem_certificates(text).map(Trust::Certificates);
    }
    let hex: Vec<u8> = text
        .bytes()
        .filter(|b| *b != b':' && !b.is_ascii_whitespace())
        .collect();
    let digit = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut fingerprint = [0u8; 32];
    if hex.len() != 64 {
        return Err(NOT_A_CERTIFICATE.into());
    }
    for (byte, [high, low]) in fingerprint.iter_mut().zip(hex.as_chunks::<2>().0) {
        let (Some(high), Some(low)) = (digit(*high), digit(*low)) else {
            return Err(NOT_A_CERTIFICATE.into());
        };
        *byte = high << 4 | low;
    }
    Ok(Trust::Fingerprint(fingerprint))
}

const NOT_A_CERTIFICATE: &str =
    "that isn't a SHA-256 fingerprint (64 hex digits) or a PEM certificate";

/// The certificates in `text`. A text box takes a paste's line breaks out, so they're put back.
fn pem_certificates(text: &str) -> Result<Vec<Vec<u8>>, String> {
    let mut tidy = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(PEM_BEGIN) {
        let after = &rest[at + PEM_BEGIN.len()..];
        let end = after.find(PEM_END).ok_or(NOT_A_CERTIFICATE)?;
        let body: String = after[..end].split_whitespace().collect();
        tidy.push_str(&format!("{PEM_BEGIN}\n{body}\n{PEM_END}\n"));
        rest = &after[end + PEM_END.len()..];
    }
    let certificates = rustls_pemfile::certs(&mut tidy.as_bytes())
        .map(|certificate| certificate.map(|der| der.to_vec()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| NOT_A_CERTIFICATE.to_owned())?;
    if certificates.is_empty() {
        return Err(NOT_A_CERTIFICATE.into());
    }
    Ok(certificates)
}

impl Settings {
    pub fn is_secure(&self) -> bool {
        self.url.starts_with("https://")
    }

    fn authority(&self) -> &str {
        let base = self.url.trim_end_matches('/');
        base.strip_prefix("https://")
            .or_else(|| base.strip_prefix("http://"))
            .unwrap_or(base)
    }

    /// Where the WebSocket API is: `ws://…/api/websocket`, or `wss://` over `https://`.
    pub fn websocket_url(&self) -> String {
        let scheme = if self.is_secure() { "wss" } else { "ws" };
        format!("{scheme}://{}/api/websocket", self.authority())
    }

    /// The host (without an IPv6 address's brackets), the port, and `host[:port]` as written.
    pub fn host_port(&self) -> (String, u16, String) {
        let authority = self.authority();
        let default = if self.is_secure() { 443 } else { 80 };
        let (host, port) = split_host(authority).unwrap_or((authority, None));
        (
            host.to_owned(),
            port.unwrap_or(default),
            authority.to_owned(),
        )
    }

    /// Which certificates are trusted. Checked when the settings are read.
    pub fn trust(&self) -> Result<Trust, String> {
        parse_trust(&self.certificate)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    url: String,
    token: Secret,
    #[serde(default)]
    commands: bool,
    #[serde(default)]
    certificate: String,
    #[serde(default)]
    resolves_locally: bool,
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
        let url = checked_url(&raw.url, raw.resolves_locally).map_err(serde::de::Error::custom)?;
        parse_trust(&raw.certificate).map_err(serde::de::Error::custom)?;
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
            commands: raw.commands,
            certificate: raw.certificate.trim().to_owned(),
            resolves_locally: raw.resolves_locally,
            include_integrations: tidy(raw.include_integrations),
            exclude_integrations: tidy(raw.exclude_integrations),
            include_areas: tidy(raw.include_areas),
            exclude_areas: tidy(raw.exclude_areas),
        })
    }
}

/// `http://` or `https://` and `host[:port]`, with a host on the local network, or a public name
/// over `https://` that the owner says resolves locally. Returned without a trailing `/`.
fn checked_url(url: &str, resolves_locally: bool) -> Result<String, String> {
    let url = url.trim().trim_end_matches('/');
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(
            "the address starts with https:// or http://, like https://homeassistant.local:8123"
                .into(),
        );
    };
    if rest.contains(['/', '?', '#', '@']) {
        return Err(
            "the address is only Home Assistant's host and port, like https://homeassistant.local:8123"
                .into(),
        );
    }
    let (host, _) = split_host(rest)?;
    match ensure_lan_host(host) {
        Ok(()) => Ok(url.to_owned()),
        Err(_)
            if secure && resolves_locally && host.parse::<IpAddr>().is_err() && is_name(host) =>
        {
            Ok(url.to_owned())
        }
        Err(_) if resolves_locally && !secure => Err(format!(
            "`{host}` is a public name, so it's only taken over https://: the token must not travel unencrypted to it"
        )),
        Err(why) => Err(why),
    }
}

/// `host[:port]` or `[v6][:port]`, the host without its brackets.
fn split_host(authority: &str) -> Result<(&str, Option<u16>), String> {
    if let Some(v6) = authority.strip_prefix('[') {
        let (host, after) = v6
            .split_once(']')
            .ok_or("the address has an unclosed `[`")?;
        check_port(after.strip_prefix(':'), after.is_empty())?;
        return Ok((host, after.strip_prefix(':').and_then(|p| p.parse().ok())));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => {
            check_port(Some(port), false)?;
            Ok((host, port.parse().ok()))
        }
        None => Ok((authority, None)),
    }
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
            "`{host}` is not on the local network. This extension only reaches a Home Assistant there: use its local address, such as https://homeassistant.local:8123 or its IP address. If this https:// name points at it on your network, turn on \"This name resolves locally\"."
        ))
    }
}

pub fn is_lan_ip(ip: IpAddr) -> bool {
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

fn labels_ok(name: &str) -> bool {
    let label_ok = |label: &str| {
        !label.is_empty()
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    !name.is_empty() && name.split('.').all(label_ok)
}

/// A full DNS name, with a dot: a bare label could be completed to anything.
fn is_name(host: &str) -> bool {
    let name = host.trim_end_matches('.');
    name.contains('.') && labels_ok(name)
}

fn is_lan_name(name: &str) -> bool {
    if !labels_ok(name) {
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
            "https://8.8.8.8:8123",
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
    fn https_is_taken_and_a_public_name_only_when_it_resolves_locally() {
        let settings = load(serde_json::json!({
            "url": "https://homeassistant.local:8123/", "token": "t",
        }))
        .expect("https on a local name");
        assert_eq!(
            settings.websocket_url(),
            "wss://homeassistant.local:8123/api/websocket"
        );
        assert_eq!(settings.trust(), Ok(Trust::Public));
        let (host, port, _) = settings.host_port();
        assert_eq!((host.as_str(), port), ("homeassistant.local", 8123));

        let public = |url: &str, local: bool| {
            load(serde_json::json!({ "url": url, "token": "t", "resolves_locally": local }))
        };
        assert!(public("https://ha.example.com", true).is_ok());
        assert_eq!(
            public("https://ha.example.com", true)
                .expect("taken")
                .host_port()
                .1,
            443
        );
        assert!(public("https://ha.example.com", false).is_err());
        // Never in the clear, never a public IP, and never a bare label.
        assert!(public("http://ha.example.com", true).is_err());
        assert!(public("https://8.8.8.8", true).is_err());
        assert!(public("https://homeassistant", true).is_err());
    }

    #[test]
    fn a_certificate_is_a_fingerprint_or_pem_however_it_was_pasted() {
        let trust = |text: &str| {
            load(serde_json::json!({
                "url": "https://192.168.1.20:8123", "token": "t", "certificate": text,
            }))
            .and_then(|settings| settings.trust())
        };
        let hex = "ab".repeat(32);
        let colons = vec!["AB"; 32].join(":");
        assert_eq!(trust(&hex), Ok(Trust::Fingerprint([0xab; 32])));
        assert_eq!(trust(&colons), Ok(Trust::Fingerprint([0xab; 32])));
        assert_eq!(trust(" none "), Ok(Trust::Public));
        assert!(trust("abcd").is_err());
        assert!(trust(&"zz".repeat(32)).is_err());
        assert!(trust("-----BEGIN CERTIFICATE-----\nnot base64!").is_err());

        let made = rcgen::generate_simple_self_signed(vec!["ha.local".to_owned()]).expect("cert");
        let der = made.cert.der().to_vec();
        let pem = made.cert.pem();
        assert_eq!(trust(&pem), Ok(Trust::Certificates(vec![der.clone()])));
        // A one-line text box takes the line breaks out.
        assert_eq!(
            trust(&pem.replace('\n', " ")),
            Ok(Trust::Certificates(vec![der.clone()]))
        );
        assert_eq!(
            trust(&pem.replace('\n', "")),
            Ok(Trust::Certificates(vec![der]))
        );
    }

    #[test]
    fn commands_are_off_until_turned_on() {
        let off = load(serde_json::json!({ "url": "http://127.0.0.1:8123", "token": "t" }))
            .expect("valid");
        assert!(!off.commands);
        let on = load(serde_json::json!({
            "url": "http://127.0.0.1:8123", "token": "t", "commands": true,
        }))
        .expect("valid");
        assert!(on.commands);
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
