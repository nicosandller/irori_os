//! Integers the way JSON Schema sees them.
//!
//! JSON Schema's `integer` is mathematical: `3`, `3.0`, and `3e0` are all the integer 3, and a
//! schema can't tell them apart. Serde's integer types reject `3.0`. To keep Rust and the
//! schemas accepting the same documents, integer fields read through [`Int`], which accepts any
//! whole number and rejects fractions.

use std::fmt;
use std::marker::PhantomData;

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};

/// A whole number read from JSON, spelled as an integer or as a float with no fractional part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(crate) struct Int<T>(pub T);

impl<'de, T> Deserialize<'de> for Int<T>
where
    T: TryFrom<i64> + TryFrom<u64>,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(IntVisitor(PhantomData))
    }
}

struct IntVisitor<T>(PhantomData<T>);

impl<T> IntVisitor<T>
where
    T: TryFrom<i64> + TryFrom<u64>,
{
    fn out_of_range<E: de::Error>(value: impl fmt::Display) -> E {
        E::custom(format!(
            "integer {value} is out of range for {}",
            std::any::type_name::<T>()
        ))
    }
}

impl<T> Visitor<'_> for IntVisitor<T>
where
    T: TryFrom<i64> + TryFrom<u64>,
{
    type Value = Int<T>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a whole number")
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        T::try_from(v).map(Int).map_err(|_| Self::out_of_range(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        T::try_from(v).map(Int).map_err(|_| Self::out_of_range(v))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
        if v.fract() != 0.0 || !v.is_finite() {
            return Err(E::custom(format!("expected a whole number, got {v}")));
        }
        // Whole floats beyond ±2^63 can't be represented as i64; treat them as out of range.
        if v >= -(2f64.powi(63)) && v < 2f64.powi(63) {
            return self.visit_i64(v as i64);
        }
        Err(Self::out_of_range(v))
    }
}

/// `#[serde(deserialize_with = "crate::int::de")]` for a plain integer field.
pub(crate) fn de<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: TryFrom<i64> + TryFrom<u64>,
{
    Int::<T>::deserialize(deserializer).map(|Int(v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse<T: TryFrom<i64> + TryFrom<u64>>(json: &str) -> Result<T, String> {
        serde_json::from_str::<Int<T>>(json)
            .map(|Int(v)| v)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn whole_numbers_in_any_spelling() {
        assert_eq!(parse::<i8>("0"), Ok(0));
        assert_eq!(parse::<i8>("0.0"), Ok(0));
        assert_eq!(parse::<i8>("-1.0"), Ok(-1));
        assert_eq!(parse::<u8>("1e2"), Ok(100));
        assert_eq!(parse::<i64>("20000.0"), Ok(20000));
    }

    #[test]
    fn fractions_and_overflow_are_rejected() {
        assert!(parse::<i8>("1.5").is_err_and(|e| e.contains("expected a whole number, got 1.5")));
        assert!(
            parse::<i8>("200").is_err_and(|e| e.contains("integer 200 is out of range for i8"))
        );
        assert!(parse::<u8>("-1").is_err_and(|e| e.contains("out of range for u8")));
        assert!(parse::<i64>("1e300").is_err_and(|e| e.contains("out of range")));
        assert!(parse::<i8>("\"1\"").is_err());
    }
}
