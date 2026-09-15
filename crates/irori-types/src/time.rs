use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

/// An instant in time, serialized as an RFC 3339 string in UTC, e.g.
/// `2026-09-15T22:04:31.120Z`. Input with any offset is accepted and normalized to UTC.
///
/// Parsing is strict so Rust and the JSON Schema accept exactly the same strings: uppercase `T`
/// and `Z`, up to 9 fractional digits, and an offset from `-23:59` to `+23:59`. Leap seconds
/// (`:60`) are rejected: they can't be represented, and accepting them would change the instant.
///
/// Types never read the clock; producing timestamps is the job of the core's injected clock
/// (ROADMAP D10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Timestamp(jiff::Timestamp);

/// The same shape [`check_shape`] enforces, for JSON Schema validators. Calendar validity
/// (e.g. February 30) can't be expressed as a pattern; only Rust checks it.
const PATTERN: &str = "^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])T([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9](\\.[0-9]{1,9})?(Z|[+-]([01][0-9]|2[0-3]):[0-5][0-9])$";

impl Timestamp {
    pub fn from_jiff(ts: jiff::Timestamp) -> Self {
        Self(ts)
    }

    pub fn as_jiff(self) -> jiff::Timestamp {
        self.0
    }
}

/// Why a string isn't a valid timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimestampError(String);

impl fmt::Display for TimestampError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TimestampError {}

impl std::str::FromStr for Timestamp {
    type Err = TimestampError;
    fn from_str(s: &str) -> Result<Self, TimestampError> {
        if !check_shape(s) {
            return Err(TimestampError(format!(
                "invalid timestamp {s:?}: must be RFC 3339 with an offset, like \
                 2026-09-15T22:04:31Z or 2026-09-15T23:04:31.5+01:00 (uppercase T and Z, \
                 seconds 00-59, at most 9 fractional digits, offset -23:59 to +23:59)"
            )));
        }
        s.parse()
            .map(Self)
            .map_err(|e| TimestampError(format!("invalid timestamp {s:?}: {e}")))
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = <Cow<'de, str>>::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Matches [`PATTERN`], without a regex dependency.
fn check_shape(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = |range: std::ops::Range<usize>| {
        b.get(range.clone())
            .is_some_and(|d| d.len() == range.len() && d.iter().all(u8::is_ascii_digit))
    };
    let two = |at: usize| u32::from(b[at] - b'0') * 10 + u32::from(b[at + 1] - b'0');

    // YYYY-MM-DDTHH:MM:SS
    if !(digits(0..4) && b.get(4) == Some(&b'-') && digits(5..7) && b.get(7) == Some(&b'-'))
        || !(digits(8..10) && b.get(10) == Some(&b'T') && digits(11..13))
        || !(b.get(13) == Some(&b':')
            && digits(14..16)
            && b.get(16) == Some(&b':')
            && digits(17..19))
    {
        return false;
    }
    let (month, day, hour, minute, second) = (two(5), two(8), two(11), two(14), two(17));
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return false;
    }

    // Optional fraction: 1-9 digits.
    let mut i = 19;
    if b.get(i) == Some(&b'.') {
        let start = i + 1;
        let end = b[start..]
            .iter()
            .position(|c| !c.is_ascii_digit())
            .map_or(b.len(), |p| start + p);
        if !(1..=9).contains(&(end - start)) {
            return false;
        }
        i = end;
    }

    // Offset: Z or ±HH:MM.
    match b.get(i) {
        Some(b'Z') => i + 1 == b.len(),
        Some(b'+' | b'-') => {
            i + 6 == b.len()
                && digits(i + 1..i + 3)
                && b[i + 3] == b':'
                && digits(i + 4..i + 6)
                && two(i + 1) <= 23
                && two(i + 4) <= 59
        }
        _ => false,
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
            // `format` is only an annotation for most validators; the pattern does the checking.
            "pattern": PATTERN,
            "description": "RFC 3339 timestamp with an offset, e.g. 2026-09-15T22:04:31.120Z. Uppercase T and Z, seconds 00-59 (no leap seconds), at most 9 fractional digits, offset -23:59 to +23:59. Serialized in UTC.",
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

    /// Rust and the schema pattern must agree on every case.
    #[test]
    fn shape_check_matches_the_schema_pattern() {
        let cases = [
            ("2026-09-15T22:04:31Z", true),
            ("2026-09-15T22:04:31.123456789Z", true),
            ("2026-09-15T22:04:31-23:59", true),
            // jiff has no leap seconds and would silently turn :60 into :59.
            ("2026-09-15T22:04:60Z", false),
            ("2026-09-15T22:04:31", false),
            ("2026-09-15T22:04:31+24:00", false),
            ("2026-09-15T22:04:31+99:99", false),
            ("2026-09-15T22:04:31.1234567891Z", false),
            ("2026-09-15T22:04:31.Z", false),
            ("2026-09-15t22:04:31z", false),
            ("2026-09-15 22:04:31Z", false),
            ("2026-13-15T22:04:31Z", false),
            ("2026-09-00T22:04:31Z", false),
            ("2026-09-15T24:00:00Z", false),
            ("2026-09-15T22:04:31Z ", false),
            ("", false),
        ];
        let schema = Timestamp::json_schema(&mut SchemaGenerator::default());
        let validator = jsonschema::validator_for(schema.as_value()).expect("valid schema");
        for (s, expected) in cases {
            assert_eq!(check_shape(s), expected, "check_shape({s:?})");
            assert_eq!(
                validator.is_valid(&serde_json::Value::from(s)),
                expected,
                "schema pattern on {s:?}"
            );
        }
    }

    #[test]
    fn calendar_errors_come_from_jiff_with_a_clear_prefix() {
        let e = "2026-02-30T22:04:31Z"
            .parse::<Timestamp>()
            .expect_err("no Feb 30");
        assert!(
            e.to_string()
                .starts_with("invalid timestamp \"2026-02-30T22:04:31Z\": "),
            "{e}"
        );
    }
}
