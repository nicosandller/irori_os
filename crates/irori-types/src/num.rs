//! Whole-number fields, checked by name.
//!
//! JSON Schema's `integer` is mathematical: `3`, `3.0`, and `3e0` are all the integer 3, and a
//! schema can't tell them apart, so Rust accepts all three too. Numbers are read as written
//! ([`Num`]) and then checked by [`whole`], so every error names the field and its range, e.g.
//! `rgb[0] 256 is out of range; it must be from 0 to 255`.

use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};

use crate::InvariantError;

/// A JSON number as written, before any field-specific check.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Num {
    Int(i64),
    UInt(u64),
    Float(f64),
}

impl fmt::Display for Num {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(v) => v.fmt(f),
            Self::UInt(v) => v.fmt(f),
            Self::Float(v) => v.fmt(f),
        }
    }
}

impl<'de> Deserialize<'de> for Num {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NumVisitor;
        impl Visitor<'_> for NumVisitor {
            type Value = Num;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a number")
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Num, E> {
                Ok(Num::Int(v))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Num, E> {
                Ok(Num::UInt(v))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Num, E> {
                Ok(Num::Float(v))
            }
        }
        deserializer.deserialize_any(NumVisitor)
    }
}

/// Checks that `n` is a whole number from `min` to `max`, then narrows it to `T`.
pub(crate) fn whole<T: TryFrom<i64>>(
    field: &str,
    n: Num,
    min: i64,
    max: i64,
) -> Result<T, InvariantError> {
    let range = || format!("it must be from {min} to {max}");
    let value: i128 = match n {
        Num::Int(v) => v.into(),
        Num::UInt(v) => v.into(),
        Num::Float(v) if v.is_finite() && v.fract() == 0.0 => {
            // Anything this large is out of range anyway; clamp before converting.
            v.clamp(-1e30, 1e30) as i128
        }
        Num::Float(v) => {
            return Err(InvariantError(format!(
                "{field} {v} is not a whole number; {}",
                range()
            )));
        }
    };
    if (i128::from(min)..=i128::from(max)).contains(&value)
        && let Ok(narrowed) = i64::try_from(value).map(T::try_from)
        && let Ok(narrowed) = narrowed
    {
        return Ok(narrowed);
    }
    Err(InvariantError(format!(
        "{field} {n} is out of range; {}",
        range()
    )))
}

/// `#[serde(deserialize_with = ...)]` helpers can't take arguments, so fields that aren't
/// already inside a hand-written `Deserialize` use one of these.
pub(crate) fn level<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i8, D::Error> {
    whole("level", Num::deserialize(deserializer)?, -128, 127).map_err(de::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(json: &str, min: i64, max: i64) -> Result<i64, String> {
        let n: Num = serde_json::from_str(json).map_err(|e| e.to_string())?;
        whole::<i64>("x", n, min, max).map_err(|e| e.to_string())
    }

    #[test]
    fn whole_numbers_in_any_spelling() {
        assert_eq!(check("0", -128, 127), Ok(0));
        assert_eq!(check("0.0", -128, 127), Ok(0));
        assert_eq!(check("-1.0", -128, 127), Ok(-1));
        assert_eq!(check("1e2", 0, 255), Ok(100));
    }

    #[test]
    fn errors_name_the_field_and_range() {
        assert_eq!(
            check("0.5", -128, 127),
            Err("x 0.5 is not a whole number; it must be from -128 to 127".into())
        );
        assert_eq!(
            check("256", 0, 255),
            Err("x 256 is out of range; it must be from 0 to 255".into())
        );
        assert_eq!(
            check("18446744073709551615", 0, 255),
            Err("x 18446744073709551615 is out of range; it must be from 0 to 255".into())
        );
        assert!(check("1e300", 0, 255).is_err_and(|e| e.contains("is out of range")));
        assert!(check("\"1\"", 0, 255).is_err());
    }
}
