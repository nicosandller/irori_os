//! A timer: `timer.<id>`, counting down from a length of time, and `event.<id>`, which says
//! each time it's started, paused, cancelled or finished. "The lights stay on ten more
//! minutes", an egg timer. A flow starts it with `timer.start`, and another starts from its
//! event's `finished`.
//!
//! The helpers keep the clock: a timer's state says when it finishes, and when that comes it
//! finishes. A timer that ran out while Irori was down finishes as Irori starts, and says so,
//! so "turn the heater off when the timer ends" still happens after a reboot (Home Assistant's
//! `restore`). With `restore = false` it comes back idle instead, saying nothing.

use irori_protocol::ProtocolError;
use irori_protocol::types::{
    Capabilities, EventCapabilities, Name, ObjectId, Service, TimerCapabilities, TimerState,
    TimerStatus, Timestamp,
};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::Made;

/// `[timers.<id>]`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Timer {
    pub name: Name,
    /// How long it runs when started without being told, in seconds. Unless said, it has to be
    /// told each time.
    #[serde(default)]
    pub duration: f64,
    /// Keeps counting through a restart, and one that ran out meanwhile finishes as Irori
    /// starts. On unless said.
    #[serde(default = "yes")]
    pub restore: bool,
}

fn yes() -> bool {
    true
}

/// What its event says.
pub(crate) const STARTED: &str = "started";
pub(crate) const PAUSED: &str = "paused";
pub(crate) const CANCELLED: &str = "cancelled";
pub(crate) const FINISHED: &str = "finished";

pub(crate) fn made(id: &ObjectId, timer: &Timer) -> Result<Vec<Made>, ProtocolError> {
    if !(timer.duration.is_finite()
        && (0.0..=irori_protocol::types::timer::LONGEST).contains(&timer.duration))
    {
        return Err(ProtocolError::new(format!(
            "the timer `{id}`: its duration is seconds, from nought to a year, not {}",
            timer.duration
        )));
    }
    let event = Made::happens(
        "timerevent",
        id,
        &timer.name,
        Capabilities::Event(EventCapabilities {
            event_types: [STARTED, PAUSED, CANCELLED, FINISHED]
                .map(str::to_owned)
                .to_vec(),
            device_class: None,
        }),
    )?;
    let counter = Made::timer(
        id,
        &timer.name,
        Capabilities::Timer(TimerCapabilities {}),
        crate::Role::Timer {
            fires: event.unique.clone(),
            duration: timer.duration,
            restore: timer.restore,
        },
    )?;
    Ok(vec![counter, event])
}

fn plus(at: Timestamp, seconds: f64) -> Timestamp {
    jiff::SignedDuration::try_from_secs_f64(seconds)
        .ok()
        .and_then(|span| at.as_jiff().checked_add(span).ok())
        .map_or(at, Timestamp::from_jiff)
}

/// Seconds from `now` until `at`, nought once it's past.
fn until(at: Timestamp, now: Timestamp) -> f64 {
    at.as_jiff()
        .duration_since(now.as_jiff())
        .as_secs_f64()
        .max(0.0)
}

/// Whether it has run out by `now`.
pub(crate) fn due(state: &TimerState, now: Timestamp) -> bool {
    state.state == TimerStatus::Active && state.finishes_at.is_some_and(|at| at <= now)
}

/// The timer as it comes back when the helpers start: as it was kept, with what happened
/// meanwhile, and what its event says about that.
pub(crate) fn restored(
    kept: Option<TimerState>,
    duration: f64,
    restore: bool,
    now: Timestamp,
) -> (TimerState, Option<&'static str>) {
    let idle = TimerState::idle(duration);
    match kept {
        Some(kept) if restore && kept.state != TimerStatus::Idle => {
            let kept = TimerState { duration, ..kept };
            if due(&kept, now) {
                (idle, Some(FINISHED))
            } else {
                (kept, None)
            }
        }
        _ => (idle, None),
    }
}

/// What a call does to a timer at `now`: the state it's in afterwards, and what its event
/// says. `Err` says why it can't, after the timer's name.
pub(crate) fn apply(
    state: &TimerState,
    service: &Service,
    now: Timestamp,
) -> Result<(TimerState, Option<&'static str>), String> {
    let idle = TimerState::idle(state.duration);
    let running = |seconds: f64| TimerState {
        state: TimerStatus::Active,
        duration: state.duration,
        finishes_at: Some(plus(now, seconds)),
        remaining: None,
    };
    let left = || match state.state {
        TimerStatus::Active => state.finishes_at.map(|at| until(at, now)),
        TimerStatus::Paused => state.remaining,
        TimerStatus::Idle => None,
    };
    Ok(match service {
        Service::TimerStart(data) => {
            // Told how long, that; paused, what was left; otherwise its own length.
            let seconds = data
                .duration
                .or_else(|| (state.state == TimerStatus::Paused).then(left).flatten())
                .unwrap_or(state.duration);
            if seconds <= 0.0 {
                return Err("has no length of its own: say how long to run for".to_owned());
            }
            (running(seconds), Some(STARTED))
        }
        Service::TimerPause => match state.state {
            TimerStatus::Active => (
                TimerState {
                    state: TimerStatus::Paused,
                    duration: state.duration,
                    finishes_at: None,
                    remaining: left(),
                },
                Some(PAUSED),
            ),
            _ => return Err("isn't running, so there's nothing to pause".to_owned()),
        },
        Service::TimerCancel => match state.state {
            TimerStatus::Idle => (idle, None),
            _ => (idle, Some(CANCELLED)),
        },
        Service::TimerFinish => match state.state {
            TimerStatus::Idle => {
                return Err("isn't running, so there's nothing to finish".to_owned());
            }
            _ => (idle, Some(FINISHED)),
        },
        Service::TimerChange(data) => {
            let Some(left) = left() else {
                return Err("isn't running, so there's no time left to change".to_owned());
            };
            let left = left + data.by;
            match (state.state, left <= 0.0) {
                // Taken past its end: it's done.
                (_, true) => (idle, Some(FINISHED)),
                (TimerStatus::Paused, false) => (
                    TimerState {
                        remaining: Some(left),
                        ..state.clone()
                    },
                    None,
                ),
                _ => (running(left), None),
            }
        }
        service => return Err(format!("can't do `{}`", service.name())),
    })
}

#[cfg(test)]
mod tests {
    use irori_protocol::types::{TimerChange, TimerStart};

    use super::*;

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(1_790_000_000 + second).expect("an instant")
    }

    fn start(duration: Option<f64>) -> Service {
        Service::TimerStart(TimerStart { duration })
    }

    #[test]
    fn a_timer_counts_down_pauses_and_carries_on_where_it_left_off() {
        let idle = TimerState::idle(600.0);
        let (running, said) = apply(&idle, &start(None), at(0)).expect("starts");
        assert_eq!((running.finishes_at, said), (Some(at(600)), Some(STARTED)));
        assert!(!due(&running, at(599)) && due(&running, at(600)));

        let (paused, said) = apply(&running, &Service::TimerPause, at(200)).expect("pauses");
        assert_eq!(
            (paused.state, paused.remaining, said),
            (TimerStatus::Paused, Some(400.0), Some(PAUSED))
        );
        // Started again, it runs for what was left, not its whole length.
        let (again, _) = apply(&paused, &start(None), at(1000)).expect("starts again");
        assert_eq!(again.finishes_at, Some(at(1400)));
        // Told how long, that.
        let (told, _) = apply(&paused, &start(Some(60.0)), at(1000)).expect("starts");
        assert_eq!(told.finishes_at, Some(at(1060)));
    }

    #[test]
    fn a_timer_ends_by_being_cancelled_finished_or_run_past_its_end() {
        let (running, _) = apply(&TimerState::idle(600.0), &start(None), at(0)).expect("starts");
        let (cancelled, said) = apply(&running, &Service::TimerCancel, at(10)).expect("cancels");
        assert_eq!(
            (cancelled.state, said),
            (TimerStatus::Idle, Some(CANCELLED))
        );
        // Cancelling an idle one is nothing happening, not a refusal.
        assert_eq!(
            apply(&cancelled, &Service::TimerCancel, at(11)).expect("nothing"),
            (TimerState::idle(600.0), None)
        );
        let (_, said) = apply(&running, &Service::TimerFinish, at(10)).expect("finishes");
        assert_eq!(said, Some(FINISHED));

        let later = Service::TimerChange(TimerChange { by: 120.0 });
        let (longer, said) = apply(&running, &later, at(100)).expect("changes");
        assert_eq!((longer.finishes_at, said), (Some(at(720)), None));
        let sooner = Service::TimerChange(TimerChange { by: -1000.0 });
        let (ended, said) = apply(&running, &sooner, at(100)).expect("changes");
        assert_eq!((ended.state, said), (TimerStatus::Idle, Some(FINISHED)));

        // What makes no sense for an idle timer, or one with no length, is said.
        let idle = TimerState::idle(0.0);
        assert!(apply(&idle, &start(None), at(0)).is_err());
        assert!(apply(&idle, &Service::TimerPause, at(0)).is_err());
        assert!(apply(&idle, &later, at(0)).is_err());
    }

    #[test]
    fn a_timer_that_ran_out_while_irori_was_down_finishes_as_it_starts() {
        let (running, _) = apply(&TimerState::idle(600.0), &start(None), at(0)).expect("starts");
        // Still to come: it carries on.
        assert_eq!(
            restored(Some(running.clone()), 600.0, true, at(300)),
            (running.clone(), None)
        );
        // Run out meanwhile: it finishes now, and says so.
        assert_eq!(
            restored(Some(running.clone()), 600.0, true, at(900)),
            (TimerState::idle(600.0), Some(FINISHED))
        );
        // Not restored: back idle, saying nothing.
        assert_eq!(
            restored(Some(running), 600.0, false, at(900)),
            (TimerState::idle(600.0), None)
        );
        assert_eq!(
            restored(None, 600.0, true, at(0)),
            (TimerState::idle(600.0), None)
        );
    }
}
