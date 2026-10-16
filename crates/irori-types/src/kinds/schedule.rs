//! `schedule`: on during the times a week sets out, off between them. "Heating hours", "the
//! children's bedtime". Home Assistant has schedules only as helpers; here it's a kind, so a
//! thermostat's own programme could be one too.
//!
//! Its value is whether it's on now, and it says when that next changes, so a page can show it
//! and a flow can read it without working the week out again.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Field, Typed};
use crate::Timestamp;

/// What a schedule can do. Nothing to say yet: a schedule is read, not told.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleCapabilities {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleState {
    /// Within one of its times now.
    pub on: bool,
    /// When it next turns on or off. Absent for a schedule that never changes (an empty week).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_change: Option<Timestamp>,
}

pub(crate) fn fields(_: &ScheduleCapabilities) -> Vec<Field> {
    vec![Field::text("next_change")]
}

pub(crate) fn field(state: &ScheduleState, name: &str) -> Option<Typed> {
    match name {
        "next_change" => state.next_change.and_then(|at| {
            serde_json::to_value(at)
                .ok()
                .and_then(|value| value.as_str().map(|text| Typed::Text(text.to_owned())))
        }),
        _ => None,
    }
}

pub(crate) fn primary(state: &ScheduleState) -> Typed {
    Typed::Bool(state.on)
}

/// Keeps when it next changes as it said last.
pub(crate) fn with_primary(
    previous: Option<&ScheduleState>,
    value: &Typed,
) -> Option<ScheduleState> {
    let Typed::Bool(on) = value else {
        return None;
    };
    Some(ScheduleState {
        on: *on,
        next_change: previous.and_then(|state| state.next_change),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whether_it_is_on_is_its_value_and_when_that_changes_a_field() {
        let at: Timestamp = "2026-10-14T21:00:00Z".parse().expect("an instant");
        let state = ScheduleState {
            on: true,
            next_change: Some(at),
        };
        assert_eq!(primary(&state), Typed::Bool(true));
        assert_eq!(
            field(&state, "next_change"),
            Some(Typed::Text("2026-10-14T21:00:00Z".into()))
        );
        let off = with_primary(Some(&state), &Typed::Bool(false)).expect("valid");
        assert_eq!((off.on, off.next_change), (false, Some(at)));
    }
}
