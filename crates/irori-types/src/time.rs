use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

/// An instant in time, serialized as an RFC 3339 string in UTC, e.g.
/// `2026-09-15T22:04:31.120Z`. Input with any offset is accepted and normalized to UTC.
///
/// Types never read the clock; producing timestamps is the job of the core's injected clock
/// (ROADMAP D10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(jiff::Timestamp);

impl Timestamp {
    pub fn from_jiff(ts: jiff::Timestamp) -> Self {
        Self(ts)
    }

    pub fn as_jiff(self) -> jiff::Timestamp {
        self.0
    }
}

impl std::str::FromStr for Timestamp {
    type Err = jiff::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl JsonSchema for Timestamp {
    fn schema_name() -> Cow<'static, str> {
        "Timestamp".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "format": "date-time",
            // `format` is only an annotation for most validators; the pattern enforces the offset.
            "pattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]+)?(Z|[+-][0-9]{2}:[0-9]{2})$",
            "description": "RFC 3339 timestamp with an offset, e.g. 2026-09-15T22:04:31.120Z. Serialized in UTC.",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_to_utc_and_requires_an_offset() {
        let ts: Timestamp =
            serde_json::from_str("\"2026-09-15T23:04:31.12+01:00\"").expect("valid");
        assert_eq!(
            serde_json::to_string(&ts).expect("serializes"),
            "\"2026-09-15T22:04:31.12Z\""
        );
        assert!(serde_json::from_str::<Timestamp>("\"2026-09-15T22:04:31\"").is_err());
    }
}
