use irori_types::{Entity, State, TimerStatus};
use leptos::prelude::*;

use super::{UNKNOWN, tuck};
use crate::devices::Controls;

/// Seconds as a person reads a countdown: `4:05`, `1:02:30`.
pub(crate) fn clock(seconds: f64) -> String {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let total = seconds.max(0.0).round() as u64;
    let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// A timer's state in a few words: idle, when it ends, or how long was left when it stopped.
pub(crate) fn timer_words(state: &irori_types::TimerState) -> String {
    match state.state {
        TimerStatus::Idle => "Idle".to_owned(),
        TimerStatus::Active => match state.finishes_at {
            Some(at) => format!("Ends {}", super::moment::when(at)),
            None => "Running".to_owned(),
        },
        TimerStatus::Paused => format!("Paused, {} left", clock(state.remaining.unwrap_or(0.0))),
    }
}

/// A schedule's state in a few words: on or off, and until when.
pub(crate) fn schedule_words(state: &irori_types::ScheduleState) -> String {
    let now = if state.on { "On" } else { "Off" };
    match state.next_change {
        Some(at) => format!("{now} until {}", super::moment::when(at)),
        None => now.to_owned(),
    }
}

/// A schedule's reading. Nothing can be asked of it.
pub(crate) fn schedule_reading(value: Option<&State>) -> AnyView {
    let state = match value {
        Some(State::Schedule(state)) => Some(*state),
        _ => None,
    };
    let reading = state.map_or_else(|| UNKNOWN.to_owned(), |state| schedule_words(&state));
    view! { <span class="reading" class:on=state.is_some_and(|s| s.on)>{reading}</span> }.into_any()
}

/// A timer's row. Drawn once and kept, so start, pause and cancel give way to each other as it
/// runs.
pub(crate) fn timer_control(
    entity: &Entity,
    state: Signal<Option<State>>,
    offline: Signal<bool>,
    controls: Controls,
) -> AnyView {
    let timer = Memo::new(move |_| match state.get() {
        Some(State::Timer(timer)) => Some(timer),
        _ => None,
    });
    let status = Memo::new(move |_| timer.get().map(|timer| timer.state));
    let entity_id = entity.id.clone();
    let disable = {
        let entity_id = entity_id.clone();
        Signal::derive(move || {
            offline.get() || controls.busy.with(|busy| busy.contains(&entity_id))
        })
    };
    let button = |label: &'static str, action: &'static str, shown: Signal<bool>| {
        let entity_id = entity_id.clone();
        let button = view! {
            <button
                type="button"
                class="press"
                disabled=move || disable.get()
                on:click=move |_| controls.act.run((entity_id.clone(), action, None))
            >
                {label}
            </button>
        }
        .into_any();
        tuck(button, shown)
    };
    view! {
        <>
            <span class="reading" class:on=move || status.get() == Some(TimerStatus::Active)>
                {move || timer.get().map_or_else(|| UNKNOWN.to_owned(), |timer| timer_words(&timer))}
            </span>
            <span class="cover-buttons">
                {button(
                    "Start",
                    "start",
                    Signal::derive(move || status.get() != Some(TimerStatus::Active)),
                )}
                {button(
                    "Pause",
                    "pause",
                    Signal::derive(move || status.get() == Some(TimerStatus::Active)),
                )}
                {button(
                    "Cancel",
                    "cancel",
                    Signal::derive(move || {
                        matches!(status.get(), Some(TimerStatus::Active | TimerStatus::Paused))
                    }),
                )}
            </span>
        </>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::clock;

    #[test]
    fn a_countdown_reads_the_way_a_kitchen_timer_does() {
        assert_eq!(clock(245.0), "4:05");
        assert_eq!(clock(3750.4), "1:02:30");
        assert_eq!(clock(-3.0), "0:00");
    }
}
