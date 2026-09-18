//! Turning what ESPHome says into what Irori's model holds.
//!
//! ESPHome identifies an entity by a `key`: a hash its firmware computes from the entity's
//! object id, stable across reboots. The protocol's own `unique_id` field was removed upstream,
//! so identity here is the device's MAC address plus that key — stable as long as the entity
//! keeps its name, which is the same promise ESPHome makes to Home Assistant.

use esphome_client::types::{
    BinarySensorStateResponse, DeviceInfoResponse, LightStateResponse,
    ListEntitiesBinarySensorResponse, ListEntitiesLightResponse, ListEntitiesSensorResponse,
    ListEntitiesSwitchResponse, SensorStateResponse, SwitchStateResponse,
};
use irori_integration::IntegrationError;
use irori_integration::types::{
    BinarySensorCapabilities, BinarySensorClass, BinarySensorState, Capabilities, ColorMode,
    ColorTempRange, DeviceDescription, EntityDescription, EntityKind, LightCapabilities,
    LightState, Name, SensorCapabilities, SensorClass, SensorState, SensorValue, SensorValueType,
    State, StateClass, SwitchCapabilities, SwitchClass, SwitchState, UniqueId,
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
pub fn device_id(info: &DeviceInfoResponse) -> Result<UniqueId, IntegrationError> {
    let id = if info.mac_address.is_empty() {
        &info.name
    } else {
        &info.mac_address
    };
    if id.is_empty() {
        return Err(IntegrationError::new(
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
    kind: EntityKind,
    key: u32,
) -> Result<UniqueId, IntegrationError> {
    Ok(UniqueId::try_from(format!("{device}-{kind}-{key}"))?)
}

pub fn device(info: &DeviceInfoResponse) -> Result<DeviceDescription, IntegrationError> {
    let name = first_non_empty(&[&info.friendly_name, &info.name])
        .ok_or_else(|| IntegrationError::new("the device reported no name"))?;
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
) -> Result<EntityDescription, IntegrationError> {
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
    })
}

pub fn switch(
    device: &UniqueId,
    entity: &ListEntitiesSwitchResponse,
) -> Result<EntityDescription, IntegrationError> {
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
    })
}

pub fn sensor(
    device: &UniqueId,
    entity: &ListEntitiesSensorResponse,
) -> Result<EntityDescription, IntegrationError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Sensor, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Sensor(SensorCapabilities {
            // ESPHome sensors carry numbers; text lives in its separate `text_sensor` kind,
            // which Irori doesn't model yet.
            value_type: SensorValueType::Number,
            device_class: sensor_class(&entity.device_class),
            unit: optional(&entity.unit_of_measurement),
            state_class: match entity.state_class {
                1 | 4 => Some(StateClass::Measurement),
                2 => Some(StateClass::TotalIncreasing),
                3 => Some(StateClass::Total),
                _ => None,
            },
        }),
    })
}

pub fn binary_sensor(
    device: &UniqueId,
    entity: &ListEntitiesBinarySensorResponse,
) -> Result<EntityDescription, IntegrationError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::BinarySensor, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::BinarySensor(BinarySensorCapabilities {
            device_class: binary_sensor_class(&entity.device_class),
        }),
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

/// ESPHome's device classes are Home Assistant's, and Irori models the ones it has a meaning
/// for. An unknown class is simply not set: the reading still arrives.
fn sensor_class(class: &str) -> Option<SensorClass> {
    Some(match class {
        "temperature" => SensorClass::Temperature,
        "humidity" => SensorClass::Humidity,
        "illuminance" => SensorClass::Illuminance,
        "pressure" | "atmospheric_pressure" => SensorClass::Pressure,
        "power" => SensorClass::Power,
        "energy" => SensorClass::Energy,
        "voltage" => SensorClass::Voltage,
        "current" => SensorClass::Current,
        "battery" => SensorClass::Battery,
        "carbon_dioxide" => SensorClass::Co2,
        "pm25" => SensorClass::Pm25,
        "signal_strength" => SensorClass::SignalStrength,
        "distance" => SensorClass::Distance,
        _ => return None,
    })
}

fn binary_sensor_class(class: &str) -> Option<BinarySensorClass> {
    Some(match class {
        "motion" => BinarySensorClass::Motion,
        "occupancy" | "presence" => BinarySensorClass::Occupancy,
        "door" | "garage_door" => BinarySensorClass::Door,
        "window" | "opening" => BinarySensorClass::Window,
        "moisture" => BinarySensorClass::Moisture,
        "smoke" => BinarySensorClass::Smoke,
        "gas" => BinarySensorClass::Gas,
        "vibration" => BinarySensorClass::Vibration,
        "plug" => BinarySensorClass::Plug,
        "connectivity" => BinarySensorClass::Connectivity,
        "problem" | "safety" => BinarySensorClass::Problem,
        "battery" => BinarySensorClass::Battery,
        _ => return None,
    })
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
