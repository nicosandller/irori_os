use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer};

/// A credential: never printed, never sent back once given. Its schema says `writeOnly`, which is
/// how the settings form knows to draw a password box and how the core knows to keep it in
/// `secrets.toml` (`docs/specs/extensions.md` §5).
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// For the request that needs it, and nowhere else.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

impl<'de> Deserialize<'de> for Secret {
    /// Surrounding spaces are taken off: a token pasted from an app often brings a newline.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(Secret(text.trim().to_owned()))
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
    fn a_secret_never_shows_up_in_debug_output() {
        let secret: Secret = serde_json::from_value(serde_json::json!(" hunter2\n")).expect("text");
        assert_eq!(format!("{secret:?}"), "Secret(…)");
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn its_schema_is_write_only() {
        let schema = schemars::schema_for!(Secret);
        assert_eq!(schema.get("writeOnly"), Some(&serde_json::json!(true)));
    }
}
