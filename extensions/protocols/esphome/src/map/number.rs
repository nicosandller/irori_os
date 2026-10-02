//! ESPHome's `number`: a value set within a range, usually one of the device's settings.

use esphome_client::types::{
    ListEntitiesNumberResponse, NumberCommandRequest, NumberStateResponse,
};
use irori_protocol::ProtocolError;
use irori_protocol::types::{
    Capabilities, EntityDescription, EntityKind, Name, NumberCapabilities, NumberMode, NumberState,
    SensorClass, Service, State, UniqueId,
};

use super::{category, decimal, entity_id, optional};

/// ESPHome's `number`: a value set within a range, usually one of the device's settings.
pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesNumberResponse,
) -> Result<EntityDescription, ProtocolError> {
    let (min, max) = (decimal(entity.min_value), decimal(entity.max_value));
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Number, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Number(NumberCapabilities {
            min,
            max,
            // A device that leaves the step out still has one; a hundredth of the range is what a
            // slider would do anyway.
            step: Some(decimal(entity.step))
                .filter(|step| *step > 0.0)
                .unwrap_or_else(|| ((max - min) / 100.0).max(f64::MIN_POSITIVE)),
            unit: optional(&entity.unit_of_measurement),
            device_class: SensorClass::from_ha(&entity.device_class),
            // ESPHome's `NumberMode` (api.proto): 0 auto, 1 box, 2 slider.
            mode: match entity.mode {
                1 => NumberMode::Box,
                2 => NumberMode::Slider,
                _ => NumberMode::Auto,
            },
        }),
        entity_category: category(entity.entity_category),
    })
}

/// `None` when the device has no value right now.
pub fn state(state: &NumberStateResponse) -> Option<State> {
    (!state.missing_state && state.state.is_finite()).then(|| {
        State::Number(NumberState {
            value: decimal(state.state),
        })
    })
}

pub fn command(key: u32, service: &Service) -> NumberCommandRequest {
    NumberCommandRequest {
        key,
        // The core checked it's within the number's range, which came from the device.
        #[allow(clippy::cast_possible_truncation)]
        state: match service {
            Service::NumberSetValue(data) => data.value as f32,
            _ => 0.0,
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_protocol::types::EntityCategory;

    #[test]
    fn a_number_keeps_the_values_it_was_configured_with() {
        let device = UniqueId::try_from("00:11:22:33:44:55").expect("valid");
        let listed = ListEntitiesNumberResponse {
            key: 9,
            name: "Calibration".into(),
            min_value: -2.5,
            max_value: 2.5,
            step: 0.1,
            unit_of_measurement: "°C".into(),
            mode: 1,
            entity_category: 1,
            ..Default::default()
        };
        let described = describe(&device, &listed).expect("valid");
        assert_eq!(described.unique_id.as_str(), "00:11:22:33:44:55-number-9");
        assert_eq!(described.entity_category, Some(EntityCategory::Config));
        let Capabilities::Number(caps) = described.capabilities else {
            panic!("a number");
        };
        assert_eq!((caps.min, caps.max, caps.step), (-2.5, 2.5, 0.1));
        assert_eq!(caps.mode, NumberMode::Box);
        assert!(caps.validate().is_ok());

        let reported = NumberStateResponse {
            key: 9,
            state: 0.3,
            ..Default::default()
        };
        assert_eq!(
            state(&reported),
            Some(State::Number(NumberState { value: 0.3 }))
        );
        let missing = NumberStateResponse {
            missing_state: true,
            ..reported
        };
        assert_eq!(state(&missing), None);
    }
}
