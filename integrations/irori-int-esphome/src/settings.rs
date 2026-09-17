//! What the ESPHome integration can be told: encryption keys, by device.
//!
//! Kept in `secrets.toml`, in the integration's own table (`docs/specs/config.md` §3.4):
//!
//! ```toml
//! [esphome.keys]
//! "30:83:98:CA:6A:08" = "base64 key from the device's YAML"
//! ```
//!
//! Keyed by MAC address because that's the one thing a device announces before anyone connects
//! to it, and it's the same handle the device has in the registry once it's in (ROADMAP D31).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use base64::Engine as _;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer};

/// The integration's settings.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Encryption keys, by the MAC address of the device each one is for.
    ///
    /// A map key that isn't a MAC is skipped the same way a bad *value* is kept as a reason
    /// ([`GivenKey`]): refusing the whole table would disconnect every device over one typo.
    #[serde(default, deserialize_with = "keys_by_mac")]
    pub keys: BTreeMap<Mac, GivenKey>,
}

fn keys_by_mac<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<Mac, GivenKey>, D::Error> {
    let raw = BTreeMap::<String, GivenKey>::deserialize(deserializer)?;
    let mut keys = BTreeMap::new();
    for (text, given) in raw {
        match text.parse::<Mac>() {
            Ok(mac) => {
                keys.insert(mac, given);
            }
            Err(why) => {
                tracing::warn!(
                    entry = %text,
                    reason = %why,
                    "esphome.keys entry skipped; not a MAC address"
                );
            }
        }
    }
    Ok(keys)
}

/// A key as it was given: usable, or why not.
///
/// A key that isn't a key is that device's problem and nobody else's. Refusing the whole table
/// would fail the integration and disconnect every device it runs — all over one mistyped paste.
/// So a bad key is kept as the reason its device is still waiting, and everything else carries on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GivenKey(pub Result<Key, String>);

impl<'de> Deserialize<'de> for GivenKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(GivenKey(
            Key::deserialize(deserializer).map_err(|e: D::Error| e.to_string()),
        ))
    }
}

impl JsonSchema for GivenKey {
    fn schema_name() -> Cow<'static, str> {
        Key::schema_name()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        Key::json_schema(generator)
    }
}

/// A device's MAC address, the way ESPHome reports it once connected: `30:83:98:CA:6A:08`.
///
/// Read from any of the forms people actually have to hand — the one the device's web page
/// shows, the one mDNS announces (`308398ca6a08`), dashes instead of colons — so a key pasted
/// from wherever someone found the address still finds its device.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mac(String);

impl Mac {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Mac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for Mac {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let hex: String = text
            .chars()
            .filter(|c| !matches!(c, ':' | '-' | '.'))
            .collect();
        if hex.len() != 12 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!(
                "`{text}` isn't a MAC address (six pairs of hex digits, like 30:83:98:CA:6A:08)"
            ));
        }
        let pairs: Vec<String> = hex
            .to_ascii_uppercase()
            .as_bytes()
            .chunks(2)
            .map(|pair| String::from_utf8_lossy(pair).into_owned())
            .collect();
        Ok(Self(pairs.join(":")))
    }
}

impl<'de> Deserialize<'de> for Mac {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Mac {
    fn schema_name() -> Cow<'static, str> {
        "MacAddress".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "A MAC address, e.g. 30:83:98:CA:6A:08. Colons, dashes, or none.",
        })
    }
}

/// An ESPHome API encryption key: 32 bytes, base64, as `api: encryption: key:` has it.
///
/// Never shown: `Debug` prints nothing of it, and a key that doesn't parse is described without
/// being quoted, because the description goes to the log.
#[derive(Clone, PartialEq, Eq)]
pub struct Key(String);

impl Key {
    /// For handing to the client library, and nowhere else.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key(…)")
    }
}

impl std::str::FromStr for Key {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let text = text.trim();
        match base64::engine::general_purpose::STANDARD.decode(text) {
            Ok(bytes) if bytes.len() == 32 => Ok(Self(text.to_owned())),
            Ok(bytes) => Err(format!(
                "an encryption key is 32 bytes, and this one is {}; copy the whole `key:` value \
                 from the device's YAML",
                bytes.len()
            )),
            Err(_) => Err(
                "an encryption key is base64, and this isn't; copy the whole `key:` value from the \
                 device's YAML"
                    .to_owned(),
            ),
        }
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Through a visitor rather than `String::deserialize`: serde's own "invalid type" errors
        // quote the value they got, and this value is a secret.
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Key;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an encryption key, as base64 text")
            }
            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Key, E> {
                text.parse().map_err(E::custom)
            }
            // Every other shape gets the same plain answer. Left to serde, these would be
            // "invalid type: integer `12345`" — quoting the very thing that mustn't be quoted.
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Key, E> {
                Err(E::custom(NOT_TEXT))
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Key, E> {
                Err(E::custom(NOT_TEXT))
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Key, E> {
                Err(E::custom(NOT_TEXT))
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Key, E> {
                Err(E::custom(NOT_TEXT))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Key, E> {
                Err(E::custom(NOT_TEXT))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, _: A) -> Result<Key, A::Error> {
                Err(serde::de::Error::custom(NOT_TEXT))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, _: A) -> Result<Key, A::Error> {
                Err(serde::de::Error::custom(NOT_TEXT))
            }
        }
        const NOT_TEXT: &str = "an encryption key has to be text: the `key:` value from the \
                                device's YAML, in quotes";
        deserializer.deserialize_any(Visitor)
    }
}

impl JsonSchema for Key {
    fn schema_name() -> Cow<'static, str> {
        "EncryptionKey".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "The device's API encryption key: `api: encryption: key:` in its YAML.",
            "writeOnly": true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "px7tsbK3C7bpXHr2OevEV2ZMg/FrNBw2+O2pNPbedtA=";

    #[test]
    fn a_mac_is_read_from_whatever_form_it_was_copied_in() {
        for written in [
            "30:83:98:CA:6A:08",
            "30:83:98:ca:6a:08",
            "308398ca6a08",
            "30-83-98-CA-6A-08",
        ] {
            let mac: Mac = written.parse().expect("a MAC");
            assert_eq!(mac.as_str(), "30:83:98:CA:6A:08", "{written}");
        }
        assert!("30:83:98:CA:6A".parse::<Mac>().is_err());
        assert!("not a mac at all".parse::<Mac>().is_err());
    }

    #[test]
    fn settings_read_from_the_shape_the_docs_show() {
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "keys": { "308398ca6a08": KEY }
        }))
        .expect("valid");
        let mac: Mac = "30:83:98:CA:6A:08".parse().expect("a MAC");
        assert_eq!(settings.keys[&mac].0.as_ref().map(Key::expose), Ok(KEY));
    }

    /// A bad key doesn't fail the settings — that would take every device down with it — and
    /// the reason it's bad never repeats the key.
    #[test]
    fn a_bad_key_is_kept_as_a_reason_and_never_quoted() {
        let mac: Mac = "30:83:98:CA:6A:08".parse().expect("a MAC");
        let other: Mac = "aa:bb:cc:dd:ee:ff".parse().expect("a MAC");
        for (bad, shown) in [
            (serde_json::json!("c2hvcnQ="), "c2hvcnQ="),
            (serde_json::json!("not base64 at all!"), "not base64"),
            (serde_json::json!("px7tsbK3C7bpXHr2OevEV2ZMg"), "px7tsbK3"),
            (serde_json::json!(12345), "12345"),
        ] {
            let settings: Settings = serde_json::from_value(serde_json::json!({
                "keys": { "308398ca6a08": bad, "aabbccddeeff": KEY }
            }))
            .expect("the settings as a whole are fine");
            let why = settings.keys[&mac].0.clone().expect_err("a bad key");
            assert!(!why.contains(shown), "{why}");
            assert!(
                settings.keys[&other].0.is_ok(),
                "the good key is unaffected"
            );
        }
    }

    #[test]
    fn a_key_never_shows_up_in_debug_output() {
        let key: Key = KEY.parse().expect("valid");
        assert!(!format!("{key:?}").contains("px7t"));
    }

    /// A map key that isn't a MAC must not fail the whole table — same reason a bad value
    /// doesn't: one typo would take every encrypted device down with it.
    #[test]
    fn a_bad_mac_key_is_skipped_and_the_rest_load() {
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "keys": {
                "not-a-mac": KEY,
                "308398ca6a08": KEY,
                "also bad!!": KEY
            }
        }))
        .expect("the settings as a whole are fine");
        let mac: Mac = "30:83:98:CA:6A:08".parse().expect("a MAC");
        assert_eq!(settings.keys.len(), 1);
        assert_eq!(settings.keys[&mac].0.as_ref().map(Key::expose), Ok(KEY));
    }
}
