//! `vacuum`: a robot vacuum cleaner.
//!
//! Home Assistant's model: what it's doing (its *activity*), a fan speed where it has them, and
//! the handful of things it can be told. Which of those it can be told is its own to say.

use std::sync::LazyLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Field, Typed};
use crate::{InvariantError, Service, ServiceName};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VacuumCapabilities {
    /// Can be told to start cleaning.
    #[serde(default)]
    pub start: bool,
    /// Can be paused where it is.
    #[serde(default)]
    pub pause: bool,
    /// Can be stopped.
    #[serde(default)]
    pub stop: bool,
    /// Can be sent back to its dock.
    #[serde(default)]
    pub return_home: bool,
    /// Can make a noise so someone can find it.
    #[serde(default)]
    pub locate: bool,
    /// Can clean the spot it's on.
    #[serde(default)]
    pub clean_spot: bool,
    /// The fan speeds it has, by its own names (`quiet`, `turbo`). None for one with no choice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 32))]
    pub fan_speeds: Vec<String>,
}

/// What a vacuum is doing. Home Assistant's `VacuumActivity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VacuumActivity {
    Cleaning,
    Docked,
    /// Off its dock and doing nothing.
    Idle,
    Paused,
    /// On its way back to the dock.
    Returning,
    /// Stuck, or something is wrong with it.
    Error,
}

impl VacuumActivity {
    pub const ALL: [Self; 6] = [
        Self::Cleaning,
        Self::Docked,
        Self::Idle,
        Self::Paused,
        Self::Returning,
        Self::Error,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cleaning => "cleaning",
            Self::Docked => "docked",
            Self::Idle => "idle",
            Self::Paused => "paused",
            Self::Returning => "returning",
            Self::Error => "error",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        super::from_ha(text)
    }
}

/// Every text a vacuum's primary value can be, for rules to check against.
pub(crate) static ACTIVITIES: LazyLock<Vec<String>> = LazyLock::new(|| {
    VacuumActivity::ALL
        .iter()
        .map(|activity| activity.as_str().to_owned())
        .collect()
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VacuumState {
    pub activity: VacuumActivity,
    /// One of its `fan_speeds` (checked by the core).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fan_speed: Option<String>,
}

/// Data for `vacuum.set_fan_speed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VacuumFanSpeed {
    /// One of the vacuum's `fan_speeds`.
    pub fan_speed: String,
}

/// Whether a reported state is one this vacuum can be in.
pub(crate) fn fits(caps: &VacuumCapabilities, state: &VacuumState) -> Result<(), String> {
    match &state.fan_speed {
        Some(speed) if !caps.fan_speeds.contains(speed) => Err(if caps.fan_speeds.is_empty() {
            "it has no fan speeds to be at".to_owned()
        } else {
            format!(
                "its fan speed is one of {}, not {speed:?}",
                caps.fan_speeds.join(", ")
            )
        }),
        _ => Ok(()),
    }
}

/// What a vacuum has beside its activity.
pub(crate) fn fields(caps: &VacuumCapabilities) -> Vec<Field> {
    if caps.fan_speeds.is_empty() {
        Vec::new()
    } else {
        vec![Field::text("fan_speed")]
    }
}

pub(crate) fn field(state: &VacuumState, name: &str) -> Option<Typed> {
    match name {
        "fan_speed" => state.fan_speed.clone().map(Typed::Text),
        _ => None,
    }
}

pub(crate) fn primary(state: &VacuumState) -> Typed {
    Typed::Text(state.activity.as_str().to_owned())
}

/// Keeps the fan speed it said last.
pub(crate) fn with_primary(previous: Option<&VacuumState>, value: &Typed) -> Option<VacuumState> {
    let Typed::Text(text) = value else {
        return None;
    };
    Some(VacuumState {
        activity: VacuumActivity::parse(text)?,
        fan_speed: previous.and_then(|state| state.fan_speed.clone()),
    })
}

pub(crate) fn data_of(name: ServiceName) -> Data {
    match name {
        ServiceName::VacuumSetFanSpeed => Data::Required,
        _ => Data::None,
    }
}

pub(crate) fn service(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    Ok(match name {
        ServiceName::VacuumStart => Service::VacuumStart,
        ServiceName::VacuumPause => Service::VacuumPause,
        ServiceName::VacuumStop => Service::VacuumStop,
        ServiceName::VacuumReturnToBase => Service::VacuumReturnToBase,
        ServiceName::VacuumLocate => Service::VacuumLocate,
        ServiceName::VacuumCleanSpot => Service::VacuumCleanSpot,
        ServiceName::VacuumSetFanSpeed => Service::VacuumSetFanSpeed(super::parse(name, data)?),
        _ => return Err(super::not_mine(name)),
    })
}

/// What it will be doing once it has done as asked, where that's known: finding it, or a
/// change of speed, leaves it doing what it was.
pub(crate) fn asks_for(service: &Service) -> Option<Typed> {
    let activity = match service {
        Service::VacuumStart | Service::VacuumCleanSpot => VacuumActivity::Cleaning,
        Service::VacuumPause => VacuumActivity::Paused,
        Service::VacuumStop => VacuumActivity::Idle,
        Service::VacuumReturnToBase => VacuumActivity::Returning,
        _ => return None,
    };
    Some(Typed::Text(activity.as_str().to_owned()))
}

/// Whether a vacuum can be asked this. `Err` follows the entity's name.
pub(crate) fn supports_service(caps: &VacuumCapabilities, service: &Service) -> Result<(), String> {
    let (can, what) = match service {
        Service::VacuumStart => (caps.start, "be started from here"),
        Service::VacuumPause => (caps.pause, "be paused"),
        Service::VacuumStop => (caps.stop, "be stopped"),
        Service::VacuumReturnToBase => (caps.return_home, "be sent back to its dock"),
        Service::VacuumLocate => (caps.locate, "be made to say where it is"),
        Service::VacuumCleanSpot => (caps.clean_spot, "clean a spot"),
        Service::VacuumSetFanSpeed(data) => {
            return if caps.fan_speeds.contains(&data.fan_speed) {
                Ok(())
            } else if caps.fan_speeds.is_empty() {
                Err("has no fan speeds to choose from".to_owned())
            } else {
                Err(format!(
                    "has the fan speeds {}, not {:?}",
                    caps.fan_speeds.join(", "),
                    data.fan_speed
                ))
            };
        }
        _ => return Ok(()),
    };
    if can {
        Ok(())
    } else {
        Err(format!("can't {what}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn robot() -> VacuumCapabilities {
        VacuumCapabilities {
            start: true,
            pause: true,
            return_home: true,
            fan_speeds: vec!["quiet".into(), "turbo".into()],
            ..VacuumCapabilities::default()
        }
    }

    #[test]
    fn a_vacuum_is_asked_only_what_it_can_do() {
        let caps = robot();
        assert_eq!(supports_service(&caps, &Service::VacuumStart), Ok(()));
        assert_eq!(
            supports_service(&caps, &Service::VacuumLocate),
            Err("can't be made to say where it is".to_owned())
        );
        let speed = |speed: &str| {
            Service::VacuumSetFanSpeed(VacuumFanSpeed {
                fan_speed: speed.to_owned(),
            })
        };
        assert_eq!(supports_service(&caps, &speed("turbo")), Ok(()));
        assert_eq!(
            supports_service(&caps, &speed("max")),
            Err(r#"has the fan speeds quiet, turbo, not "max""#.to_owned())
        );
    }

    #[test]
    fn its_activity_is_its_value_and_its_fan_speed_is_kept() {
        let state = VacuumState {
            activity: VacuumActivity::Docked,
            fan_speed: Some("quiet".into()),
        };
        assert_eq!(primary(&state), Typed::Text("docked".into()));
        assert_eq!(
            field(&state, "fan_speed"),
            Some(Typed::Text("quiet".into()))
        );
        let cleaning = with_primary(Some(&state), &Typed::Text("cleaning".into())).expect("valid");
        assert_eq!(cleaning.fan_speed.as_deref(), Some("quiet"));
        assert_eq!(with_primary(None, &Typed::Text("flying".into())), None);
        assert!(fits(&robot(), &state).is_ok());
        let odd = VacuumState {
            fan_speed: Some("max".into()),
            ..state
        };
        assert!(fits(&robot(), &odd).is_err());
        assert_eq!(
            asks_for(&Service::VacuumReturnToBase),
            Some(Typed::Text("returning".into()))
        );
        assert_eq!(asks_for(&Service::VacuumLocate), None);
    }
}
