//! A calendar: the event on now, or the next one.

use irori_types::{CalendarState, State};
use leptos::prelude::*;

use super::UNKNOWN;

/// `Recycling, on now`, `Next: Dentist, 2026-11-20 09:00`, or that nothing is coming.
pub(crate) fn calendar_words(calendar: &CalendarState) -> String {
    match (&calendar.event, calendar.on) {
        (Some(event), true) => format!("{}, on now", event.summary),
        (Some(event), false) => match event.start_time {
            Some(time) => format!(
                "Next: {}, {} {}",
                event.summary,
                event.start,
                time.to_string().get(..5).unwrap_or_default()
            ),
            None => format!("Next: {}, {}", event.summary, event.start),
        },
        (None, true) => "On now".to_owned(),
        (None, false) => "Nothing coming".to_owned(),
    }
}

pub(crate) fn calendar_reading(value: Option<&State>) -> AnyView {
    let (reading, on) = match value {
        Some(State::Calendar(calendar)) => (calendar_words(calendar), calendar.on),
        _ => (UNKNOWN.to_owned(), false),
    };
    view! { <span class="reading" class:on=on>{reading}</span> }.into_any()
}
