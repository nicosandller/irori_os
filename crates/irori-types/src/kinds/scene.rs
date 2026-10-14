//! `scene`: a scene a device keeps and can be told to set: a Hue scene, a Zigbee group's.
//!
//! Like a button, it has no value: setting it is an action, not a state it's in, so its
//! `EntityState.state` is always `null`. Scenes Irori makes itself are flows, not this.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Typed};
use crate::{InvariantError, Service, ServiceName};

/// What a scene can do: be set. Nothing to say yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SceneCapabilities {}

pub(crate) fn data_of(_: ServiceName) -> Data {
    Data::None
}

/// Home Assistant's name for setting a scene: `scene.turn_on`.
pub(crate) fn service(
    name: ServiceName,
    _: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    match name {
        ServiceName::SceneTurnOn => Ok(Service::SceneTurnOn),
        _ => Err(super::not_mine(name)),
    }
}

/// Setting a scene leaves nothing of its own to remember.
pub(crate) fn asks_for(_: &Service) -> Option<Typed> {
    None
}
