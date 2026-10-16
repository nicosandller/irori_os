//! A date, a time of day, or a moment someone sets: `date.<id>`, `time.<id>`, `datetime.<id>`.
//! When the alarm clock goes off, the day the bins go out, when the holiday ends. A flow reads
//! it as text that sorts in time order: `text('time.wake_up') < '07:30:00'`.

use irori_protocol::ProtocolError;
use irori_protocol::types::{
    Capabilities, CivilDate, ClockTime, MomentCapabilities, Name, ObjectId, Timestamp, Typed,
};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::Made;

/// `[dates.<id>]`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Date {
    pub name: Name,
    /// What it is until someone sets it, `2026-10-14`. Unknown unless said.
    #[serde(default)]
    pub initial: Option<CivilDate>,
}

/// `[times.<id>]`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Time {
    pub name: Name,
    /// What it is until someone sets it, `06:45:00`. Unknown unless said.
    #[serde(default)]
    pub initial: Option<ClockTime>,
}

/// `[datetimes.<id>]`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Datetime {
    pub name: Name,
    /// What it is until someone sets it, `2026-10-14T06:45:00Z`. Unknown unless said.
    #[serde(default)]
    pub initial: Option<Timestamp>,
}

pub(crate) fn date(id: &ObjectId, date: &Date) -> Result<Vec<Made>, ProtocolError> {
    Ok(vec![Made::value(
        "date",
        id,
        &date.name,
        Capabilities::Date(MomentCapabilities {}),
        date.initial.map(|day| Typed::Text(day.to_string())),
    )?])
}

pub(crate) fn time(id: &ObjectId, time: &Time) -> Result<Vec<Made>, ProtocolError> {
    Ok(vec![Made::value(
        "time",
        id,
        &time.name,
        Capabilities::Time(MomentCapabilities {}),
        time.initial.map(|time| Typed::Text(time.to_string())),
    )?])
}

pub(crate) fn datetime(id: &ObjectId, moment: &Datetime) -> Result<Vec<Made>, ProtocolError> {
    let initial = moment.initial.and_then(|instant| {
        serde_json::to_value(instant)
            .ok()
            .and_then(|value| value.as_str().map(|text| Typed::Text(text.to_owned())))
    });
    Ok(vec![Made::value(
        "datetime",
        id,
        &moment.name,
        Capabilities::Datetime(MomentCapabilities {}),
        initial,
    )?])
}
