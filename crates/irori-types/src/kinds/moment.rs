//! `date`, `time` and `datetime`: a day, a time of day, or a moment that someone sets. An alarm
//! clock's time, the day the bins go out, when a holiday ends.
//!
//! Three kinds, as in Home Assistant, sharing this module because they differ only in what
//! their value is. Each is written the ISO 8601 way, which also means text that sorts in time
//! order, so rules compare them as text: `text('time.wake_up') < '07:30:00'`.
//!
//! - a `date` is a calendar day with no time zone: `2026-10-14`
//! - a `time` is a time of day with no date and no zone, to the second: `06:45:00`
//! - a `datetime` is an instant, a [`Timestamp`]: `2026-10-14T06:45:00Z`

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use super::{Data, Typed};
use crate::{InvariantError, Service, ServiceName, Timestamp};

/// What a `date`, a `time` or a `datetime` can do: hold its value. Nothing to say yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MomentCapabilities {}

/// A calendar day, `2026-10-14`. No time zone: it's the day wherever the home is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CivilDate(jiff::civil::Date);

/// A time of day to the second, `06:45:00`. No date and no zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClockTime(jiff::civil::Time);

impl CivilDate {
    /// `None` for a day that doesn't exist (February 30).
    pub fn new(year: i16, month: i8, day: i8) -> Option<Self> {
        jiff::civil::Date::new(year, month, day).ok().map(Self)
    }

    pub fn as_jiff(self) -> jiff::civil::Date {
        self.0
    }
}

impl From<jiff::civil::Date> for CivilDate {
    fn from(date: jiff::civil::Date) -> Self {
        Self(date)
    }
}

impl ClockTime {
    /// `None` for a time that doesn't exist (25:00).
    pub fn new(hour: i8, minute: i8, second: i8) -> Option<Self> {
        jiff::civil::Time::new(hour, minute, second, 0)
            .ok()
            .map(Self)
    }

    pub fn as_jiff(self) -> jiff::civil::Time {
        self.0
    }
}

impl From<jiff::civil::Time> for ClockTime {
    /// To the second: what's finer is dropped.
    fn from(time: jiff::civil::Time) -> Self {
        Self(time.with().subsec_nanosecond(0).build().unwrap_or(time))
    }
}

impl fmt::Display for CivilDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}",
            self.0.year(),
            self.0.month(),
            self.0.day()
        )
    }
}

impl fmt::Display for ClockTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02}:{:02}:{:02}",
            self.0.hour(),
            self.0.minute(),
            self.0.second()
        )
    }
}

/// The digits of `text` in the shape `shape` gives, where `0` stands for a digit and anything
/// else for itself: `0000-00-00`. `None` when it isn't that shape.
fn digits<const N: usize>(text: &str, shape: &str) -> Option<[i16; N]> {
    if text.len() != shape.len() {
        return None;
    }
    let mut parts = [0_i16; N];
    let mut part = 0;
    for (found, wanted) in text.bytes().zip(shape.bytes()) {
        if wanted == b'0' {
            if !found.is_ascii_digit() {
                return None;
            }
            parts[part] = parts[part] * 10 + i16::from(found - b'0');
        } else if found == wanted {
            part += 1;
        } else {
            return None;
        }
    }
    Some(parts)
}

impl std::str::FromStr for CivilDate {
    type Err = InvariantError;

    fn from_str(text: &str) -> Result<Self, InvariantError> {
        digits::<3>(text, "0000-00-00")
            .and_then(|[year, month, day]| {
                Self::new(year, i8::try_from(month).ok()?, i8::try_from(day).ok()?)
            })
            .ok_or_else(|| {
                InvariantError(format!(
                    "{text:?} isn't a date: a day that exists, written like 2026-10-14"
                ))
            })
    }
}

impl std::str::FromStr for ClockTime {
    type Err = InvariantError;

    /// `06:45:00`, or `06:45` for the same thing.
    fn from_str(text: &str) -> Result<Self, InvariantError> {
        let parts = digits::<3>(text, "00:00:00")
            .or_else(|| digits::<2>(text, "00:00").map(|[hour, minute]| [hour, minute, 0]));
        parts
            .and_then(|[hour, minute, second]| {
                Self::new(
                    i8::try_from(hour).ok()?,
                    i8::try_from(minute).ok()?,
                    i8::try_from(second).ok()?,
                )
            })
            .ok_or_else(|| {
                InvariantError(format!(
                    "{text:?} isn't a time of day: written like 06:45:00, from 00:00:00 to 23:59:59"
                ))
            })
    }
}

macro_rules! written_as_text {
    ($type:ident, $name:literal, $pattern:literal, $example:literal) => {
        impl Serialize for $type {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $type {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = <Cow<'de, str>>::deserialize(deserializer)?;
                text.parse().map_err(serde::de::Error::custom)
            }
        }

        impl JsonSchema for $type {
            fn schema_name() -> Cow<'static, str> {
                $name.into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({ "type": "string", "pattern": $pattern, "examples": [$example] })
            }
        }
    };
}

written_as_text!(
    CivilDate,
    "CivilDate",
    "^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])$",
    "2026-10-14"
);
written_as_text!(
    ClockTime,
    "ClockTime",
    "^([01][0-9]|2[0-3]):[0-5][0-9](:[0-5][0-9])?$",
    "06:45:00"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DateState {
    pub value: CivilDate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimeState {
    pub value: ClockTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DatetimeState {
    pub value: Timestamp,
}

/// Data for `date.set_value`. Home Assistant's field name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DateSetValue {
    pub date: CivilDate,
}

/// Data for `time.set_value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimeSetValue {
    pub time: ClockTime,
}

/// Data for `datetime.set_value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DatetimeSetValue {
    pub datetime: Timestamp,
}

/// A moment as rules read it: its text.
fn text(value: impl fmt::Display) -> Typed {
    Typed::Text(value.to_string())
}

/// An instant as text, in UTC to the second where it has no finer part.
fn instant(value: Timestamp) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

pub(crate) fn date_primary(state: &DateState) -> Typed {
    text(state.value)
}

pub(crate) fn time_primary(state: &TimeState) -> Typed {
    text(state.value)
}

pub(crate) fn datetime_primary(state: &DatetimeState) -> Typed {
    Typed::Text(instant(state.value))
}

pub(crate) fn date_with_primary(value: &Typed) -> Option<DateState> {
    match value {
        Typed::Text(text) => text.parse().ok().map(|value| DateState { value }),
        _ => None,
    }
}

pub(crate) fn time_with_primary(value: &Typed) -> Option<TimeState> {
    match value {
        Typed::Text(text) => text.parse().ok().map(|value| TimeState { value }),
        _ => None,
    }
}

pub(crate) fn datetime_with_primary(value: &Typed) -> Option<DatetimeState> {
    match value {
        Typed::Text(text) => text.parse().ok().map(|value| DatetimeState { value }),
        _ => None,
    }
}

pub(crate) fn data_of(_: ServiceName) -> Data {
    Data::Required
}

pub(crate) fn service(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    Ok(match name {
        ServiceName::DateSetValue => Service::DateSetValue(super::parse(name, data)?),
        ServiceName::TimeSetValue => Service::TimeSetValue(super::parse(name, data)?),
        ServiceName::DatetimeSetValue => Service::DatetimeSetValue(super::parse(name, data)?),
        _ => return Err(super::not_mine(name)),
    })
}

pub(crate) fn asks_for(service: &Service) -> Option<Typed> {
    match service {
        Service::DateSetValue(data) => Some(text(data.date)),
        Service::TimeSetValue(data) => Some(text(data.time)),
        Service::DatetimeSetValue(data) => Some(Typed::Text(instant(data.datetime))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_date_and_a_time_are_written_the_iso_way_and_only_that_way() {
        let date: CivilDate = "2026-10-14".parse().expect("a date");
        assert_eq!(date.to_string(), "2026-10-14");
        assert_eq!(serde_json::to_value(date).expect("json"), "2026-10-14");
        for wrong in [
            "2026-02-30",
            "14/10/2026",
            "2026-1-4",
            "2026-10-14T00:00:00Z",
            "",
        ] {
            assert!(wrong.parse::<CivilDate>().is_err(), "{wrong}");
        }
        let error = "2026-02-30".parse::<CivilDate>().expect_err("no such day");
        assert!(error.to_string().contains("a day that exists"), "{error}");

        let time: ClockTime = "06:45:00".parse().expect("a time");
        assert_eq!(
            "06:45".parse::<ClockTime>(),
            Ok(time),
            "seconds can be left out"
        );
        assert_eq!(time.to_string(), "06:45:00");
        for wrong in ["24:00:00", "6:45", "06:45:60", "06:45:00.5", "0645"] {
            assert!(wrong.parse::<ClockTime>().is_err(), "{wrong}");
        }
        // In time order, as text.
        assert!("06:45:00" < "07:30:00" && "2026-10-14" < "2026-11-01");
    }

    #[test]
    fn rules_read_a_moment_as_text_and_set_one_from_it() {
        let wake = TimeState {
            value: "06:45".parse().expect("a time"),
        };
        assert_eq!(time_primary(&wake), Typed::Text("06:45:00".into()));
        assert_eq!(
            time_with_primary(&Typed::Text("06:45:00".into())),
            Some(wake)
        );
        assert_eq!(time_with_primary(&Typed::Text("soon".into())), None);
        assert_eq!(date_with_primary(&Typed::Number(1.0)), None);
        let ends = DatetimeState {
            value: "2026-10-14T07:45:00+01:00".parse().expect("an instant"),
        };
        assert_eq!(
            datetime_primary(&ends),
            Typed::Text("2026-10-14T06:45:00Z".into())
        );
    }
}
