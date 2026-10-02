//! `valve`: something that lets water or gas through, or doesn't, e.g. a main water shut-off or
//! an irrigation zone. Opens and closes like a cover ([`super::opening`]), sometimes part of the
//! way.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::opening::OpeningCommand;
use super::{Data, Typed};
use crate::{InvariantError, Service, ServiceName};

use super::opening::{OpenState, OpeningAbilities, OpeningState};
use crate::EntityKind;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValveCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_class: Option<ValveClass>,
    /// Can be opened part of the way, and says how far.
    #[serde(default)]
    pub position: bool,
    /// Can be stopped while it moves.
    #[serde(default)]
    pub stop: bool,
}

impl ValveCapabilities {
    /// What it can do besides open and close, as anything that opens and closes.
    pub fn opening(&self) -> OpeningAbilities {
        OpeningAbilities {
            position: self.position,
            stop: self.stop,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ValveClass {
    Water,
    Gas,
}

impl ValveClass {
    /// The class Home Assistant calls `name`.
    pub fn from_ha(name: &str) -> Option<Self> {
        super::from_ha(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValveState {
    pub state: OpenState,
    /// 0 (closed) to 100 (open), when it can say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(max = 100))]
    pub position: Option<u8>,
}

impl ValveState {
    /// Deserialization runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), crate::InvariantError> {
        self.opening().validate(EntityKind::Valve)
    }

    /// Where it is, as anything that opens and closes.
    pub fn opening(&self) -> OpeningState {
        OpeningState {
            state: self.state,
            position: self.position,
        }
    }
}

impl From<OpeningState> for ValveState {
    fn from(opening: OpeningState) -> Self {
        Self {
            state: opening.state,
            position: opening.position,
        }
    }
}

pub(crate) fn primary(state: &ValveState) -> Typed {
    Typed::Text(state.state.as_str().to_owned())
}

/// Keeps the position it said last.
pub(crate) fn with_primary(previous: Option<&ValveState>, value: &Typed) -> Option<ValveState> {
    let state = match value {
        Typed::Text(text) => OpenState::parse(text)?,
        _ => return None,
    };
    Some(ValveState {
        state,
        position: previous.and_then(|valve| valve.position),
    })
}

pub(crate) fn data_of(name: ServiceName) -> Data {
    super::opening::data_of(name)
}

pub(crate) fn service(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    Ok(match name {
        ServiceName::ValveOpen => Service::ValveOpen,
        ServiceName::ValveClose => Service::ValveClose,
        ServiceName::ValveStop => Service::ValveStop,
        ServiceName::ValveSetPosition => Service::ValveSetPosition(super::parse(name, data)?),
        _ => return Err(super::not_mine(name)),
    })
}

pub(crate) fn asks_for(service: &Service) -> Option<Typed> {
    OpeningCommand::of(EntityKind::Valve, service).and_then(OpeningCommand::asks_for)
}

pub(crate) fn supports_service(caps: &ValveCapabilities, service: &Service) -> Result<(), String> {
    match OpeningCommand::of(EntityKind::Valve, service) {
        Some(command) => super::opening::supports(caps.opening(), command),
        None => Ok(()),
    }
}

/// Whether a reported state is one this valve can be in.
pub(crate) fn fits(caps: &ValveCapabilities, state: &ValveState) -> Result<(), String> {
    super::opening::fits(caps.opening(), state.opening())
}
