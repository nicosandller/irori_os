//! ESPHome's `button`: something to press. It has no state, so nothing is ever reported for it.

use esphome_client::types::{ButtonCommandRequest, ListEntitiesButtonResponse};
use irori_protocol::ProtocolError;
use irori_protocol::types::{
    ButtonCapabilities, ButtonClass, Capabilities, EntityDescription, EntityKind, Name, UniqueId,
};

use super::{category, entity_id};

/// ESPHome's `button`: something to press. It has no state, so nothing is ever reported for it.
pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesButtonResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Button, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Button(ButtonCapabilities {
            device_class: ButtonClass::from_ha(&entity.device_class),
        }),
        entity_category: category(entity.entity_category),
    })
}

pub fn command(key: u32) -> ButtonCommandRequest {
    ButtonCommandRequest {
        key,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_button_says_what_it_does() {
        let device = UniqueId::try_from("00:11:22:33:44:55").expect("valid");
        let listed = ListEntitiesButtonResponse {
            key: 8,
            name: "Restart".into(),
            device_class: "restart".into(),
            entity_category: 1,
            ..Default::default()
        };
        let described = describe(&device, &listed).expect("valid");
        assert_eq!(described.unique_id.as_str(), "00:11:22:33:44:55-button-8");
        assert_eq!(
            described.capabilities,
            Capabilities::Button(ButtonCapabilities {
                device_class: Some(ButtonClass::Restart)
            })
        );
    }
}
