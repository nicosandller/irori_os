//! ESPHome's `light`: on and off, and as it says, a brightness, a colour temperature and a colour.

use esphome_client::types::{LightCommandRequest, LightStateResponse, ListEntitiesLightResponse};
use irori_protocol::ProtocolError;
use irori_protocol::types::{
    Capabilities, ColorMode, ColorTempRange, EntityDescription, EntityKind, LightCapabilities,
    LightState, Name, Service, State, UniqueId,
};

use super::{category, entity_id};

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

pub fn describe(
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

/// The light's value. `known` says whether the entity was described as dimmable or coloured:
/// reporting a capability the entity doesn't have is refused by the core, so the state is
/// trimmed to what it declared.
pub fn state(state: &LightStateResponse, known: &LightCapabilities) -> State {
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

/// A light command: on with what it was told, or off.
pub fn command(key: u32, service: &Service) -> LightCommandRequest {
    let mut request = LightCommandRequest {
        key,
        has_state: true,
        state: matches!(service, Service::LightTurnOn(_)),
        ..Default::default()
    };
    if let Service::LightTurnOn(data) = service {
        if let Some(brightness) = data.brightness {
            request.has_brightness = true;
            request.brightness = to_fraction(brightness);
        }
        if let Some(kelvin) = data.color_temp_kelvin {
            request.has_color_temperature = true;
            request.color_temperature = mireds(kelvin);
        }
        if let Some([red, green, blue]) = data.rgb {
            request.has_rgb = true;
            request.red = to_fraction(red);
            request.green = to_fraction(green);
            request.blue = to_fraction(blue);
        }
    }
    request
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
        let State::Light(light) = super::state(&state, &on_off) else {
            panic!("a light reports a light state");
        };
        assert!(light.on);
        assert_eq!(light.brightness, None, "it never said it could dim");
        assert_eq!(light.color_temp_kelvin, None);
        assert_eq!(light.rgb, None);
    }
}
