//! ESPHome's `text`: a piece of text set from outside, unlike a `text_sensor`.

use esphome_client::types::{ListEntitiesTextResponse, TextCommandRequest, TextStateResponse};
use irori_protocol::ProtocolError;
use irori_protocol::types::{
    Capabilities, EntityDescription, EntityKind, Name, Service, State, TextCapabilities, TextMode,
    TextState, UniqueId,
};

use super::{category, entity_id, optional};

/// ESPHome's `text`: a piece of text set from outside, unlike a `text_sensor`.
pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesTextResponse,
) -> Result<EntityDescription, ProtocolError> {
    // Irori holds at most 255 characters, as Home Assistant does.
    let max_length = entity.max_length.min(255);
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Text, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Text(TextCapabilities {
            min_length: entity.min_length.min(max_length),
            max_length,
            pattern: optional(&entity.pattern),
            // ESPHome's `TextMode` (api.proto): 0 text, 1 password.
            mode: if entity.mode == 1 {
                TextMode::Password
            } else {
                TextMode::Text
            },
        }),
        entity_category: category(entity.entity_category),
    })
}

/// `None` when the device has no text right now.
pub fn state(state: &TextStateResponse) -> Option<State> {
    (!state.missing_state).then(|| {
        State::Text(TextState {
            value: state.state.clone(),
        })
    })
}

pub fn command(key: u32, service: &Service) -> TextCommandRequest {
    TextCommandRequest {
        key,
        state: match service {
            Service::TextSetValue(data) => data.value.clone(),
            _ => String::new(),
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_keeps_its_lengths_and_hides_a_password() {
        let device = UniqueId::try_from("00:11:22:33:44:55").expect("valid");
        let listed = ListEntitiesTextResponse {
            key: 7,
            name: "Wi-Fi password".into(),
            min_length: 8,
            max_length: 64,
            mode: 1,
            ..Default::default()
        };
        let Capabilities::Text(caps) = describe(&device, &listed).expect("valid").capabilities
        else {
            panic!("a text");
        };
        assert_eq!((caps.min_length, caps.max_length), (8, 64));
        assert_eq!(caps.mode, TextMode::Password);
        let long = ListEntitiesTextResponse {
            max_length: 1000,
            ..listed
        };
        let Capabilities::Text(caps) = describe(&device, &long).expect("valid").capabilities else {
            panic!("a text");
        };
        assert_eq!(caps.max_length, 255, "Irori holds at most 255 characters");
    }
}
