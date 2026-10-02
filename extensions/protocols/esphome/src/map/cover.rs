//! ESPHome's `cover`: it opens and closes ([`super::opening`]), and may tilt.

use esphome_client::types::{CoverCommandRequest, CoverStateResponse, ListEntitiesCoverResponse};
use irori_protocol::ProtocolError;
use irori_protocol::types::{
    Capabilities, CoverCapabilities, CoverClass, CoverState, EntityDescription, EntityKind, Name,
    OpeningCommand, Service, State, UniqueId,
};

use super::opening::{Target, from_percent, to_percent};
use super::{category, entity_id};

/// ESPHome's `cover`. It opens and closes, and may also go to a position and tilt.
pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesCoverResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Cover, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Cover(CoverCapabilities {
            device_class: CoverClass::from_ha(&entity.device_class),
            position: entity.supports_position,
            tilt: entity.supports_tilt,
            stop: entity.supports_stop,
        }),
        entity_category: category(entity.entity_category),
    })
}

/// A cover's state, trimmed to what it said it can do.
pub fn state(state: &CoverStateResponse, known: &CoverCapabilities) -> State {
    let opening = super::opening::state(state.current_operation, state.position, known.opening());
    State::Cover(CoverState::at(
        opening,
        known.tilt.then(|| to_percent(state.tilt)),
    ))
}

/// A cover command: where to go or to stop, or how far to tilt.
pub fn command(key: u32, service: &Service) -> CoverCommandRequest {
    let mut request = CoverCommandRequest {
        key,
        ..Default::default()
    };
    match OpeningCommand::of(EntityKind::Cover, service).map(Target::from) {
        Some(Target::Position(position)) => {
            (request.has_position, request.position) = (true, position);
        }
        Some(Target::Stop) => request.stop = true,
        None => {
            if let Service::CoverSetTilt(data) = service {
                (request.has_tilt, request.tilt) = (true, from_percent(data.tilt));
            }
        }
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_protocol::types::OpenState;

    #[test]
    fn a_cover_reports_where_it_is_and_is_sent_by_position() {
        let blind = CoverCapabilities {
            device_class: Some(CoverClass::Blind),
            position: true,
            tilt: false,
            stop: true,
        };
        let moving = CoverStateResponse {
            key: 3,
            position: 0.4,
            tilt: 0.9,
            current_operation: 2,
            ..Default::default()
        };
        assert_eq!(
            state(&moving, &blind),
            State::Cover(CoverState {
                state: OpenState::Closing,
                position: Some(40),
                tilt: None,
            })
        );
        let garage = CoverCapabilities::default();
        let shut = CoverStateResponse {
            key: 3,
            ..Default::default()
        };
        assert_eq!(
            state(&shut, &garage),
            State::Cover(CoverState {
                state: OpenState::Closed,
                position: None,
                tilt: None,
            })
        );
        let open = command(3, &Service::CoverOpen);
        assert!(open.has_position && (open.position - 1.0).abs() < f32::EPSILON);
        assert!(command(3, &Service::CoverStop).stop);
    }
}
