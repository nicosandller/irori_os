//! `cover`: something that opens and closes, e.g. a blind, a curtain, a garage door, a gate.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::opening::OpeningCommand;
use super::{Data, Typed};
use crate::{Service, ServiceName};

use super::opening::{OpenState, OpeningAbilities, OpeningState};
use crate::{EntityKind, InvariantError};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoverCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_class: Option<CoverClass>,
    /// Can be sent to a position between open and closed, and says where it is.
    #[serde(default)]
    pub position: bool,
    /// Has slats that tilt, and says how far.
    #[serde(default)]
    pub tilt: bool,
    /// Can be stopped while it's moving.
    #[serde(default)]
    pub stop: bool,
}

/// What it covers, when that's one of Home Assistant's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoverClass {
    Awning,
    Blind,
    Curtain,
    Damper,
    Door,
    Garage,
    Gate,
    Shade,
    Shutter,
    Window,
}

impl CoverClass {
    /// The class Home Assistant calls `name`.
    pub fn from_ha(name: &str) -> Option<Self> {
        super::from_ha(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoverState {
    pub state: OpenState,
    /// 0 (closed) to 100 (open), when it can say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(max = 100))]
    pub position: Option<u8>,
    /// 0 (closed) to 100 (open), for one whose slats tilt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(max = 100))]
    pub tilt: Option<u8>,
}

impl CoverState {
    /// Deserialization runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        self.opening().validate(EntityKind::Cover)?;
        match self.tilt {
            Some(tilt) if tilt > 100 => Err(InvariantError(format!(
                "a cover's tilt is 0-100, not {tilt}"
            ))),
            _ => Ok(()),
        }
    }

    /// Where it is, as anything that opens and closes.
    pub fn opening(&self) -> OpeningState {
        OpeningState {
            state: self.state,
            position: self.position,
        }
    }

    /// At `opening`, with its tilt as it was.
    pub fn at(opening: OpeningState, tilt: Option<u8>) -> Self {
        Self {
            state: opening.state,
            position: opening.position,
            tilt,
        }
    }
}

impl CoverCapabilities {
    /// What it can do besides open and close, as anything that opens and closes.
    pub fn opening(&self) -> OpeningAbilities {
        OpeningAbilities {
            position: self.position,
            stop: self.stop,
        }
    }
}

/// Data for `cover.set_tilt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTilt {
    /// 0 (closed) to 100 (open).
    #[schemars(range(max = 100))]
    pub tilt: u8,
}

/// Whether a cover's slats can be tilted.
pub(crate) fn supports_tilt(caps: &CoverCapabilities, data: &SetTilt) -> Result<(), String> {
    if !caps.tilt {
        return Err("has nothing to tilt".into());
    }
    if data.tilt > 100 {
        return Err(format!("takes a tilt from 0 to 100, not {}", data.tilt));
    }
    Ok(())
}

/// Whether a reported state is one this cover can be in.
pub(crate) fn fits(caps: &CoverCapabilities, state: &CoverState) -> Result<(), String> {
    super::opening::fits(caps.opening(), state.opening())?;
    if state.tilt.is_some() && !caps.tilt {
        return Err("it reports a tilt, but said it has nothing to tilt".into());
    }
    Ok(())
}

pub(crate) fn primary(state: &CoverState) -> Typed {
    Typed::Text(state.state.as_str().to_owned())
}

/// Keeps the position and tilt it said last.
pub(crate) fn with_primary(previous: Option<&CoverState>, value: &Typed) -> Option<CoverState> {
    let state = match value {
        Typed::Text(text) => OpenState::parse(text)?,
        _ => return None,
    };
    Some(match previous {
        Some(cover) => CoverState {
            state,
            ..cover.clone()
        },
        None => CoverState {
            state,
            position: None,
            tilt: None,
        },
    })
}

pub(crate) fn data_of(name: ServiceName) -> Data {
    match name {
        ServiceName::CoverSetTilt => Data::Required,
        name => super::opening::data_of(name),
    }
}

pub(crate) fn service(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    Ok(match name {
        ServiceName::CoverOpen => Service::CoverOpen,
        ServiceName::CoverClose => Service::CoverClose,
        ServiceName::CoverStop => Service::CoverStop,
        ServiceName::CoverSetPosition => Service::CoverSetPosition(super::parse(name, data)?),
        ServiceName::CoverSetTilt => Service::CoverSetTilt(super::parse(name, data)?),
        _ => return Err(super::not_mine(name)),
    })
}

/// A tilt leaves nothing for a toggle to go by.
pub(crate) fn asks_for(service: &Service) -> Option<Typed> {
    OpeningCommand::of(EntityKind::Cover, service).and_then(OpeningCommand::asks_for)
}

pub(crate) fn supports_service(caps: &CoverCapabilities, service: &Service) -> Result<(), String> {
    match service {
        Service::CoverSetTilt(data) => supports_tilt(caps, data),
        service => match OpeningCommand::of(EntityKind::Cover, service) {
            Some(command) => super::opening::supports(caps.opening(), command),
            None => Ok(()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cover_does_only_what_it_says_it_can() {
        let blind = CoverCapabilities {
            device_class: Some(CoverClass::Blind),
            position: true,
            tilt: false,
            stop: true,
        };
        assert_eq!(
            supports_tilt(&blind, &SetTilt { tilt: 20 }),
            Err("has nothing to tilt".to_owned())
        );
        let garage = CoverCapabilities::default();
        assert_eq!(garage.opening(), OpeningAbilities::default());
        assert!(
            fits(
                &garage,
                &CoverState {
                    state: OpenState::Open,
                    position: Some(100),
                    tilt: None
                }
            )
            .is_err()
        );
    }

    #[test]
    fn a_position_above_100_is_refused() {
        let state: Result<CoverState, _> =
            serde_json::from_str(r#"{"state": "open", "position": 140}"#);
        // u8 takes 140, so the range is checked by `validate`, which a `State` runs.
        assert!(state.is_ok_and(|s| s.validate().is_err()));
        assert_eq!(OpenState::parse("closing"), Some(OpenState::Closing));
    }
}
