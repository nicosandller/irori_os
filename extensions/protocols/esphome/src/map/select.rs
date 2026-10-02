//! ESPHome's `select`: one choice out of a fixed list.

use esphome_client::types::{
    ListEntitiesSelectResponse, SelectCommandRequest, SelectStateResponse,
};
use irori_protocol::ProtocolError;
use irori_protocol::types::{
    Capabilities, EntityDescription, EntityKind, Name, SelectCapabilities, SelectState, Service,
    State, UniqueId,
};

use super::{category, entity_id};

/// ESPHome's `select`: one choice out of a fixed list.
pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesSelectResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Select, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Select(SelectCapabilities {
            options: entity.options.clone(),
        }),
        entity_category: category(entity.entity_category),
    })
}

/// `None` when the device has no choice to report right now.
pub fn state(state: &SelectStateResponse) -> Option<State> {
    (!state.missing_state).then(|| {
        State::Select(SelectState {
            option: state.state.clone(),
        })
    })
}

pub fn command(key: u32, service: &Service) -> SelectCommandRequest {
    SelectCommandRequest {
        key,
        state: match service {
            Service::SelectSelectOption(data) => data.option.clone(),
            _ => String::new(),
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_select_offers_the_devices_options() {
        let device = UniqueId::try_from("00:11:22:33:44:55").expect("valid");
        let listed = ListEntitiesSelectResponse {
            key: 4,
            name: "Power-on behaviour".into(),
            options: vec!["off".into(), "on".into(), "previous".into()],
            entity_category: 1,
            ..Default::default()
        };
        let described = describe(&device, &listed).expect("valid");
        assert_eq!(described.unique_id.as_str(), "00:11:22:33:44:55-select-4");
        assert_eq!(
            described.capabilities,
            Capabilities::Select(SelectCapabilities {
                options: vec!["off".into(), "on".into(), "previous".into()]
            })
        );
        let reported = SelectStateResponse {
            key: 4,
            state: "previous".into(),
            ..Default::default()
        };
        assert_eq!(
            state(&reported),
            Some(State::Select(SelectState {
                option: "previous".into()
            }))
        );
    }
}
