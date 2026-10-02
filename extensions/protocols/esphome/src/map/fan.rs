//! ESPHome's `fan`.

use esphome_client::types::{FanCommandRequest, FanStateResponse, ListEntitiesFanResponse};
use irori_protocol::ProtocolError;
use irori_protocol::types::{
    Capabilities, EntityDescription, EntityKind, FanCapabilities, FanDirection, FanPercentage,
    FanState, Name, Service, State, UniqueId, percentage_to_speed, speed_to_percentage,
};

use super::{category, entity_id, optional};

/// ESPHome's `fan`.
pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesFanResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Fan, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Fan(FanCapabilities {
            speed_count: if entity.supports_speed {
                u16::try_from(entity.supported_speed_count.max(1)).unwrap_or(u16::MAX)
            } else {
                0
            },
            oscillate: entity.supports_oscillation,
            direction: entity.supports_direction,
            preset_modes: entity.supported_preset_modes.clone(),
        }),
        entity_category: category(entity.entity_category),
    })
}

/// A fan's state, trimmed to what it said it can do. ESPHome reports its speed as a level out of
/// its speed count, which becomes a percentage the way Home Assistant does it.
pub fn state(state: &FanStateResponse, known: &FanCapabilities) -> State {
    State::Fan(FanState {
        on: state.state,
        percentage: (known.speed_count > 0).then(|| {
            speed_to_percentage(
                u16::try_from(state.speed_level.max(0)).unwrap_or(0),
                known.speed_count,
            )
        }),
        oscillating: known.oscillate.then_some(state.oscillating),
        direction: known.direction.then_some(if state.direction == 1 {
            FanDirection::Reverse
        } else {
            FanDirection::Forward
        }),
        preset_mode: optional(&state.preset_mode).filter(|mode| known.preset_modes.contains(mode)),
    })
}

/// A fan command. ESPHome's fan takes everything in one request, each part with its `has_` flag.
pub fn command(key: u32, service: &Service, known: Option<&FanCapabilities>) -> FanCommandRequest {
    let speed_count = known.map_or(0, |known| known.speed_count);
    let level = |percentage: u8| i32::from(percentage_to_speed(percentage, speed_count));
    let mut request = FanCommandRequest {
        key,
        ..Default::default()
    };
    match service {
        Service::FanTurnOn(data) => {
            (request.has_state, request.state) = (true, true);
            if let Some(percentage) = data.percentage {
                (request.has_speed_level, request.speed_level) = (true, level(percentage));
            }
            if let Some(mode) = &data.preset_mode {
                (request.has_preset_mode, request.preset_mode) = (true, mode.clone());
            }
        }
        Service::FanTurnOff | Service::FanSetPercentage(FanPercentage { percentage: 0 }) => {
            (request.has_state, request.state) = (true, false);
        }
        Service::FanSetPercentage(data) => {
            (request.has_state, request.state) = (true, true);
            (request.has_speed_level, request.speed_level) = (true, level(data.percentage));
        }
        Service::FanOscillate(data) => {
            (request.has_oscillating, request.oscillating) = (true, data.oscillating);
        }
        Service::FanSetDirection(data) => {
            (request.has_direction, request.direction) =
                (true, i32::from(data.direction == FanDirection::Reverse));
        }
        Service::FanSetPresetMode(data) => {
            (request.has_preset_mode, request.preset_mode) = (true, data.preset_mode.clone());
        }
        _ => {}
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fans_speed_level_is_a_percentage_and_back() {
        let known = FanCapabilities {
            speed_count: 3,
            oscillate: true,
            direction: false,
            preset_modes: vec!["sleep".into()],
        };
        let state = FanStateResponse {
            key: 5,
            state: true,
            speed_level: 2,
            oscillating: true,
            preset_mode: "sleep".into(),
            ..Default::default()
        };
        assert_eq!(
            super::state(&state, &known),
            State::Fan(FanState {
                on: true,
                percentage: Some(66),
                oscillating: Some(true),
                direction: None,
                preset_mode: Some("sleep".into()),
            })
        );
        let request = command(
            5,
            &Service::FanSetPercentage(FanPercentage { percentage: 100 }),
            Some(&known),
        );
        assert!(request.has_speed_level && request.speed_level == 3);
        let off = command(5, &Service::FanTurnOff, Some(&known));
        assert!(off.has_state && !off.state);
    }
}
