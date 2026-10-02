//! ESPHome's `valve`: it opens and closes ([`super::opening`]), the way a cover does.

use esphome_client::types::{ListEntitiesValveResponse, ValveCommandRequest, ValveStateResponse};
use irori_protocol::ProtocolError;
use irori_protocol::types::{
    Capabilities, EntityDescription, EntityKind, Name, OpeningCommand, Service, State, UniqueId,
    ValveCapabilities, ValveClass, ValveState,
};

use super::opening::Target;
use super::{category, entity_id};

/// ESPHome's `valve`.
pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesValveResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Valve, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Valve(ValveCapabilities {
            device_class: ValveClass::from_ha(&entity.device_class),
            position: entity.supports_position,
            stop: entity.supports_stop,
        }),
        entity_category: category(entity.entity_category),
    })
}

/// A valve's state, trimmed to what it said it can do.
pub fn state(state: &ValveStateResponse, known: &ValveCapabilities) -> State {
    State::Valve(ValveState::from(super::opening::state(
        state.current_operation,
        state.position,
        known.opening(),
    )))
}

/// A valve command: where to go, or to stop.
pub fn command(key: u32, service: &Service) -> ValveCommandRequest {
    let mut request = ValveCommandRequest {
        key,
        ..Default::default()
    };
    match OpeningCommand::of(EntityKind::Valve, service).map(Target::from) {
        Some(Target::Position(position)) => {
            (request.has_position, request.position) = (true, position);
        }
        Some(Target::Stop) => request.stop = true,
        None => {}
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_protocol::types::OpenState;

    #[test]
    fn a_valve_reports_and_is_sent_like_a_cover() {
        let zone = ValveCapabilities {
            device_class: Some(ValveClass::Water),
            position: true,
            stop: false,
        };
        let opening = ValveStateResponse {
            key: 6,
            position: 0.3,
            current_operation: 1,
            ..Default::default()
        };
        assert_eq!(
            state(&opening, &zone),
            State::Valve(ValveState {
                state: OpenState::Opening,
                position: Some(30),
            })
        );
        let close = command(6, &Service::ValveClose);
        assert!(close.has_position && close.position == 0.0);
    }
}
