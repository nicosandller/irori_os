//! `camera`: something that sees: a security camera, a doorbell's camera.
//!
//! Home Assistant's model: whether it's off, idle, recording or being watched, and whether it's
//! watching for motion. Its picture and its live video aren't state. A page asks the protocol for
//! them when it wants them (`docs/specs/protocols.md` §5.2, §5.3), so they never pass through
//! history or flows. One that can turn (pan, tilt, zoom) is told to move for a moment at a time,
//! and can be sent to positions saved on it (presets).

use std::sync::LazyLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Field, Typed};
use crate::{InvariantError, Service, ServiceName};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
///
/// What came with format 27 (`live`, `qualities`, `pan_tilt`, `zoom`, `presets`, `talk`) is left out
/// when it's off, so a camera that has none of it reads the same to a core from before.
pub struct CameraCapabilities {
    /// Can be turned off and on again: a privacy switch.
    #[serde(default)]
    pub on_off: bool,
    /// Watches for motion, and can be told to stop and start again.
    #[serde(default)]
    pub motion_detection: bool,
    /// Can be watched live (`docs/specs/protocols.md` §5.3).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub live: bool,
    /// The qualities it can be watched in, for one that's `live`: at least one, each once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 2))]
    pub qualities: Vec<LiveQuality>,
    /// Can be turned left and right, and up and down.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pan_tilt: bool,
    /// Can zoom in and out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub zoom: bool,
    /// Has positions saved on it that it can be sent to. Which ones is state (`presets`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub presets: bool,
    /// Has a speaker a page can talk through (`docs/specs/protocols.md` §5.4).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub talk: bool,
}

impl CameraCapabilities {
    /// Deserialization of an entity runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.live && self.qualities.is_empty() {
            return Err(InvariantError(
                "a live camera says which qualities it can be watched in: at least one".into(),
            ));
        }
        if !self.live && !self.qualities.is_empty() {
            return Err(InvariantError(
                "qualities are for a camera that can be watched live".into(),
            ));
        }
        if self.qualities.len() == 2 && self.qualities[0] == self.qualities[1] {
            return Err(InvariantError(format!(
                "the quality `{}` is listed twice",
                self.qualities[0].as_str()
            )));
        }
        Ok(())
    }

    /// Whether it can be told to move at all.
    pub fn moves(&self) -> bool {
        self.pan_tilt || self.zoom
    }
}

/// How sharp live video is. A camera usually sends the same picture twice: a small stream that's
/// cheap to watch, and its full size.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LiveQuality {
    /// Its small stream: quick to start, light on the network. Home Assistant's "fluent".
    Low,
    /// Its full size. Home Assistant's "clear".
    High,
}

impl LiveQuality {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::High => "high",
        }
    }
}

/// A position saved on a camera, which it can be sent back to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CameraPreset {
    /// What the camera calls it, for `camera.go_to_preset`.
    #[schemars(length(min = 1, max = 255))]
    pub id: String,
    /// What a person called it.
    #[schemars(length(min = 1, max = 255))]
    pub name: String,
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
    /// The positions saved on it, for one with `presets`. In the camera's own order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 256))]
    pub presets: Option<Vec<CameraPreset>>,
}

impl CameraState {
    /// Deserialization of a state runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        let Some(presets) = &self.presets else {
            return Ok(());
        };
        if presets.len() > 256 {
            return Err(InvariantError(format!(
                "{} presets is too many; at most 256",
                presets.len()
            )));
        }
        let mut seen = std::collections::BTreeSet::new();
        for preset in presets {
            for (what, text) in [("id", &preset.id), ("name", &preset.name)] {
                if text.trim().is_empty() || text.len() > 255 {
                    return Err(InvariantError(format!(
                        "a preset's {what} is from 1 to 255 bytes, not {text:?}"
                    )));
                }
            }
            if !seen.insert(preset.id.as_str()) {
                return Err(InvariantError(format!(
                    "the preset `{}` is listed twice",
                    preset.id
                )));
            }
        }
        Ok(())
    }
}

/// Data for `camera.move`: how fast to turn each way, from -1 (left, down, out) to 1 (right, up,
/// in). It moves for `seconds` and then stops on its own, so a lost `camera.stop` never leaves it
/// turning.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CameraMove {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = -1.0, max = 1.0))]
    pub pan: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = -1.0, max = 1.0))]
    pub tilt: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = -1.0, max = 1.0))]
    pub zoom: Option<f64>,
    /// How long to keep moving: 0.5 seconds unless told.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.1, max = 10.0))]
    pub seconds: Option<f64>,
}

impl CameraMove {
    /// How long a move lasts when it doesn't say.
    pub const SECONDS: f64 = 0.5;

    /// Deserialization of a call runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.pan.is_none() && self.tilt.is_none() && self.zoom.is_none() {
            return Err(InvariantError(
                "a move says which way: `pan`, `tilt` or `zoom`".into(),
            ));
        }
        for (what, speed) in [("pan", self.pan), ("tilt", self.tilt), ("zoom", self.zoom)] {
            if let Some(speed) = speed
                && !(-1.0..=1.0).contains(&speed)
            {
                return Err(InvariantError(format!(
                    "{what} {speed} is out of range; it must be from -1 to 1"
                )));
            }
        }
        if let Some(seconds) = self.seconds
            && !(0.1..=10.0).contains(&seconds)
        {
            return Err(InvariantError(format!(
                "seconds {seconds} is out of range; it must be from 0.1 to 10"
            )));
        }
        Ok(())
    }

    /// How long it moves for.
    pub fn duration(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f64(self.seconds.unwrap_or(Self::SECONDS))
    }
}

/// Data for `camera.go_to_preset`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CameraGoToPreset {
    /// One of its `presets`' ids.
    #[schemars(length(min = 1, max = 255))]
    pub preset: String,
}

/// Whether a reported state is one this camera can be in.
pub(crate) fn fits(caps: &CameraCapabilities, state: &CameraState) -> Result<(), String> {
    if state.motion_detection.is_some() && !caps.motion_detection {
        return Err("it doesn't watch for motion".to_owned());
    }
    if state.presets.is_some() && !caps.presets {
        return Err("it has no presets".to_owned());
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
        presets: previous.and_then(|state| state.presets.clone()),
    })
}

/// On unless it's off.
pub(crate) fn toggle(current: Option<&Typed>) -> ServiceName {
    match current {
        Some(Typed::Text(text)) if text != CameraStatus::Off.as_str() => ServiceName::CameraTurnOff,
        _ => ServiceName::CameraTurnOn,
    }
}

pub(crate) fn data_of(name: ServiceName) -> Data {
    match name {
        ServiceName::CameraMove | ServiceName::CameraGoToPreset => Data::Required,
        _ => Data::None,
    }
}

pub(crate) fn service(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    Ok(match name {
        ServiceName::CameraTurnOn => Service::CameraTurnOn,
        ServiceName::CameraTurnOff => Service::CameraTurnOff,
        ServiceName::CameraEnableMotionDetection => Service::CameraEnableMotionDetection,
        ServiceName::CameraDisableMotionDetection => Service::CameraDisableMotionDetection,
        ServiceName::CameraMove => Service::CameraMove(super::parse(name, data)?),
        ServiceName::CameraStop => Service::CameraStop,
        ServiceName::CameraGoToPreset => Service::CameraGoToPreset(super::parse(name, data)?),
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
        Service::CameraMove(data) => {
            if (data.pan.is_some() || data.tilt.is_some()) && !caps.pan_tilt {
                (false, "turn")
            } else {
                (caps.zoom || data.zoom.is_none(), "zoom")
            }
        }
        Service::CameraStop => (caps.moves(), "move"),
        Service::CameraGoToPreset(_) => (caps.presets, "go to a preset"),
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
            ..CameraCapabilities::default()
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
            presets: None,
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
            presets: None,
        };
        let recording = with_primary(Some(&watching), &Typed::Text("recording".into()));
        assert_eq!(
            recording,
            Some(CameraState {
                state: CameraStatus::Recording,
                motion_detection: Some(true),
                presets: None,
            })
        );
    }

    #[test]
    fn a_live_camera_says_how_it_can_be_watched() {
        let live = CameraCapabilities {
            live: true,
            ..CameraCapabilities::default()
        };
        assert!(live.validate().is_err(), "live, but in no quality");
        let both = CameraCapabilities {
            live: true,
            qualities: vec![LiveQuality::Low, LiveQuality::High],
            ..CameraCapabilities::default()
        };
        assert!(both.validate().is_ok());
        let twice = CameraCapabilities {
            qualities: vec![LiveQuality::High, LiveQuality::High],
            ..both.clone()
        };
        assert!(twice.validate().is_err());
        let still = CameraCapabilities {
            qualities: vec![LiveQuality::Low],
            ..CameraCapabilities::default()
        };
        assert!(still.validate().is_err(), "qualities without live");
    }

    #[test]
    fn a_camera_moves_only_the_ways_it_can() {
        let turns = CameraCapabilities {
            pan_tilt: true,
            ..CameraCapabilities::default()
        };
        let left = Service::CameraMove(CameraMove {
            pan: Some(-0.5),
            ..CameraMove::default()
        });
        let closer = Service::CameraMove(CameraMove {
            zoom: Some(1.0),
            ..CameraMove::default()
        });
        assert_eq!(supports_service(&turns, &left), Ok(()));
        assert_eq!(
            supports_service(&turns, &closer),
            Err("can't zoom".to_owned())
        );
        assert_eq!(supports_service(&turns, &Service::CameraStop), Ok(()));
        assert_eq!(
            supports_service(&CameraCapabilities::default(), &Service::CameraStop),
            Err("can't move".to_owned())
        );
        assert_eq!(
            supports_service(
                &turns,
                &Service::CameraGoToPreset(CameraGoToPreset { preset: "1".into() })
            ),
            Err("can't go to a preset".to_owned())
        );
        assert!(CameraMove::default().validate().is_err(), "no direction");
        let wild = CameraMove {
            tilt: Some(2.0),
            ..CameraMove::default()
        };
        assert!(wild.validate().is_err());
        let forever = CameraMove {
            pan: Some(1.0),
            seconds: Some(60.0),
            ..CameraMove::default()
        };
        assert!(forever.validate().is_err(), "a move stops on its own soon");
        assert_eq!(
            left_duration(),
            std::time::Duration::from_millis(500),
            "half a second unless told"
        );
    }

    fn left_duration() -> std::time::Duration {
        CameraMove {
            pan: Some(-1.0),
            ..CameraMove::default()
        }
        .duration()
    }

    #[test]
    fn presets_are_named_once_each_and_only_on_a_camera_that_has_them() {
        let preset = |id: &str| CameraPreset {
            id: id.into(),
            name: format!("Spot {id}"),
        };
        let state = CameraState {
            state: CameraStatus::Idle,
            motion_detection: None,
            presets: Some(vec![preset("1"), preset("2")]),
        };
        assert!(state.validate().is_ok());
        assert!(fits(&CameraCapabilities::default(), &state).is_err());
        let doubled = CameraState {
            presets: Some(vec![preset("1"), preset("1")]),
            ..state
        };
        assert!(doubled.validate().is_err());
    }
}
