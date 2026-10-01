//! Turning what ESPHome says into what Irori's model holds.
//!
//! ESPHome identifies an entity by a `key`: a hash its firmware computes from the entity's
//! object id, stable across reboots. The protocol's own `unique_id` field was removed upstream,
//! so identity here is the device's MAC address plus that key — stable as long as the entity
//! keeps its name, which is the same promise ESPHome makes to Home Assistant.

use esphome_client::types::{
    BinarySensorStateResponse, DeviceInfoResponse, EspHomeMessage, LightStateResponse,
    ListEntitiesBinarySensorResponse, ListEntitiesLightResponse, ListEntitiesNumberResponse,
    ListEntitiesSensorResponse, ListEntitiesSwitchResponse, ListEntitiesTextSensorResponse,
    NumberStateResponse, SensorStateResponse, SwitchStateResponse, TextSensorStateResponse,
};
use irori_protocol::ProtocolError;
use irori_protocol::types::{
    BinarySensorCapabilities, BinarySensorClass, BinarySensorState, Capabilities, ColorMode,
    ColorTempRange, DeviceDescription, EntityCategory, EntityDescription, EntityKind,
    LightCapabilities, LightState, Name, NumberCapabilities, NumberMode, NumberState, ObjectId,
    SensorCapabilities, SensorClass, SensorState, SensorValue, SensorValueType, State, StateClass,
    SwitchCapabilities, SwitchClass, SwitchState, UniqueId, Unmodeled,
};

/// ESPHome's `ColorMode` enum (api.proto). The values are a bit mask of what a mode carries.
mod color_mode {
    pub const ON_OFF: i32 = 1;
    pub const COLOR_TEMPERATURE: i32 = 11;
    pub const COLD_WARM_WHITE: i32 = 19;
    pub const RGB: i32 = 35;
    pub const RGB_WHITE: i32 = 39;
    pub const RGB_COLOR_TEMPERATURE: i32 = 47;
    pub const RGB_COLD_WARM_WHITE: i32 = 51;

    /// Every mode except plain on/off carries a brightness (2 and 3 are the legacy and current
    /// brightness-only modes, the rest are colour modes that dim as well).
    pub fn dims(mode: i32) -> bool {
        mode != ON_OFF
    }

    pub fn has_color_temp(mode: i32) -> bool {
        matches!(
            mode,
            COLOR_TEMPERATURE | COLD_WARM_WHITE | RGB_COLOR_TEMPERATURE | RGB_COLD_WARM_WHITE
        )
    }

    pub fn has_rgb(mode: i32) -> bool {
        matches!(
            mode,
            RGB | RGB_WHITE | RGB_COLOR_TEMPERATURE | RGB_COLD_WARM_WHITE
        )
    }
}

/// How the device is identified to Irori: its MAC address, or its node name when a device
/// doesn't report one (the `host` platform on a machine without a MAC, for instance).
pub fn device_id(info: &DeviceInfoResponse) -> Result<UniqueId, ProtocolError> {
    let id = if info.mac_address.is_empty() {
        &info.name
    } else {
        &info.mac_address
    };
    if id.is_empty() {
        return Err(ProtocolError::new(
            "the device reported neither a MAC address nor a name, so there's nothing stable to \
             identify it by",
        ));
    }
    Ok(UniqueId::try_from(id.as_str())?)
}

/// An entity's id: the device, the kind, and ESPHome's key for it.
///
/// The kind is in there because ESPHome's key is a hash of the object id alone. Change a
/// component from a sensor to a switch while keeping its name and the key is unchanged — and an
/// entity is not allowed to change kind (`docs/specs/entities.md` §4), so the core would refuse
/// the new one and the entity would vanish. With the kind in the id they are simply two
/// different entities, and the old one is removed as any disappeared entity is.
pub fn entity_id(
    device: &UniqueId,
    kind: impl std::fmt::Display,
    key: u32,
) -> Result<UniqueId, ProtocolError> {
    Ok(UniqueId::try_from(format!("{device}-{kind}-{key}"))?)
}

pub fn device(info: &DeviceInfoResponse) -> Result<DeviceDescription, ProtocolError> {
    let name = first_non_empty(&[&info.friendly_name, &info.name])
        .ok_or_else(|| ProtocolError::new("the device reported no name"))?;
    Ok(DeviceDescription {
        unique_id: device_id(info)?,
        name: Name::try_from(name)?,
        manufacturer: optional(&info.manufacturer),
        model: optional(&info.model),
        // What's running on the device, which is what a person wants when something misbehaves.
        sw_version: optional(&info.esphome_version),
        hw_version: None,
        // An area name the device suggests; ignored if it isn't a name Irori would accept.
        suggested_area: optional(&info.suggested_area).and_then(|a| Name::try_from(a).ok()),
        via_device_unique_id: None,
    })
}

pub fn light(
    device: &UniqueId,
    entity: &ListEntitiesLightResponse,
) -> Result<EntityDescription, ProtocolError> {
    let modes = &entity.supported_color_modes;
    let color_temp = modes.iter().any(|m| color_mode::has_color_temp(*m));
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Light, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Light(LightCapabilities {
            brightness: modes.iter().any(|m| color_mode::dims(*m)),
            // Mireds are what the wire carries; Irori's model is in kelvin.
            color_temp_kelvin: color_temp
                .then(|| kelvin_range(entity.min_mireds, entity.max_mireds))
                .flatten(),
            rgb: modes.iter().any(|m| color_mode::has_rgb(*m)),
        }),
        entity_category: category(entity.entity_category),
    })
}

pub fn switch(
    device: &UniqueId,
    entity: &ListEntitiesSwitchResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Switch, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Switch(SwitchCapabilities {
            device_class: match entity.device_class.as_str() {
                "outlet" => Some(SwitchClass::Outlet),
                "switch" => Some(SwitchClass::Switch),
                _ => None,
            },
        }),
        entity_category: category(entity.entity_category),
    })
}

pub fn sensor(
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
            unit: optional(&entity.unit_of_measurement),
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

/// ESPHome's `number`: a value set within a range, usually one of the device's settings.
pub fn number(
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

/// The value as written: ESPHome sends 32-bit floats, and `0.1_f32` widened is
/// `0.10000000149011612`. Its shortest decimal form is the number the device was configured with.
pub fn decimal(value: f32) -> f64 {
    value.to_string().parse().unwrap_or(f64::from(value))
}

/// `None` when the device has no value right now.
pub fn number_state(state: &NumberStateResponse) -> Option<State> {
    (!state.missing_state && state.state.is_finite()).then(|| {
        State::Number(NumberState {
            value: decimal(state.state),
        })
    })
}

/// ESPHome's `text_sensor`: an Irori `sensor` that reports text. Its id says `text_sensor`
/// rather than `sensor`, so it never shares one with a numeric sensor of the same key.
pub fn text_sensor(
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

pub fn binary_sensor(
    device: &UniqueId,
    entity: &ListEntitiesBinarySensorResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::BinarySensor, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::BinarySensor(BinarySensorCapabilities {
            device_class: BinarySensorClass::from_ha(&entity.device_class),
        }),
        entity_category: category(entity.entity_category),
    })
}

/// The light's value. `known` says whether the entity was described as dimmable or coloured:
/// reporting a capability the entity doesn't have is refused by the core, so the state is
/// trimmed to what it declared.
pub fn light_state(state: &LightStateResponse, known: &LightCapabilities) -> State {
    let mode = state.color_mode;
    let showing_rgb = known.rgb && color_mode::has_rgb(mode);
    let showing_color_temp = known.color_temp_kelvin.is_some() && color_mode::has_color_temp(mode);
    State::Light(LightState {
        on: state.state,
        // ESPHome's brightness is 0.0-1.0; Irori's is 1-255, and 0 isn't a brightness (it's off).
        brightness: (known.brightness && color_mode::dims(mode))
            .then(|| scale_to_byte(state.brightness))
            .flatten(),
        color_mode: match (showing_rgb, showing_color_temp) {
            (true, _) => Some(ColorMode::Rgb),
            (false, true) => Some(ColorMode::ColorTemp),
            (false, false) => None,
        },
        color_temp_kelvin: showing_color_temp
            .then(|| kelvin(state.color_temperature))
            .flatten(),
        rgb: showing_rgb.then(|| {
            [
                scale_to_byte(state.red).unwrap_or(0),
                scale_to_byte(state.green).unwrap_or(0),
                scale_to_byte(state.blue).unwrap_or(0),
            ]
        }),
    })
}

/// An entity of a kind Irori doesn't model yet, as listed on its device
/// (`docs/specs/protocols.md` §6.7). `None` for listings that aren't entities at all (the
/// device's user-defined actions) and for ones too new for this build to name.
pub fn unmodeled(device: &UniqueId, message: &EspHomeMessage) -> Option<Unmodeled> {
    use EspHomeMessage as M;
    let (platform, name) = match message {
        M::ListEntitiesAlarmControlPanelResponse(e) => ("alarm_control_panel", &e.name),
        M::ListEntitiesButtonResponse(e) => ("button", &e.name),
        M::ListEntitiesCameraResponse(e) => ("camera", &e.name),
        M::ListEntitiesClimateResponse(e) => ("climate", &e.name),
        M::ListEntitiesCoverResponse(e) => ("cover", &e.name),
        M::ListEntitiesDateResponse(e) => ("date", &e.name),
        M::ListEntitiesDateTimeResponse(e) => ("datetime", &e.name),
        M::ListEntitiesEventResponse(e) => ("event", &e.name),
        M::ListEntitiesFanResponse(e) => ("fan", &e.name),
        M::ListEntitiesInfraredResponse(e) => ("infrared", &e.name),
        M::ListEntitiesLockResponse(e) => ("lock", &e.name),
        M::ListEntitiesMediaPlayerResponse(e) => ("media_player", &e.name),
        M::ListEntitiesRadioFrequencyResponse(e) => ("radio_frequency", &e.name),
        M::ListEntitiesSelectResponse(e) => ("select", &e.name),
        M::ListEntitiesSirenResponse(e) => ("siren", &e.name),
        M::ListEntitiesTextResponse(e) => ("text", &e.name),
        M::ListEntitiesTimeResponse(e) => ("time", &e.name),
        M::ListEntitiesUpdateResponse(e) => ("update", &e.name),
        M::ListEntitiesValveResponse(e) => ("valve", &e.name),
        M::ListEntitiesWaterHeaterResponse(e) => ("water_heater", &e.name),
        _ => return None,
    };
    Some(Unmodeled {
        device_unique_id: Some(device.clone()),
        platform: ObjectId::try_from(platform).ok()?,
        name: Name::try_from(name.trim()).ok(),
    })
}

/// ESPHome's `EntityCategory` (api.proto): 0 is none, 1 config, 2 diagnostic.
fn category(category: i32) -> Option<EntityCategory> {
    match category {
        1 => Some(EntityCategory::Config),
        2 => Some(EntityCategory::Diagnostic),
        _ => None,
    }
}

/// `None` when the device says it has no text right now.
pub fn text_sensor_state(state: &TextSensorStateResponse) -> Option<State> {
    (!state.missing_state).then(|| {
        State::Sensor(SensorState {
            value: SensorValue::Text(state.state.clone()),
        })
    })
}

pub fn switch_state(state: &SwitchStateResponse) -> State {
    State::Switch(SwitchState { on: state.state })
}

pub fn binary_sensor_state(state: &BinarySensorStateResponse) -> State {
    State::BinarySensor(BinarySensorState { on: state.state })
}

/// `None` when the device says it has no reading right now: the entity becomes unknown rather
/// than keeping a stale number (`docs/specs/entities.md` §5).
pub fn sensor_state(state: &SensorStateResponse) -> Option<State> {
    let value = f64::from(state.state);
    if state.missing_state || !value.is_finite() {
        return None;
    }
    Some(State::Sensor(SensorState {
        value: SensorValue::Number(value),
    }))
}

/// 0.0-1.0 to 1-255. `None` for nothing (which is "off", not a brightness).
fn scale_to_byte(value: f32) -> Option<u8> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 1..=255 first"
    )]
    Some((f64::from(value) * 255.0).round().clamp(1.0, 255.0) as u8)
}

/// Mireds to kelvin. Irori's model is 1000-20000 K; anything outside is left unset rather than
/// clamped to a colour the light isn't showing.
fn kelvin(mireds: f32) -> Option<u16> {
    if !mireds.is_finite() || mireds <= 0.0 {
        return None;
    }
    let kelvin = 1_000_000.0 / f64::from(mireds);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "range-checked on the line above"
    )]
    (1000.0..=20000.0)
        .contains(&kelvin)
        .then(|| kelvin.round() as u16)
}

/// Mireds run the other way round from kelvin: the smallest mired is the coolest light.
fn kelvin_range(min_mireds: f32, max_mireds: f32) -> Option<ColorTempRange> {
    let (min, max) = (kelvin(max_mireds)?, kelvin(min_mireds)?);
    (min <= max).then_some(ColorTempRange { min, max })
}

/// Kelvin back to mireds, for a command.
pub fn mireds(kelvin: u16) -> f32 {
    1_000_000.0 / f32::from(kelvin)
}

/// 0-255 back to ESPHome's 0.0-1.0.
pub fn to_fraction(value: u8) -> f32 {
    f32::from(value) / 255.0
}

fn optional(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn first_non_empty<'a>(values: &[&'a String]) -> Option<&'a str> {
    values
        .iter()
        .map(|value| value.as_str())
        .find(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brightness_crosses_the_two_scales() {
        assert_eq!(scale_to_byte(1.0), Some(255));
        assert_eq!(scale_to_byte(0.5), Some(128));
        // Off is not a brightness of zero: Irori says 1-255 or nothing at all.
        assert_eq!(scale_to_byte(0.0), None);
        assert_eq!(scale_to_byte(f32::NAN), None);
        assert_eq!(to_fraction(255), 1.0);
    }

    #[test]
    fn colour_temperature_crosses_the_two_scales() {
        // 370 mireds is the classic warm white, 2700 K.
        assert_eq!(kelvin(370.0), Some(2703));
        assert_eq!(kelvin(0.0), None);
        // A range comes in mireds, smallest first, and leaves in kelvin, smallest first.
        assert_eq!(
            kelvin_range(153.0, 500.0),
            Some(ColorTempRange {
                min: 2000,
                max: 6536
            })
        );
        assert!((mireds(2703) - 369.9).abs() < 0.5);
    }

    #[test]
    fn a_light_reports_only_what_it_said_it_could_do() {
        let on_off = LightCapabilities::default();
        let state = LightStateResponse {
            key: 1,
            state: true,
            brightness: 1.0,
            color_mode: color_mode::ON_OFF,
            ..Default::default()
        };
        let State::Light(light) = light_state(&state, &on_off) else {
            panic!("a light reports a light state");
        };
        assert!(light.on);
        assert_eq!(light.brightness, None, "it never said it could dim");
        assert_eq!(light.color_temp_kelvin, None);
        assert_eq!(light.rgb, None);
    }

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
        let described = text_sensor(&device, &listed).expect("valid");
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
            text_sensor_state(&state),
            Some(State::Sensor(SensorState {
                value: SensorValue::Text("Home".into())
            }))
        );
        let missing = TextSensorStateResponse {
            missing_state: true,
            ..state
        };
        assert_eq!(text_sensor_state(&missing), None);
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
        let described = number(&device, &listed).expect("valid");
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
            number_state(&reported),
            Some(State::Number(NumberState { value: 0.3 }))
        );
        let missing = NumberStateResponse {
            missing_state: true,
            ..reported
        };
        assert_eq!(number_state(&missing), None);
    }

    #[test]
    fn a_missing_reading_is_unknown_rather_than_stale() {
        let missing = SensorStateResponse {
            key: 1,
            state: 0.0,
            missing_state: true,
            ..Default::default()
        };
        assert_eq!(sensor_state(&missing), None);
        let real = SensorStateResponse {
            key: 1,
            state: 21.5,
            missing_state: false,
            ..Default::default()
        };
        assert_eq!(
            sensor_state(&real),
            Some(State::Sensor(SensorState {
                value: SensorValue::Number(21.5)
            }))
        );
    }
}
