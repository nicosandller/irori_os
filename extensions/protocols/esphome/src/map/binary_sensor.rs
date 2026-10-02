//! ESPHome's `binary_sensor`.

use esphome_client::types::{BinarySensorStateResponse, ListEntitiesBinarySensorResponse};
use irori_protocol::ProtocolError;
use irori_protocol::types::{
    BinarySensorCapabilities, BinarySensorClass, BinarySensorState, Capabilities,
    EntityDescription, EntityKind, Name, State, UniqueId,
};

use super::{category, entity_id};

pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesBinarySensorResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::BinarySensor, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::BinarySensor(BinarySensorCapabilities {
            device_class: BinarySensorClass::from_ha(&entity.device_class),
        }),
        entity_category: category(entity.entity_category),
    })
}

pub fn state(state: &BinarySensorStateResponse) -> State {
    State::BinarySensor(BinarySensorState { on: state.state })
}
