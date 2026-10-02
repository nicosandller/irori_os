//! ESPHome's `event`: something that happens, e.g. a button's single or double press.

use esphome_client::types::{EventResponse, ListEntitiesEventResponse};
use irori_protocol::ProtocolError;
use irori_protocol::types::{
    Capabilities, EntityDescription, EntityKind, EventCapabilities, EventClass, EventState, Name,
    State, UniqueId,
};

use super::{category, entity_id};

/// ESPHome's `event`: something that happens, e.g. a button's single or double press.
pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesEventResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Event, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Event(EventCapabilities {
            event_types: entity.event_types.clone(),
            device_class: EventClass::from_ha(&entity.device_class),
        }),
        entity_category: category(entity.entity_category),
    })
}

/// A fired event. ESPHome sends these only as they happen, never again on reconnect, so none is
/// a replay.
pub fn state(event: &EventResponse) -> State {
    State::Event(EventState {
        event_type: event.event_type.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_lists_what_can_happen() {
        let device = UniqueId::try_from("00:11:22:33:44:55").expect("valid");
        let listed = ListEntitiesEventResponse {
            key: 9,
            name: "Doorbell".into(),
            event_types: vec!["ring".into()],
            device_class: "doorbell".into(),
            ..Default::default()
        };
        let described = describe(&device, &listed).expect("valid");
        assert_eq!(
            described.capabilities,
            Capabilities::Event(EventCapabilities {
                event_types: vec!["ring".into()],
                device_class: Some(EventClass::Doorbell),
            })
        );
        let rang = EventResponse {
            key: 9,
            event_type: "ring".into(),
            ..Default::default()
        };
        assert_eq!(
            state(&rang),
            State::Event(EventState {
                event_type: "ring".into()
            })
        );
    }
}
