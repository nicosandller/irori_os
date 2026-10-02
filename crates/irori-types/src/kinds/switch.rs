//! `switch`: on/off, e.g. a smart plug or relay.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Typed};
use crate::{InvariantError, Service, ServiceName};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SwitchCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_class: Option<SwitchClass>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SwitchClass {
    Outlet,
    Switch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SwitchState {
    pub on: bool,
}

impl SwitchClass {
    /// The class Home Assistant calls `name` (`outlet`), as protocols that speak its vocabulary
    /// (ESPHome, MQTT discovery) report it.
    pub fn from_ha(name: &str) -> Option<Self> {
        super::from_ha(name)
    }
}

pub(crate) fn primary(state: &SwitchState) -> Typed {
    Typed::Bool(state.on)
}

pub(crate) fn with_primary(value: &Typed) -> Option<SwitchState> {
    match value {
        Typed::Bool(on) => Some(SwitchState { on: *on }),
        _ => None,
    }
}

pub(crate) fn toggle(current: Option<&Typed>) -> ServiceName {
    super::on_off_toggle(
        current,
        ServiceName::SwitchTurnOn,
        ServiceName::SwitchTurnOff,
    )
}

pub(crate) fn data_of(_: ServiceName) -> Data {
    Data::None
}

pub(crate) fn service(
    name: ServiceName,
    _: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    Ok(match name {
        ServiceName::SwitchTurnOn => Service::SwitchTurnOn,
        ServiceName::SwitchTurnOff => Service::SwitchTurnOff,
        _ => return Err(super::not_mine(name)),
    })
}

pub(crate) fn asks_for(service: &Service) -> Option<Typed> {
    match service {
        Service::SwitchTurnOn => Some(Typed::Bool(true)),
        Service::SwitchTurnOff => Some(Typed::Bool(false)),
        _ => None,
    }
}
