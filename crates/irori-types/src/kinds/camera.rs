//! `camera`: something that sees: a security camera, a doorbell's camera.
//!
//! Home Assistant's model, without its streams: whether it's off, idle or recording, and whether
//! it's watching for motion. Its picture isn't state. A page asks the protocol for one when it
//! wants it (`docs/specs/protocols.md` §5.2), so pictures never pass through history or flows.

use std::sync::LazyLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Field, Typed};
use crate::{InvariantError, Service, ServiceName};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CameraCapabilities {
    /// Can be turned off and on again: a privacy switch.
    #[serde(default)]
    pub on_off: bool,
    /// Watches for motion, and can be told to stop and start again.
    #[serde(default)]
    pub motion_detection: bool,
}

/// What a camera is doing. Home Assistant's camera states, with `off` for one turned off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CameraStatus {
    Off,
    Idle,
    Recording,
    /// Someone is watching it live.
    Streaming,
}

impl CameraStatus {
    pub const ALL: [Self; 4] = [Self::Off, Self::Idle, Self::Recording, Self::Streaming];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Idle => "idle",
            Self::Recording => "recording",
            Self::Streaming => "streaming",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        super::from_ha(text)
    }
}

/// Every text a camera's primary value can be, for rules to check against.
pub(crate) static CAMERA_STATES: LazyLock<Vec<String>> = LazyLock::new(|| {
    CameraStatus::ALL
        .iter()
        .map(|status| status.as_str().to_owned())
        .collect()
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CameraState {
    pub state: CameraStatus,
    /// Whether it's watching for motion, for one with `motion_detection`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_detection: Option<bool>,
}

/// Whether a reported state is one this camera can be in.
pub(crate) fn fits(caps: &CameraCapabilities, state: &CameraState) -> Result<(), String> {
    if state.motion_detection.is_some() && !caps.motion_detection {
        return Err("it doesn't watch for motion".to_owned());
    }
    if state.state == CameraStatus::Off && !caps.on_off {
        return Err("it can't be off".to_owned());
    }
    Ok(())
}

pub(crate) fn fields(caps: &CameraCapabilities) -> Vec<Field> {
    if caps.motion_detection {
        vec![Field::bool("motion_detection")]
    } else {
        Vec::new()
    }
}

pub(crate) fn field(state: &CameraState, name: &str) -> Option<Typed> {
    match name {
        "motion_detection" => state.motion_detection.map(Typed::Bool),
        _ => None,
    }
}

pub(crate) fn primary(state: &CameraState) -> Typed {
    Typed::Text(state.state.as_str().to_owned())
}

/// Keeps whether it was watching for motion.
pub(crate) fn with_primary(previous: Option<&CameraState>, value: &Typed) -> Option<CameraState> {
    let Typed::Text(text) = value else {
        return None;
    };
    Some(CameraState {
        state: CameraStatus::parse(text)?,
        motion_detection: previous.and_then(|state| state.motion_detection),
    })
}

/// On unless it's off.
pub(crate) fn toggle(current: Option<&Typed>) -> ServiceName {
    match current {
        Some(Typed::Text(text)) if text != CameraStatus::Off.as_str() => ServiceName::CameraTurnOff,
        _ => ServiceName::CameraTurnOn,
    }
}

pub(crate) fn data_of(_: ServiceName) -> Data {
    Data::None
}

pub(crate) fn service(
    name: ServiceName,
    _: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    Ok(match name {
        ServiceName::CameraTurnOn => Service::CameraTurnOn,
        ServiceName::CameraTurnOff => Service::CameraTurnOff,
        ServiceName::CameraEnableMotionDetection => Service::CameraEnableMotionDetection,
        ServiceName::CameraDisableMotionDetection => Service::CameraDisableMotionDetection,
        _ => return Err(super::not_mine(name)),
    })
}

/// Turned off, it's off. Turned on, it may be idle or already recording: not known until it
/// says.
pub(crate) fn asks_for(service: &Service) -> Option<Typed> {
    match service {
        Service::CameraTurnOff => Some(Typed::Text(CameraStatus::Off.as_str().to_owned())),
        _ => None,
    }
}

/// Whether a camera can be asked this. `Err` follows the entity's name.
pub(crate) fn supports_service(caps: &CameraCapabilities, service: &Service) -> Result<(), String> {
    let (can, what) = match service {
        Service::CameraTurnOn | Service::CameraTurnOff => (caps.on_off, "be turned on and off"),
        Service::CameraEnableMotionDetection | Service::CameraDisableMotionDetection => {
            (caps.motion_detection, "watch for motion")
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

    #[test]
    fn a_camera_is_asked_only_what_it_can_do_and_toggles_off_unless_off() {
        let fixed = CameraCapabilities {
            on_off: false,
            motion_detection: true,
        };
        assert_eq!(
            supports_service(&fixed, &Service::CameraTurnOff),
            Err("can't be turned on and off".to_owned())
        );
        assert_eq!(
            supports_service(&fixed, &Service::CameraEnableMotionDetection),
            Ok(())
        );
        let off = CameraState {
            state: CameraStatus::Off,
            motion_detection: None,
        };
        assert!(fits(&fixed, &off).is_err());
        assert_eq!(
            toggle(Some(&Typed::Text("recording".into()))),
            ServiceName::CameraTurnOff
        );
        assert_eq!(
            toggle(Some(&Typed::Text("off".into()))),
            ServiceName::CameraTurnOn
        );
        let watching = CameraState {
            state: CameraStatus::Idle,
            motion_detection: Some(true),
        };
        let recording = with_primary(Some(&watching), &Typed::Text("recording".into()));
        assert_eq!(
            recording,
            Some(CameraState {
                state: CameraStatus::Recording,
                motion_detection: Some(true),
            })
        );
    }
}
