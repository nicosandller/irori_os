//! `calendar`: a calendar the home can read: a family's, the bin collection days, school
//! holidays. Home Assistant's `calendar`.
//!
//! Its value is whether an event is on now. It also says the event that is on, or the next one,
//! in the home's own time: a day, and a time of day unless it lasts all day. Reading a calendar's
//! other events, and adding to it, are not here yet.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::moment::{CivilDate, ClockTime};
use super::{Field, Typed};
use crate::InvariantError;

/// The longest summary, and the longest location or description.
pub const SUMMARY_MOST: usize = 255;
pub const DETAIL_MOST: usize = 4096;

/// What a calendar can do: say its events. Nothing to say yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CalendarCapabilities {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CalendarState {
    /// An event is on now.
    pub on: bool,
    /// The event that is on, or the next one. Absent when it has none coming.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<CalendarEvent>,
}

/// One event, in the home's own time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CalendarEvent {
    /// What it is.
    #[schemars(length(max = 255))]
    pub summary: String,
    /// The day it starts.
    pub start: CivilDate,
    /// When on that day, unless it lasts all day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<ClockTime>,
    /// The day it ends: for an all-day event, the day after its last.
    pub end: CivilDate,
    /// When on that day, unless it lasts all day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<ClockTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 4096))]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 4096))]
    pub description: Option<String>,
}

impl CalendarEvent {
    /// Whether it lasts all day: it has days and no times.
    pub fn all_day(&self) -> bool {
        self.start_time.is_none()
    }
}

impl CalendarState {
    /// Deserialization runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        let Some(event) = &self.event else {
            return Ok(());
        };
        if event.summary.chars().count() > SUMMARY_MOST {
            return Err(InvariantError(format!(
                "an event's summary is at most {SUMMARY_MOST} characters"
            )));
        }
        for detail in [&event.location, &event.description].into_iter().flatten() {
            if detail.chars().count() > DETAIL_MOST {
                return Err(InvariantError(format!(
                    "an event's location and description are at most {DETAIL_MOST} characters"
                )));
            }
        }
        if event.start_time.is_some() != event.end_time.is_some() {
            return Err(InvariantError(
                "an event has times at both ends, or lasts all day and has none".into(),
            ));
        }
        if (event.end, event.end_time) < (event.start, event.start_time) {
            return Err(InvariantError("an event ends after it starts".into()));
        }
        Ok(())
    }
}

pub(crate) fn primary(state: &CalendarState) -> Typed {
    Typed::Bool(state.on)
}

/// The event's summary, for rules: `text('calendar.bins', 'event') == 'Recycling'`.
pub(crate) fn fields() -> Vec<Field> {
    vec![Field::text("event")]
}

pub(crate) fn field(state: &CalendarState, name: &str) -> Option<Typed> {
    match name {
        "event" => state
            .event
            .as_ref()
            .map(|event| Typed::Text(event.summary.clone())),
        _ => None,
    }
}

/// Keeps the event it said last.
pub(crate) fn with_primary(
    previous: Option<&CalendarState>,
    value: &Typed,
) -> Option<CalendarState> {
    let Typed::Bool(on) = value else {
        return None;
    };
    Some(CalendarState {
        on: *on,
        event: previous.and_then(|previous| previous.event.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bins() -> CalendarEvent {
        CalendarEvent {
            summary: "Recycling".into(),
            start: CivilDate::new(2026, 11, 20).expect("a day"),
            start_time: None,
            end: CivilDate::new(2026, 11, 21).expect("a day"),
            end_time: None,
            location: None,
            description: None,
        }
    }

    #[test]
    fn whether_an_event_is_on_is_its_value_and_the_event_a_field() {
        let state = CalendarState {
            on: false,
            event: Some(bins()),
        };
        assert!(state.validate().is_ok());
        assert!(bins().all_day());
        assert_eq!(primary(&state), Typed::Bool(false));
        assert_eq!(
            field(&state, "event"),
            Some(Typed::Text("Recycling".into()))
        );
    }

    #[test]
    fn an_event_ends_after_it_starts_and_has_times_at_both_ends_or_neither() {
        let mut backwards = bins();
        backwards.end = CivilDate::new(2026, 11, 19).expect("a day");
        let state = |event| CalendarState {
            on: true,
            event: Some(event),
        };
        assert!(state(backwards).validate().is_err());
        let mut half = bins();
        half.start_time = ClockTime::new(9, 0, 0);
        assert!(state(half).validate().is_err());
    }
}
