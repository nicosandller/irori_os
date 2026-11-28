//! What the Bambu Lab extension can be told: where the printer is, its serial number, and its
//! access code.
//!
//! All three are required, so until they're given the extension waits in "needs setup". The
//! access code is a secret (`writeOnly`): kept in `secrets.toml`, never sent back to a page,
//! never printed. It is tied to the address (`x-irori-secret-for`): the core clears it when
//! the address changes, so it's never sent to a host it wasn't entered for.
//!
//! The manifest asks for `lan` and nothing else, so the address is checked here by what is
//! written, without looking it up, the way the MQTT extension checks its broker.

use std::fmt;
use std::net::IpAddr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer};

/// The port the printer's MQTT listens on.
pub const DEFAULT_PORT: u16 = 8883;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// The printer's address on the local network, as the network settings on its screen show
    /// it. A private, loopback or link-local IP is accepted, as is a name under `.local`,
    /// `.home.arpa`, `.internal`, `.lan` or `.home`. A public address or an internet hostname
    /// is refused.
    #[schemars(title = "Printer address")]
    #[serde(deserialize_with = "local_host")]
    pub host: String,
    /// The printer's serial number, as its screen or its label shows it.
    #[schemars(title = "Serial number")]
    #[serde(deserialize_with = "serial")]
    pub serial: String,
    /// The access code the network settings on the printer's screen show. It is the
    /// printer's local password, and it is only ever sent to the address above.
    ///
    /// The printer shows a certificate of its own. On the first connection the extension takes
    /// the certificate it is shown, sends the access code, and remembers that certificate: it
    /// does not check that Bambu Lab issued it. From then on the access code only goes to a
    /// printer showing the same certificate.
    #[schemars(title = "Access code", extend("x-irori-secret-for" = "host"))]
    pub access_code: Secret,
    /// Optional. The SHA-256 fingerprint of the printer's certificate, 64 hex digits. With it,
    /// the access code is only ever sent to a printer showing that certificate, from the first
    /// connection on. It is also how to accept a new certificate after the printer's changed:
    /// the extension's card shows the fingerprint it was offered.
    #[schemars(title = "Trust only this certificate")]
    #[serde(default, deserialize_with = "fingerprint")]
    pub certificate: Option<String>,
    /// The printer's MQTT port. 8883 unless something in between changes it.
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

/// The access code. Never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// For handing to the printer connection, and nowhere else.
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
        let code = String::deserialize(deserializer)?;
        let code = code.trim();
        if code.is_empty() || code.len() > 64 || code.chars().any(char::is_control) {
            return Err(serde::de::Error::custom(
                "the access code is 1 to 64 characters, as the printer's screen shows it",
            ));
        }
        Ok(Secret(code.to_owned()))
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

/// The serial goes into the MQTT topics, so only letters and digits are taken.
fn serial<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let serial = String::deserialize(deserializer)?;
    let serial = serial.trim();
    if serial.is_empty() || serial.len() > 32 || !serial.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return Err(serde::de::Error::custom(
            "the serial number is 1 to 32 letters and digits",
        ));
    }
    Ok(serial.to_owned())
}

/// A SHA-256 fingerprint as 64 lowercase hex digits. Colons and spaces are taken out; nothing
/// typed means none.
fn fingerprint<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    let Some(text) = Option::<String>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let hex: String = text
        .chars()
        .filter(|c| *c != ':' && !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if hex.is_empty() {
        return Ok(None);
    }
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(serde::de::Error::custom(
            "that isn't a SHA-256 fingerprint (64 hex digits)",
        ));
    }
    Ok(Some(hex))
}

/// Names that never resolve on the public internet. The same set the MQTT extension accepts.
const LOCAL_DOMAINS: [&str; 6] = ["local", "localhost", "home.arpa", "internal", "lan", "home"];

const NOT_LOCAL: &str = "the printer address is not on the local network. This extension can only reach a printer there.";

fn local_host<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let host = String::deserialize(deserializer)?;
    let host = host.trim().trim_end_matches('.');
    if host.is_empty() {
        return Err(serde::de::Error::custom("the printer address is empty"));
    }
    let local = match host.parse::<IpAddr>() {
        Ok(ip) => is_lan_ip(ip),
        Err(_) => is_lan_name(&host.to_ascii_lowercase()),
    };
    if !local {
        return Err(serde::de::Error::custom(NOT_LOCAL));
    }
    Ok(host.to_ascii_lowercase())
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
        // fc00::/7 is IPv6's range for one network and not the internet.
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.is_unicast_link_local() || v6.segments()[0] & 0xfe00 == 0xfc00
        }
    }
}

/// A bare name such as `printer` is refused: a search domain can point it at a public address.
fn is_lan_name(name: &str) -> bool {
    let label_ok = |label: &str| {
        !label.is_empty()
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    name.split('.').all(label_ok)
        && LOCAL_DOMAINS
            .iter()
            .any(|domain| name == *domain || name.ends_with(&format!(".{domain}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(value: serde_json::Value) -> Result<Settings, String> {
        serde_json::from_value(value).map_err(|error| error.to_string())
    }

    fn with(key: &str, value: serde_json::Value) -> Result<Settings, String> {
        let mut settings = serde_json::json!({
            "host": "printer.home.arpa", "serial": "00TEST0000000001", "access_code": "12345678"
        });
        settings[key] = value;
        load(settings)
    }

    #[test]
    fn the_three_required_settings_are_enough() {
        let settings = with("port", serde_json::json!(8883)).expect("valid");
        assert_eq!(settings.host, "printer.home.arpa");
        assert_eq!(settings.port, DEFAULT_PORT);
        assert_eq!(settings.certificate, None);
        assert_eq!(settings.access_code.expose(), "12345678");
        for missing in ["host", "serial", "access_code"] {
            let mut all = serde_json::json!({
                "host": "127.0.0.1", "serial": "00TEST0000000001", "access_code": "12345678"
            });
            all.as_object_mut().expect("an object").remove(missing);
            assert!(load(all).is_err(), "{missing} is required");
        }
    }

    #[test]
    fn the_access_code_is_never_shown() {
        let settings = with("access_code", serde_json::json!("hunter22")).expect("valid");
        assert!(!format!("{settings:?}").contains("hunter22"));
        let error = with("access_code", serde_json::json!("bad\u{7}hunter22")).expect_err("bad");
        assert!(!error.contains("hunter22"), "{error}");
    }

    #[test]
    fn the_printer_has_to_be_on_the_local_network() {
        for host in ["127.0.0.1", "10.0.0.7", "printer.local", "fd00::7"] {
            assert!(with("host", serde_json::json!(host)).is_ok(), "{host}");
        }
        // 192.0.2.0/24 is a documentation range: public, as far as this check goes.
        for host in [
            "192.0.2.10",
            "printer.example.com",
            "printer",
            "",
            "10.0.0.7:8883",
        ] {
            assert!(with("host", serde_json::json!(host)).is_err(), "{host}");
        }
    }

    #[test]
    fn the_serial_is_letters_and_digits_only() {
        for serial in ["", "00TEST/#", "00TEST 01", &"A".repeat(33)] {
            assert!(
                with("serial", serde_json::json!(serial)).is_err(),
                "{serial}"
            );
        }
    }

    #[test]
    fn a_fingerprint_is_64_hex_digits_however_it_is_spaced() {
        let spaced = format!("AB:{}", "cd ".repeat(31));
        let settings = with("certificate", serde_json::json!(spaced)).expect("valid");
        assert_eq!(settings.certificate, Some(format!("ab{}", "cd".repeat(31))));
        assert_eq!(
            with("certificate", serde_json::json!(" "))
                .expect("none")
                .certificate,
            None
        );
        assert!(with("certificate", serde_json::json!("abcd")).is_err());
        assert!(with("certificate", serde_json::json!("zz".repeat(32))).is_err());
    }
}
