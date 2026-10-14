//! ESPHome's `number`: a value set within a range, usually one of the device's settings.

use esphome_client::types::{
    ListEntitiesNumberResponse, NumberCommandRequest, NumberStateResponse,
};
use irori_protocol::ProtocolError;
use irori_protocol::types::units::Conversion;
use irori_protocol::types::{
    Capabilities, EntityDescription, EntityKind, Name, NumberCapabilities, NumberMode, NumberState,
    SensorClass, Service, State, UniqueId,
};

use super::{category, decimal, entity_id, optional};

/// How this number's value and range become °C, when it's a temperature in another unit.
pub fn conversion(entity: &ListEntitiesNumberResponse) -> Option<Conversion> {
    Conversion::of(
        SensorClass::from_ha(&entity.device_class),
        optional(&entity.unit_of_measurement).as_deref(),
    )
}

/// ESPHome's `number`: a value set within a range, usually one of the device's settings.
pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesNumberResponse,
) -> Result<EntityDescription, ProtocolError> {
    let conversion = conversion(entity);
    let convert = |value: f64| conversion.map_or(value, |c| c.to_model(value));
    let (min, max) = (
        convert(decimal(entity.min_value)),
        convert(decimal(entity.max_value)),
    );
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
                .map(|step| conversion.map_or(step, |c| c.step_to_model(step)))
                .filter(|step| *step > 0.0)
                .unwrap_or_else(|| ((max - min) / 100.0).max(f64::MIN_POSITIVE)),
            // Temperatures are °C in the model, whatever the device speaks.
            unit: match conversion {
                Some(_) => Some(Conversion::UNIT.to_owned()),
                None => optional(&entity.unit_of_measurement),
            },
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
pub fn state(state: &NumberStateResponse, conversion: Option<Conversion>) -> Option<State> {
    (!state.missing_state && state.state.is_finite()).then(|| {
        let value = decimal(state.state);
        State::Number(NumberState {
            value: conversion.map_or(value, |c| c.to_model(value)),
        })
    })
}

pub fn command(
    key: u32,
    service: &Service,
    conversion: Option<Conversion>,
) -> NumberCommandRequest {
    NumberCommandRequest {
        key,
        // The core checked it's within the number's range, which came from the device.
        #[allow(clippy::cast_possible_truncation)]
        state: match service {
            Service::NumberSetValue(data) => {
                conversion.map_or(data.value, |c| c.from_model(data.value)) as f32
            }
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
    fn a_fahrenheit_setting_is_celsius_in_the_model_and_fahrenheit_on_the_wire() {
        let device = UniqueId::try_from("00:11:22:33:44:55").expect("valid");
        let listed = ListEntitiesNumberResponse {
            key: 4,
            name: "Frost guard".into(),
            device_class: "temperature".into(),
            unit_of_measurement: "°F".into(),
            min_value: 32.0,
            max_value: 50.0,
            step: 1.0,
            ..Default::default()
        };
        let conversion = conversion(&listed);
        let Capabilities::Number(caps) = describe(&device, &listed).expect("valid").capabilities
        else {
            panic!("a number");
        };
        assert_eq!((caps.min, caps.max, caps.step), (0.0, 10.0, 0.5556));
        assert_eq!(caps.unit.as_deref(), Some("°C"));
        assert!(caps.validate().is_ok());

        let reported = NumberStateResponse {
            key: 4,
            state: 41.0,
            ..Default::default()
        };
        assert_eq!(
            state(&reported, conversion),
            Some(State::Number(NumberState { value: 5.0 }))
        );
        let set = Service::NumberSetValue(irori_protocol::types::NumberSetValue { value: 5.0 });
        assert_eq!(command(4, &set, conversion).state, 41.0);
    }

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
            state(&reported, None),
            Some(State::Number(NumberState { value: 0.3 }))
        );
        let missing = NumberStateResponse {
            missing_state: true,
            ..reported
        };
        assert_eq!(state(&missing, None), None);
    }
}
