//! `timer`: something that counts down and says when it's done. An egg timer, "the lights stay
//! on for ten more minutes", a wash cycle's time left.
//!
//! Home Assistant has timers only as helpers. Here it's a kind like any other, so a device
//! that counts down can be one too, and nothing that reads or starts one needs to know which.
//!
//! Times are seconds. A timer that's running says when it finishes rather than how long is
//! left, so its state doesn't change every second it counts.

use std::sync::LazyLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Field, Typed};
use crate::{InvariantError, Service, ServiceName, Timestamp};

/// The longest a timer runs: a year. Longer is a date, not a countdown.
pub const LONGEST: f64 = 366.0 * 24.0 * 3600.0;

/// What a timer can do. Nothing to say yet: every timer starts, pauses, and ends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimerCapabilities {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimerStatus {
    /// Not counting: never started, run out, or cancelled.
    Idle,
    /// Counting down.
    Active,
    /// Stopped part of the way, with time left.
    Paused,
}

impl TimerStatus {
    pub const ALL: [Self; 3] = [Self::Idle, Self::Active, Self::Paused];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Active => "active",
            Self::Paused => "paused",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        super::from_ha(text)
    }
}

/// Every text a timer's primary value can be, for rules to check against.
pub(crate) static STATUSES: LazyLock<Vec<String>> = LazyLock::new(|| {
    TimerStatus::ALL
        .iter()
        .map(|status| status.as_str().to_owned())
        .collect()
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimerState {
    pub state: TimerStatus,
    /// How long it runs when started without being told, in seconds.
    #[schemars(range(min = 0.0))]
    pub duration: f64,
    /// When it runs out. While it's active, and only then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finishes_at: Option<Timestamp>,
    /// Seconds left. While it's paused, and only then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0))]
    pub remaining: Option<f64>,
}

impl TimerState {
    /// A timer that isn't counting.
    pub fn idle(duration: f64) -> Self {
        Self {
            state: TimerStatus::Idle,
            duration,
            finishes_at: None,
            remaining: None,
        }
    }

    /// Deserialization runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        seconds("duration", self.duration)?;
        if let Some(remaining) = self.remaining {
            seconds("remaining", remaining)?;
        }
        let (finishes, remains) = (self.finishes_at.is_some(), self.remaining.is_some());
        match self.state {
            TimerStatus::Active if !finishes || remains => Err(InvariantError(
                "an active timer says when it finishes (`finishes_at`), and nothing else".into(),
            )),
            TimerStatus::Paused if !remains || finishes => Err(InvariantError(
                "a paused timer says how long is left (`remaining`), and nothing else".into(),
            )),
            TimerStatus::Idle if finishes || remains => Err(InvariantError(
                "an idle timer isn't counting: it has no `finishes_at` and no `remaining`".into(),
            )),
            _ => Ok(()),
        }
    }
}

fn seconds(what: &str, value: f64) -> Result<(), InvariantError> {
    if value.is_finite() && (0.0..=LONGEST).contains(&value) {
        Ok(())
    } else {
        Err(InvariantError(format!(
            "a timer's {what} is seconds, from nought to a year, not {value}"
        )))
    }
}

/// Data for `timer.start`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimerStart {
    /// How long to run for, in seconds. Left out, its own `duration`, or what was left when it
    /// was paused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("exclusiveMinimum" = 0))]
    pub duration: Option<f64>,
}

impl TimerStart {
    pub fn validate(&self) -> Result<(), InvariantError> {
        match self.duration {
            Some(duration) if !(duration.is_finite() && duration > 0.0 && duration <= LONGEST) => {
                Err(InvariantError(format!(
                    "`duration` is seconds to run for, above nought and at most a year, not {duration}"
                )))
            }
            _ => Ok(()),
        }
    }
}

/// Data for `timer.change`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimerChange {
    /// Seconds to add to a running timer, or to take away when negative.
    pub by: f64,
}

impl TimerChange {
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.by.is_finite() && self.by != 0.0 && self.by.abs() <= LONGEST {
            Ok(())
        } else {
            Err(InvariantError(format!(
                "`by` is seconds to add or take away, not nought and at most a year, not {}",
                self.by
            )))
        }
    }
}

/// What a timer has beside whether it's counting.
pub(crate) fn fields(_: &TimerCapabilities) -> Vec<Field> {
    vec![
        Field::number("duration"),
        Field::number("remaining"),
        Field::text("finishes_at"),
    ]
}

pub(crate) fn field(state: &TimerState, name: &str) -> Option<Typed> {
    match name {
        "duration" => Some(Typed::Number(state.duration)),
        "remaining" => state.remaining.map(Typed::Number),
        "finishes_at" => state.finishes_at.and_then(|at| {
            serde_json::to_value(at)
                .ok()
                .and_then(|value| value.as_str().map(|text| Typed::Text(text.to_owned())))
        }),
        _ => None,
    }
}

pub(crate) fn primary(state: &TimerState) -> Typed {
    Typed::Text(state.state.as_str().to_owned())
}

/// Only an idle timer can be made from its value alone: one that's counting needs to say
/// until when.
pub(crate) fn with_primary(previous: Option<&TimerState>, value: &Typed) -> Option<TimerState> {
    let Typed::Text(text) = value else {
        return None;
    };
    match (TimerStatus::parse(text)?, previous) {
        (TimerStatus::Idle, previous) => Some(TimerState::idle(
            previous.map_or(0.0, |state| state.duration),
        )),
        (status, Some(previous)) if previous.state == status => Some(previous.clone()),
        _ => None,
    }
}

pub(crate) fn data_of(name: ServiceName) -> Data {
    match name {
        ServiceName::TimerStart => Data::Optional,
        ServiceName::TimerChange => Data::Required,
        _ => Data::None,
    }
}

pub(crate) fn service(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    Ok(match name {
        ServiceName::TimerStart => Service::TimerStart(super::parse(name, data)?),
        ServiceName::TimerPause => Service::TimerPause,
        ServiceName::TimerCancel => Service::TimerCancel,
        ServiceName::TimerFinish => Service::TimerFinish,
        ServiceName::TimerChange => Service::TimerChange(super::parse(name, data)?),
        _ => return Err(super::not_mine(name)),
    })
}

/// What it will be once it has done as asked. Changing how long is left leaves it as it was.
pub(crate) fn asks_for(service: &Service) -> Option<Typed> {
    let status = match service {
        Service::TimerStart(_) => TimerStatus::Active,
        Service::TimerPause => TimerStatus::Paused,
        Service::TimerCancel | Service::TimerFinish => TimerStatus::Idle,
        _ => return None,
    };
    Some(Typed::Text(status.as_str().to_owned()))
}

/// Every timer does everything a timer does; what's asked only has to be well formed, which
/// its data already checked.
pub(crate) fn supports_service(_: &TimerCapabilities, _: &Service) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timer_says_only_what_fits_what_it_is_doing() {
        let at: Timestamp = "2026-10-14T06:45:00Z".parse().expect("an instant");
        let active = TimerState {
            state: TimerStatus::Active,
            duration: 600.0,
            finishes_at: Some(at),
            remaining: None,
        };
        assert!(active.validate().is_ok());
        assert_eq!(primary(&active), Typed::Text("active".into()));
        assert_eq!(
            field(&active, "finishes_at"),
            Some(Typed::Text("2026-10-14T06:45:00Z".into()))
        );
        assert_eq!(field(&active, "remaining"), None);
        let paused = TimerState {
            state: TimerStatus::Paused,
            finishes_at: None,
            remaining: Some(240.0),
            ..active.clone()
        };
        assert!(paused.validate().is_ok());
        assert_eq!(field(&paused, "remaining"), Some(Typed::Number(240.0)));
        assert!(TimerState::idle(600.0).validate().is_ok());

        // Counting with no end, paused with nothing left to say, idle and still counting.
        assert!(
            TimerState {
                finishes_at: None,
                ..active.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            TimerState {
                remaining: None,
                ..paused
            }
            .validate()
            .is_err()
        );
        assert!(
            TimerState {
                finishes_at: Some(at),
                ..TimerState::idle(600.0)
            }
            .validate()
            .is_err()
        );
        assert!(TimerState::idle(-1.0).validate().is_err());
    }

    #[test]
    fn what_a_timer_is_asked_has_to_be_a_length_of_time() {
        assert!(TimerStart::default().validate().is_ok());
        assert!(
            TimerStart {
                duration: Some(90.0)
            }
            .validate()
            .is_ok()
        );
        assert!(
            TimerStart {
                duration: Some(0.0)
            }
            .validate()
            .is_err()
        );
        assert!(TimerChange { by: -30.0 }.validate().is_ok());
        assert!(TimerChange { by: 0.0 }.validate().is_err());
        assert_eq!(
            asks_for(&Service::TimerStart(TimerStart::default())),
            Some(Typed::Text("active".into()))
        );
        assert_eq!(
            asks_for(&Service::TimerFinish),
            Some(Typed::Text("idle".into()))
        );
        assert_eq!(
            asks_for(&Service::TimerChange(TimerChange { by: 5.0 })),
            None
        );
        // Idle can be made from its value alone; counting can't.
        assert_eq!(
            with_primary(None, &Typed::Text("idle".into())),
            Some(TimerState::idle(0.0))
        );
        assert_eq!(with_primary(None, &Typed::Text("active".into())), None);
    }
}
