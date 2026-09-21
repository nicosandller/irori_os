//! Identifiers and validated strings. See `docs/specs/entities.md` §3.
//!
//! Every type here validates on construction and on deserialization, so a value that exists
//! is well-formed. Their JSON Schemas carry the same rules as patterns and lengths.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use crate::EntityKind;

/// Why a string was rejected as an identifier or name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdError {
    what: &'static str,
    value: String,
    reason: &'static str,
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid {} {:?}: {}", self.what, self.value, self.reason)
    }
}

impl std::error::Error for IdError {}

pub(crate) fn err(what: &'static str, value: &str, reason: &'static str) -> IdError {
    IdError {
        what,
        value: value.to_owned(),
        reason,
    }
}

/// Maximum length of a slug and of an entity's object id.
pub const SLUG_MAX_LEN: usize = 64;

/// JSON Schema (ECMA-262) pattern for a slug.
const SLUG_PATTERN: &str = "^[a-z0-9]+(_[a-z0-9]+)*$";

/// Lowercase ASCII letters and digits in `_`-separated words: no leading, trailing, or doubled `_`.
fn check_slug(what: &'static str, value: &str) -> Result<(), IdError> {
    if value.is_empty() {
        return Err(err(what, value, "must not be empty"));
    }
    if value.len() > SLUG_MAX_LEN {
        return Err(err(what, value, "must be at most 64 characters"));
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(err(
            what,
            value,
            "may only contain lowercase letters a-z, digits, and `_`",
        ));
    }
    if value.starts_with('_') || value.ends_with('_') || value.contains("__") {
        return Err(err(
            what,
            value,
            "`_` must separate words (no leading, trailing, or doubled `_`)",
        ));
    }
    Ok(())
}

/// Implements the string plumbing shared by every validated string type:
/// `TryFrom<String>`, `FromStr`, `Display`, `as_str`, and serde through `String`.
macro_rules! string_newtype {
    ($name:ident, $check:expr) => {
        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = $crate::IdError;
            fn try_from(value: String) -> Result<Self, $crate::IdError> {
                let check: fn(&str) -> Result<(), $crate::IdError> = $check;
                check(&value)?;
                Ok(Self(value))
            }
        }

        impl TryFrom<&str> for $name {
            type Error = $crate::IdError;
            fn try_from(value: &str) -> Result<Self, $crate::IdError> {
                Self::try_from(value.to_owned())
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = $crate::IdError;
            fn from_str(value: &str) -> Result<Self, $crate::IdError> {
                Self::try_from(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> String {
                value.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

pub(crate) use string_newtype;

/// A slug identifier type: `^[a-z0-9]+(_[a-z0-9]+)*$`, 1–64 characters.
macro_rules! slug_id {
    ($(#[$meta:meta])* $name:ident, $what:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        string_newtype!($name, |v| check_slug($what, v));

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "string",
                    "description": concat!(
                        "Format of a ", "slug (", $what, "): lowercase letters a-z and digits, in words separated by single `_`. 1-64 characters."
                    ),
                    "pattern": SLUG_PATTERN,
                    "minLength": 1,
                    "maxLength": SLUG_MAX_LEN,
                })
            }
        }
    };
}

slug_id!(
    /// Identifies a floor, e.g. `ground_floor`.
    FloorId,
    "floor id"
);
slug_id!(
    /// Identifies an area (a room or zone), e.g. `hallway`.
    AreaId,
    "area id"
);
slug_id!(
    /// Identifies a device, e.g. `hallway_motion_sensor`.
    DeviceId,
    "device id"
);
slug_id!(
    /// Identifies an protocol, e.g. `mqtt`. Equals the id of the extension that contributes
    /// it (ROADMAP D25).
    ProtocolId,
    "protocol id"
);
slug_id!(
    /// Identifies an extension, e.g. `esphome`. See `docs/specs/extensions.md`.
    ExtensionId,
    "extension id"
);
slug_id!(
    /// The part of an [`EntityId`] after the `.`, e.g. `hallway` in `light.hallway`.
    ObjectId,
    "object id"
);
slug_id!(
    /// Identifies a user.
    UserId,
    "user id"
);
slug_id!(
    /// Identifies a rule.
    RuleId,
    "rule id"
);
slug_id!(
    /// Identifies an API access token (not the secret itself).
    TokenId,
    "token id"
);
slug_id!(
    /// A key in an entity's free-form `attributes` map.
    AttributeKey,
    "attribute key"
);

/// Identifies an entity: `<kind>.<object_id>`, e.g. `light.hallway`.
///
/// The part before the dot is the entity's [`EntityKind`], so the kind is always known from
/// the id alone. The object id is a slug of 1–64 characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EntityId(String);

string_newtype!(EntityId, check_entity_id);

fn check_entity_id(value: &str) -> Result<(), IdError> {
    const WHAT: &str = "entity id";
    let Some((kind, object_id)) = value.split_once('.') else {
        return Err(err(
            WHAT,
            value,
            "must be `<kind>.<object_id>`, e.g. `light.hallway`",
        ));
    };
    if EntityKind::from_domain(kind).is_none() {
        return Err(err(
            WHAT,
            value,
            "unknown kind before the `.` (expected one of: light, switch, sensor, binary_sensor)",
        ));
    }
    check_slug(WHAT, object_id)
}

impl EntityId {
    /// Builds `<kind>.<object_id>`.
    pub fn new(kind: EntityKind, object_id: &str) -> Result<Self, IdError> {
        Self::try_from(format!("{}.{object_id}", kind.domain()))
    }

    /// The kind named by the id's domain.
    pub fn kind(&self) -> EntityKind {
        self.0
            .split_once('.')
            .and_then(|(domain, _)| EntityKind::from_domain(domain))
            .expect("EntityId is validated on construction")
    }

    /// The part after the `.`.
    pub fn object_id(&self) -> &str {
        self.0
            .split_once('.')
            .map(|(_, object_id)| object_id)
            .expect("EntityId is validated on construction")
    }
}

impl JsonSchema for EntityId {
    fn schema_name() -> Cow<'static, str> {
        "EntityId".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        // One branch per kind, so the 64-character limit applies to the object id whatever the
        // kind's length (a single maxLength would let `light.` ids run 7 characters too long).
        let branches: Vec<_> = EntityKind::ALL
            .iter()
            .map(|kind| {
                json_schema!({
                    "pattern": format!("^{}\\.[a-z0-9]+(_[a-z0-9]+)*$", kind.domain()),
                    "maxLength": kind.domain().len() + 1 + SLUG_MAX_LEN,
                })
            })
            .collect();
        json_schema!({
            "type": "string",
            "description": "An entity id: `<kind>.<object_id>`, e.g. `light.hallway`. The object id uses lowercase letters a-z and digits in words separated by single `_`, 1-64 characters.",
            "anyOf": branches,
        })
    }
}

/// Identifies a context: a ULID (26 characters, Crockford base32, uppercase).
///
/// The core generates these; ULIDs sort by creation time, which keeps "what caused what"
/// chains easy to order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContextId(String);

string_newtype!(ContextId, check_ulid);

fn check_ulid(value: &str) -> Result<(), IdError> {
    const WHAT: &str = "context id";
    const CROCKFORD: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    if value.len() != 26 {
        return Err(err(WHAT, value, "must be a 26-character ULID"));
    }
    if !value.bytes().all(|b| CROCKFORD.contains(&b)) {
        return Err(err(
            WHAT,
            value,
            "must be an uppercase Crockford base32 ULID (no I, L, O, U)",
        ));
    }
    // The first character carries the top bits of a 48-bit timestamp, so it can't exceed 7.
    if value.as_bytes()[0] > b'7' {
        return Err(err(
            WHAT,
            value,
            "ULID timestamp overflows (first character must be 0-7)",
        ));
    }
    Ok(())
}

impl JsonSchema for ContextId {
    fn schema_name() -> Cow<'static, str> {
        "ContextId".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "A ULID: 26 characters, uppercase Crockford base32.",
            "pattern": "^[0-7][0-9A-HJKMNP-TV-Z]{25}$",
        })
    }
}

/// The stable identifier an protocol gives a device or entity, e.g. a Zigbee IEEE address.
/// Unique within that protocol; 1–255 characters, no control characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct UniqueId(String);

string_newtype!(UniqueId, check_unique_id);

fn check_unique_id(value: &str) -> Result<(), IdError> {
    const WHAT: &str = "unique id";
    if value.is_empty() {
        return Err(err(WHAT, value, "must not be empty"));
    }
    if value.chars().count() > 255 {
        return Err(err(WHAT, value, "must be at most 255 characters"));
    }
    if value.chars().any(char::is_control) {
        return Err(err(WHAT, value, "must not contain control characters"));
    }
    Ok(())
}

impl JsonSchema for UniqueId {
    fn schema_name() -> Cow<'static, str> {
        "UniqueId".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "Stable id assigned by the protocol (e.g. a Zigbee IEEE address). Unique within the protocol.",
            "pattern": "^[^\\u0000-\\u001F\\u007F-\\u009F]+$",
            "minLength": 1,
            "maxLength": 255,
        })
    }
}

/// A human-readable name: 1–100 characters, no leading or trailing whitespace, no control
/// characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Name(String);

string_newtype!(Name, check_name);

/// Whitespace for name boundaries: Unicode `White_Space` plus U+FEFF (BOM). ECMA-262 `\s`
/// includes U+FEFF but Rust's doesn't, so both sides spell the set out instead of trusting `\s`.
fn is_name_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{FEFF}'
}

/// [`is_name_space`] as a regex class body, minus the control characters the pattern already
/// excludes (U+0009-U+000D, U+0085).
const NAME_SPACE_CLASS: &str =
    "\\u0020\\u00A0\\u1680\\u2000-\\u200A\\u2028\\u2029\\u202F\\u205F\\u3000\\uFEFF";

fn check_name(value: &str) -> Result<(), IdError> {
    check_text("name", value, 100, "must be at most 100 characters")
}

/// Shared by [`Name`] and [`Description`]: 1-`max` characters, trimmed, no control characters.
fn check_text(
    what: &'static str,
    value: &str,
    max: usize,
    too_long: &'static str,
) -> Result<(), IdError> {
    if value.is_empty() {
        return Err(err(what, value, "must not be empty"));
    }
    if value.chars().count() > max {
        return Err(err(what, value, too_long));
    }
    if value.starts_with(is_name_space) || value.ends_with(is_name_space) {
        return Err(err(what, value, "must not start or end with whitespace"));
    }
    if value.chars().any(char::is_control) {
        return Err(err(what, value, "must not contain control characters"));
    }
    Ok(())
}

/// The JSON Schema pattern for [`check_text`] (the length limit is a separate `maxLength`).
/// `forbidden` adds characters that may not appear anywhere, as a regex class body.
fn text_pattern(forbidden: &str) -> String {
    format!(
        "^[^{ws}{ctrl}]([^{ctrl}]*[^{ws}{ctrl}])?$",
        ws = NAME_SPACE_CLASS,
        ctrl = format!("\\u0000-\\u001F\\u007F-\\u009F{forbidden}"),
    )
}

impl JsonSchema for Name {
    fn schema_name() -> Cow<'static, str> {
        "Name".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "A human-readable name. 1-100 characters, no leading or trailing whitespace.",
            "pattern": text_pattern(""),
            "minLength": 1,
            "maxLength": 100,
        })
    }
}

/// A one-or-two sentence description, e.g. of an extension: 1–500 characters, no leading or
/// trailing whitespace, no control characters (so no line breaks).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Description(String);

string_newtype!(Description, check_description);

/// Unicode line and paragraph separators aren't control characters, but they break a line.
const LINE_SEPARATORS: &str = "\\u2028\\u2029";

fn check_description(value: &str) -> Result<(), IdError> {
    check_text("description", value, 500, "must be at most 500 characters")?;
    if value.contains(['\u{2028}', '\u{2029}']) {
        return Err(err(
            "description",
            value,
            "must be a single line (no line or paragraph separators)",
        ));
    }
    Ok(())
}

impl JsonSchema for Description {
    fn schema_name() -> Cow<'static, str> {
        "Description".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "A short description. 1-500 characters on one line, no leading or trailing whitespace.",
            "pattern": text_pattern(LINE_SEPARATORS),
            "minLength": 1,
            "maxLength": 500,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        for ok in [
            "hallway",
            "ground_floor",
            "a1",
            "0x00158d0001a2b3c4",
            "a_b_c",
        ] {
            assert!(AreaId::try_from(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "Hallway",
            "hall way",
            "_hall",
            "hall_",
            "hall__way",
            "hall-way",
            "é",
        ] {
            assert!(AreaId::try_from(bad).is_err(), "{bad:?}");
        }
        assert!(AreaId::try_from("a".repeat(64)).is_ok());
        assert!(AreaId::try_from("a".repeat(65)).is_err());
    }

    #[test]
    fn entity_ids() {
        let id = EntityId::try_from("binary_sensor.hallway_motion").expect("valid");
        assert_eq!(id.kind(), EntityKind::BinarySensor);
        assert_eq!(id.object_id(), "hallway_motion");
        assert_eq!(
            EntityId::new(EntityKind::Light, "hallway"),
            EntityId::try_from("light.hallway")
        );

        for bad in [
            "hallway",
            "fan.bedroom",
            "light.",
            "light.Hallway",
            "Light.hallway",
            "light.hall.way",
        ] {
            assert!(EntityId::try_from(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn error_messages_say_what_and_why() {
        let e = EntityId::try_from("fan.bedroom").expect_err("unknown kind");
        assert_eq!(
            e.to_string(),
            "invalid entity id \"fan.bedroom\": unknown kind before the `.` (expected one of: light, switch, sensor, binary_sensor)"
        );
    }

    #[test]
    fn ulids() {
        assert!(ContextId::try_from("01J8ZQ4Y7K3M2X5N6P8R9S0T1V").is_ok());
        for bad in [
            "01j8zq4y7k3m2x5n6p8r9s0t1v",
            "01J8ZQ4Y7K3M2X5N6P8R9S0T1",
            "81J8ZQ4Y7K3M2X5N6P8R9S0T1V",
            "01J8ZQ4Y7K3M2X5N6P8R9S0T1I",
        ] {
            assert!(ContextId::try_from(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn names() {
        assert!(Name::try_from("Hallway ceiling").is_ok());
        assert!(Name::try_from("Küche · Decke").is_ok());
        for bad in ["", " Hallway", "Hallway ", "Hall\nway"] {
            assert!(Name::try_from(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn serde_round_trip_and_rejection() {
        let id: EntityId = serde_json::from_str("\"light.hallway\"").expect("valid");
        assert_eq!(
            serde_json::to_string(&id).expect("serializes"),
            "\"light.hallway\""
        );
        let e = serde_json::from_str::<EntityId>("\"light.Hallway\"").expect_err("invalid");
        assert!(e.to_string().contains("invalid entity id"), "{e}");
    }

    /// The schema and Rust must agree at the length limit, for every kind.
    #[test]
    fn entity_id_schema_matches_rust_at_the_length_limit() {
        let schema = EntityId::json_schema(&mut SchemaGenerator::default());
        let validator = jsonschema::validator_for(schema.as_value()).expect("valid schema");
        for kind in EntityKind::ALL {
            for (len, expected) in [(64, true), (65, false)] {
                let id = format!("{}.{}", kind.domain(), "a".repeat(len));
                assert_eq!(
                    EntityId::try_from(id.as_str()).is_ok(),
                    expected,
                    "rust {id}"
                );
                assert_eq!(
                    validator.is_valid(&serde_json::Value::from(id.as_str())),
                    expected,
                    "schema {id}"
                );
            }
        }
    }

    /// Names at the whitespace boundary: Rust and the schema must agree on every character,
    /// including U+FEFF, which ECMA-262 `\s` treats as whitespace and Rust doesn't.
    #[test]
    fn name_whitespace_matches_the_schema() {
        let schema = Name::json_schema(&mut SchemaGenerator::default());
        let validator = jsonschema::validator_for(schema.as_value()).expect("valid schema");
        let spaces = [
            '\u{0020}', '\u{00A0}', '\u{1680}', '\u{2000}', '\u{200A}', '\u{2028}', '\u{2029}',
            '\u{202F}', '\u{205F}', '\u{3000}', '\u{FEFF}',
        ];
        for c in spaces {
            for name in [format!("{c}Hallway"), format!("Hallway{c}")] {
                assert!(
                    Name::try_from(name.as_str()).is_err(),
                    "rust accepted {name:?}"
                );
                assert!(
                    !validator.is_valid(&serde_json::Value::from(name.as_str())),
                    "schema accepted {name:?}"
                );
            }
            let inner = format!("Hall{c}way");
            assert!(
                Name::try_from(inner.as_str()).is_ok(),
                "rust rejected {inner:?}"
            );
            assert!(
                validator.is_valid(&serde_json::Value::from(inner.as_str())),
                "schema rejected {inner:?}"
            );
        }
        // Descriptions are one line: no line or paragraph separators, even inside.
        let description = Description::json_schema(&mut SchemaGenerator::default());
        let description = jsonschema::validator_for(description.as_value()).expect("valid schema");
        for (text, expected) in [
            ("Devices over ESPHome's native API.", true),
            ("First line\u{2028}second line", false),
            ("First paragraph\u{2029}second", false),
        ] {
            assert_eq!(
                Description::try_from(text).is_ok(),
                expected,
                "rust {text:?}"
            );
            assert_eq!(
                description.is_valid(&serde_json::Value::from(text)),
                expected,
                "schema {text:?}"
            );
        }
        // Not whitespace in either: zero-width space.
        assert!(Name::try_from("\u{200B}Hallway").is_ok());
        assert!(validator.is_valid(&serde_json::Value::from("\u{200B}Hallway")));
    }
}
