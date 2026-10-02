//! ESPHome's `switch`.

use esphome_client::types::{
    ListEntitiesSwitchResponse, SwitchCommandRequest, SwitchStateResponse,
};
use irori_protocol::ProtocolError;
use irori_protocol::types::{
    Capabilities, EntityDescription, EntityKind, Name, Service, State, SwitchCapabilities,
    SwitchClass, SwitchState, UniqueId,
};

use super::{category, entity_id};

pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesSwitchResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Switch, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Switch(SwitchCapabilities {
            device_class: match entity.device_class.as_str() {
                "outlet" => Some(SwitchClass::Outlet),
                "switch" => Some(SwitchClass::Switch),
                _ => None,
            },
        }),
        entity_category: category(entity.entity_category),
    })
}

pub fn state(state: &SwitchStateResponse) -> State {
    State::Switch(SwitchState { on: state.state })
}

pub fn command(key: u32, service: &Service) -> SwitchCommandRequest {
    SwitchCommandRequest {
        key,
        state: matches!(service, Service::SwitchTurnOn),
        ..Default::default()
    }
}
