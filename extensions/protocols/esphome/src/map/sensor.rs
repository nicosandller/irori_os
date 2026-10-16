//! ESPHome's `sensor` (a number) and `text_sensor` (text): both are Irori sensors.

use esphome_client::types::{
    ListEntitiesSensorResponse, ListEntitiesTextSensorResponse, SensorStateResponse,
    TextSensorStateResponse,
};
use irori_protocol::ProtocolError;
use irori_protocol::types::units::Conversion;
use irori_protocol::types::{
    Capabilities, EntityDescription, EntityKind, Name, SensorCapabilities, SensorClass,
    SensorState, SensorValue, SensorValueType, State, StateClass, UniqueId,
};

use super::{category, entity_id, optional};

/// How this sensor's readings become °C, when it's a temperature in another unit.
pub fn conversion(entity: &ListEntitiesSensorResponse) -> Option<Conversion> {
    Conversion::of(
        SensorClass::from_ha(&entity.device_class),
        optional(&entity.unit_of_measurement).as_deref(),
    )
}

pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesSensorResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Sensor, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Sensor(SensorCapabilities {
            // ESPHome sensors carry numbers; text comes from `text_sensor` (below).
            value_type: SensorValueType::Number,
            // ESPHome's device classes are Home Assistant's. One Irori doesn't have is left out; the
            // reading still arrives.
            device_class: SensorClass::from_ha(&entity.device_class),
            // Temperatures are °C in the model, whatever the device speaks.
            unit: match conversion(entity) {
                Some(_) => Some(Conversion::UNIT.to_owned()),
                None => optional(&entity.unit_of_measurement),
            },
            state_class: match entity.state_class {
                1 | 4 => Some(StateClass::Measurement),
                2 => Some(StateClass::TotalIncreasing),
                3 => Some(StateClass::Total),
                _ => None,
            },
            options: Vec::new(),
        }),
        entity_category: category(entity.entity_category),
    })
}

/// `None` when the device says it has no reading right now: the entity becomes unknown rather
/// than keeping a stale number (`docs/specs/entities.md` §5).
pub fn state(state: &SensorStateResponse, conversion: Option<Conversion>) -> Option<State> {
    let value = f64::from(state.state);
    if state.missing_state || !value.is_finite() {
        return None;
    }
    Some(State::Sensor(SensorState {
        value: SensorValue::Number(match conversion {
            Some(conversion) => conversion.to_model(value),
            None => value,
        }),
    }))
}

/// ESPHome's `text_sensor`: an Irori `sensor` that reports text. Its id says `text_sensor`
/// rather than `sensor`, so it never shares one with a numeric sensor of the same key.
pub fn describe_text(
    device: &UniqueId,
    entity: &ListEntitiesTextSensorResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, "text_sensor", entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Sensor(SensorCapabilities {
            value_type: SensorValueType::Text,
            // Text sensors carry text classes (`date`, `timestamp`) or none at all.
            device_class: SensorClass::from_ha(&entity.device_class),
            unit: None,
            state_class: None,
            options: Vec::new(),
        }),
        entity_category: category(entity.entity_category),
    })
}

/// `None` when the device says it has no text right now.
pub fn text_state(state: &TextSensorStateResponse) -> Option<State> {
    (!state.missing_state).then(|| {
        State::Sensor(SensorState {
            value: SensorValue::Text(state.state.clone()),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_protocol::types::EntityCategory;

    #[test]
    fn a_text_sensor_is_a_sensor_that_reports_text() {
        let device = UniqueId::try_from("00:11:22:33:44:55").expect("valid");
        let listed = ListEntitiesTextSensorResponse {
            key: 7,
            name: "Wifi network".into(),
            device_class: "timestamp".into(),
            entity_category: 2,
            ..Default::default()
        };
        let described = describe_text(&device, &listed).expect("valid");
        assert_eq!(described.entity_category, Some(EntityCategory::Diagnostic));
        assert_eq!(
            described.unique_id.as_str(),
            "00:11:22:33:44:55-text_sensor-7"
        );
        assert!(matches!(
            described.capabilities,
            Capabilities::Sensor(SensorCapabilities {
                value_type: SensorValueType::Text,
                device_class: Some(SensorClass::Timestamp),
                ..
            })
        ));
        let state = TextSensorStateResponse {
            key: 7,
            state: "Home".into(),
            ..Default::default()
        };
        assert_eq!(
            text_state(&state),
            Some(State::Sensor(SensorState {
                value: SensorValue::Text("Home".into())
            }))
        );
        let missing = TextSensorStateResponse {
            missing_state: true,
            ..state
        };
        assert_eq!(text_state(&missing), None);
    }

    #[test]
    fn a_fahrenheit_temperature_is_celsius_in_the_model() {
        let device = UniqueId::try_from("00:11:22:33:44:55").expect("valid");
        let listed = ListEntitiesSensorResponse {
            key: 3,
            name: "Outside".into(),
            device_class: "temperature".into(),
            unit_of_measurement: "°F".into(),
            ..Default::default()
        };
        let conversion = conversion(&listed);
        let Capabilities::Sensor(caps) = describe(&device, &listed).expect("valid").capabilities
        else {
            panic!("a sensor");
        };
        assert_eq!(caps.unit.as_deref(), Some("°C"));
        let reading = SensorStateResponse {
            key: 3,
            state: 70.0,
            ..Default::default()
        };
        assert_eq!(
            state(&reading, conversion),
            Some(State::Sensor(SensorState {
                value: SensorValue::Number(21.11)
            }))
        );

        // A humidity that happens to say °F isn't a temperature, and one in °C needs nothing.
        let humidity = ListEntitiesSensorResponse {
            device_class: "humidity".into(),
            ..listed.clone()
        };
        assert_eq!(super::conversion(&humidity), None);
        let celsius = ListEntitiesSensorResponse {
            unit_of_measurement: "°C".into(),
            ..listed
        };
        assert_eq!(super::conversion(&celsius), None);
    }

    #[test]
    fn a_missing_reading_is_unknown_rather_than_stale() {
        let missing = SensorStateResponse {
            key: 1,
            state: 0.0,
            missing_state: true,
            ..Default::default()
        };
        assert_eq!(state(&missing, None), None);
        let real = SensorStateResponse {
            key: 1,
            state: 21.5,
            missing_state: false,
            ..Default::default()
        };
        assert_eq!(
            state(&real, None),
            Some(State::Sensor(SensorState {
                value: SensorValue::Number(21.5)
            }))
        );
    }
}
