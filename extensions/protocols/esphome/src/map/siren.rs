//! ESPHome's `siren`.

use esphome_client::types::{ListEntitiesSirenResponse, SirenCommandRequest, SirenStateResponse};
use irori_protocol::ProtocolError;
use irori_protocol::types::{
    Capabilities, EntityDescription, EntityKind, Name, Service, SirenCapabilities, State, UniqueId,
};

use super::{category, entity_id};
use irori_protocol::types::SirenState;

/// ESPHome's `siren`.
pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesSirenResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Siren, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Siren(SirenCapabilities {
            tones: entity.tones.clone(),
            volume: entity.supports_volume,
            duration: entity.supports_duration,
        }),
        entity_category: category(entity.entity_category),
    })
}

pub fn command(key: u32, service: &Service) -> SirenCommandRequest {
    let mut request = SirenCommandRequest {
        key,
        has_state: true,
        ..Default::default()
    };
    if let Service::SirenTurnOn(data) = service {
        request.state = true;
        if let Some(tone) = &data.tone {
            (request.has_tone, request.tone) = (true, tone.clone());
        }
        if let Some(volume) = data.volume_level {
            #[allow(clippy::cast_possible_truncation)]
            let volume = volume as f32;
            (request.has_volume, request.volume) = (true, volume);
        }
        if let Some(duration) = data.duration {
            (request.has_duration, request.duration) = (true, duration);
        }
    }
    request
}

pub fn state(state: &SirenStateResponse) -> State {
    State::Siren(SirenState { on: state.state })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_siren_is_told_its_tone_volume_and_duration() {
        let request = command(
            7,
            &Service::SirenTurnOn(irori_protocol::types::SirenTurnOn {
                tone: Some("alarm".into()),
                volume_level: Some(0.5),
                duration: Some(30),
            }),
        );
        assert!(request.has_state && request.state);
        assert_eq!(request.tone, "alarm");
        assert!(request.has_volume && (request.volume - 0.5).abs() < f32::EPSILON);
        assert_eq!((request.has_duration, request.duration), (true, 30));
        let off = command(7, &Service::SirenTurnOff);
        assert!(off.has_state && !off.state && !off.has_tone);
    }
}
