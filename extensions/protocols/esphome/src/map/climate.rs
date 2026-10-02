//! ESPHome's `climate`. Temperatures become °C; the device's unit is what it speaks.

use esphome_client::types::{
    ClimateCommandRequest, ClimateStateResponse, ListEntitiesClimateResponse,
};
use irori_protocol::ProtocolError;
use irori_protocol::types::units::{TemperatureUnit, round_to};
use irori_protocol::types::{
    Capabilities, ClimateCapabilities, ClimateState, EntityDescription, EntityKind, HumidityRange,
    HvacAction, HvacMode, Name, Service, State, UniqueId,
};

use super::{category, celsius, entity_id, optional, temperature_unit};

/// ESPHome's `ClimateMode`, by number, as Home Assistant's mode.
const CLIMATE_MODES: [(i32, HvacMode); 7] = [
    (0, HvacMode::Off),
    (1, HvacMode::HeatCool),
    (2, HvacMode::Cool),
    (3, HvacMode::Heat),
    (4, HvacMode::FanOnly),
    (5, HvacMode::Dry),
    (6, HvacMode::Auto),
];

/// ESPHome's `ClimateFanMode`, by number, as Home Assistant names it.
const CLIMATE_FAN_MODES: [&str; 10] = [
    "on", "off", "auto", "low", "medium", "high", "middle", "focus", "diffuse", "quiet",
];

/// ESPHome's `ClimateSwingMode`.
const CLIMATE_SWING_MODES: [&str; 4] = ["off", "both", "vertical", "horizontal"];

/// ESPHome's `ClimatePreset`.
const CLIMATE_PRESETS: [&str; 8] = [
    "none", "home", "away", "boost", "comfort", "eco", "sleep", "activity",
];

fn named(names: &[&str], number: i32) -> Option<String> {
    usize::try_from(number)
        .ok()
        .and_then(|i| names.get(i))
        .map(|&name| name.to_owned())
}

fn numbered(names: &[&str], name: &str) -> Option<i32> {
    names
        .iter()
        .position(|n| *n == name)
        .and_then(|i| i32::try_from(i).ok())
}

/// ESPHome's `climate`. Temperatures become °C; `unit` is what the device speaks.
pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesClimateResponse,
) -> Result<EntityDescription, ProtocolError> {
    let unit = temperature_unit(entity.temperature_unit);
    let hvac_modes: Vec<HvacMode> = CLIMATE_MODES
        .iter()
        .filter(|(number, _)| entity.supported_modes.contains(number))
        .map(|(_, mode)| *mode)
        .collect();
    let step = if entity.visual_target_temperature_step > 0.0 {
        f64::from(entity.visual_target_temperature_step)
    } else {
        0.5
    };
    let custom = |standard: Vec<String>, custom: &[String]| {
        let mut all = standard;
        for mode in custom {
            if !all.contains(mode) {
                all.push(mode.clone());
            }
        }
        all
    };
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Climate, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Climate(ClimateCapabilities {
            // A climate entity with no modes listed can still be read; it's off as far as
            // anyone can tell.
            hvac_modes: if hvac_modes.is_empty() {
                vec![HvacMode::Off]
            } else {
                hvac_modes
            },
            min_temp: celsius(unit, entity.visual_min_temperature),
            max_temp: celsius(unit, entity.visual_max_temperature),
            temp_step: round_to(unit.step_to_celsius(step), 2),
            target_temperature: !entity.supports_two_point_target_temperature,
            target_temperature_range: entity.supports_two_point_target_temperature,
            target_humidity: entity.supports_target_humidity.then(|| HumidityRange {
                min: f64::from(entity.visual_min_humidity),
                max: f64::from(entity.visual_max_humidity),
            }),
            fan_modes: custom(
                entity
                    .supported_fan_modes
                    .iter()
                    .filter_map(|&n| named(&CLIMATE_FAN_MODES, n))
                    .collect(),
                &entity.supported_custom_fan_modes,
            ),
            swing_modes: entity
                .supported_swing_modes
                .iter()
                .filter_map(|&n| named(&CLIMATE_SWING_MODES, n))
                .collect(),
            preset_modes: custom(
                entity
                    .supported_presets
                    .iter()
                    .filter_map(|&n| named(&CLIMATE_PRESETS, n))
                    .collect(),
                &entity.supported_custom_presets,
            ),
        }),
        entity_category: category(entity.entity_category),
    })
}

/// A climate entity's state, in °C, trimmed to what it said it can do. ESPHome sends every
/// field whether or not the device has it, so a field it didn't claim is left out.
pub fn state(
    state: &ClimateStateResponse,
    known: &ClimateCapabilities,
    unit: TemperatureUnit,
    reads: Reads,
) -> Option<State> {
    let hvac_mode = CLIMATE_MODES
        .iter()
        .find(|(number, _)| *number == state.mode)
        .map(|(_, mode)| *mode)?;
    let temperature = |value: f32| (!value.is_nan()).then(|| celsius(unit, value));
    let one_of = |value: Option<String>, list: &[String]| value.filter(|v| list.contains(v));
    Some(State::Climate(ClimateState {
        hvac_mode,
        hvac_action: if reads.action {
            match state.action {
                0 => Some(HvacAction::Off),
                2 => Some(HvacAction::Cooling),
                3 => Some(HvacAction::Heating),
                4 => Some(HvacAction::Idle),
                5 => Some(HvacAction::Drying),
                6 => Some(HvacAction::Fan),
                7 => Some(HvacAction::Defrosting),
                _ => None,
            }
        } else {
            None
        },
        current_temperature: if reads.current_temperature {
            temperature(state.current_temperature)
        } else {
            None
        },
        target_temperature: if known.target_temperature {
            temperature(state.target_temperature)
        } else {
            None
        },
        target_temp_low: if known.target_temperature_range {
            temperature(state.target_temperature_low)
        } else {
            None
        },
        target_temp_high: if known.target_temperature_range {
            temperature(state.target_temperature_high)
        } else {
            None
        },
        current_humidity: if reads.current_humidity && !state.current_humidity.is_nan() {
            Some(round_to(f64::from(state.current_humidity), 1))
        } else {
            None
        },
        target_humidity: if known.target_humidity.is_some() && !state.target_humidity.is_nan() {
            Some(round_to(f64::from(state.target_humidity), 1))
        } else {
            None
        },
        fan_mode: one_of(
            optional(&state.custom_fan_mode).or_else(|| named(&CLIMATE_FAN_MODES, state.fan_mode)),
            &known.fan_modes,
        ),
        swing_mode: one_of(
            named(&CLIMATE_SWING_MODES, state.swing_mode),
            &known.swing_modes,
        ),
        preset_mode: one_of(
            optional(&state.custom_preset).or_else(|| named(&CLIMATE_PRESETS, state.preset)),
            &known.preset_modes,
        ),
    }))
}

/// What a climate entity said it reports beyond its capabilities: these change what's in its
/// state, not what can be asked of it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reads {
    pub current_temperature: bool,
    pub current_humidity: bool,
    pub action: bool,
}

impl Reads {
    pub fn of(entity: &ListEntitiesClimateResponse) -> Self {
        Self {
            current_temperature: entity.supports_current_temperature,
            current_humidity: entity.supports_current_humidity,
            action: entity.supports_action,
        }
    }
}

/// A climate command, with temperatures in the device's `unit`. `turn_on` goes to `on_mode`,
/// which the caller picks (`ClimateCapabilities::mode_to_turn_on`).
pub fn command(
    key: u32,
    service: &Service,
    unit: TemperatureUnit,
    on_mode: Option<HvacMode>,
) -> ClimateCommandRequest {
    #[allow(clippy::cast_possible_truncation)] // a temperature fits an f32
    let to_device = |celsius: f64| unit.from_celsius(celsius) as f32;
    let mode_number = |mode: HvacMode| {
        CLIMATE_MODES
            .iter()
            .find(|(_, m)| *m == mode)
            .map_or(0, |(number, _)| *number)
    };
    let mut request = ClimateCommandRequest {
        key,
        ..Default::default()
    };
    let set_mode = |request: &mut ClimateCommandRequest, mode: HvacMode| {
        (request.has_mode, request.mode) = (true, mode_number(mode));
    };
    match service {
        Service::ClimateSetHvacMode(data) => set_mode(&mut request, data.hvac_mode),
        Service::ClimateTurnOff => set_mode(&mut request, HvacMode::Off),
        Service::ClimateTurnOn => {
            if let Some(mode) = on_mode {
                set_mode(&mut request, mode);
            }
        }
        Service::ClimateSetTemperature(data) => {
            if let Some(mode) = data.hvac_mode {
                set_mode(&mut request, mode);
            }
            if let Some(value) = data.temperature {
                (request.has_target_temperature, request.target_temperature) =
                    (true, to_device(value));
            }
            if let Some(low) = data.target_temp_low {
                (
                    request.has_target_temperature_low,
                    request.target_temperature_low,
                ) = (true, to_device(low));
            }
            if let Some(high) = data.target_temp_high {
                (
                    request.has_target_temperature_high,
                    request.target_temperature_high,
                ) = (true, to_device(high));
            }
        }
        Service::ClimateSetHumidity(data) => {
            #[allow(clippy::cast_possible_truncation)] // 0-100
            let humidity = data.humidity as f32;
            (request.has_target_humidity, request.target_humidity) = (true, humidity);
        }
        Service::ClimateSetFanMode(data) => match numbered(&CLIMATE_FAN_MODES, &data.fan_mode) {
            Some(number) => (request.has_fan_mode, request.fan_mode) = (true, number),
            None => {
                (request.has_custom_fan_mode, request.custom_fan_mode) =
                    (true, data.fan_mode.clone());
            }
        },
        Service::ClimateSetSwingMode(data) => {
            if let Some(number) = numbered(&CLIMATE_SWING_MODES, &data.swing_mode) {
                (request.has_swing_mode, request.swing_mode) = (true, number);
            }
        }
        Service::ClimateSetPresetMode(data) => {
            match numbered(&CLIMATE_PRESETS, &data.preset_mode) {
                Some(number) => (request.has_preset, request.preset) = (true, number),
                None => {
                    (request.has_custom_preset, request.custom_preset) =
                        (true, data.preset_mode.clone());
                }
            }
        }
        _ => {}
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thermostat_in_fahrenheit_is_held_in_celsius() {
        let device = UniqueId::try_from("aa:bb").expect("valid");
        let listed = esphome_client::types::ListEntitiesClimateResponse {
            key: 9,
            name: "Hall".into(),
            supported_modes: vec![0, 3],
            visual_min_temperature: 50.0,
            visual_max_temperature: 86.0,
            visual_target_temperature_step: 1.0,
            supports_current_temperature: true,
            supported_presets: vec![5],
            supported_custom_presets: vec!["holiday".into()],
            temperature_unit: 1,
            ..Default::default()
        };
        let described = describe(&device, &listed).expect("valid");
        let Capabilities::Climate(caps) = &described.capabilities else {
            panic!("a climate entity");
        };
        assert_eq!(caps.hvac_modes, vec![HvacMode::Off, HvacMode::Heat]);
        assert_eq!((caps.min_temp, caps.max_temp), (10.0, 30.0));
        assert_eq!(caps.temp_step, 0.56);
        assert!(caps.target_temperature && !caps.target_temperature_range);
        assert_eq!(
            caps.preset_modes,
            vec!["eco".to_owned(), "holiday".to_owned()]
        );

        let unit = temperature_unit(listed.temperature_unit);
        let reported = ClimateStateResponse {
            key: 9,
            mode: 3,
            current_temperature: 68.0,
            target_temperature: 70.0,
            action: 3,
            custom_preset: "holiday".into(),
            ..Default::default()
        };
        let Some(State::Climate(state)) = state(&reported, caps, unit, Reads::of(&listed)) else {
            panic!("a climate state");
        };
        assert_eq!(state.hvac_mode, HvacMode::Heat);
        assert_eq!(state.current_temperature, Some(20.0));
        assert_eq!(state.target_temperature, Some(21.11));
        assert_eq!(
            state.hvac_action, None,
            "it didn't say it reports what it's doing"
        );
        assert_eq!(state.preset_mode.as_deref(), Some("holiday"));
        assert_eq!(
            irori_protocol::types::Capabilities::Climate(caps.clone()).fits(&State::Climate(state)),
            Ok(())
        );

        let warmer = command(
            9,
            &Service::ClimateSetTemperature(irori_protocol::types::ClimateSetTemperature {
                temperature: Some(21.0),
                ..Default::default()
            }),
            unit,
            None,
        );
        assert!(warmer.has_target_temperature && !warmer.has_mode);
        assert!((warmer.target_temperature - 69.8).abs() < 0.01);
        let on = command(9, &Service::ClimateTurnOn, unit, caps.mode_to_turn_on(None));
        assert_eq!((on.has_mode, on.mode), (true, 3));
        let custom = command(
            9,
            &Service::ClimateSetPresetMode(irori_protocol::types::ClimatePresetMode {
                preset_mode: "holiday".into(),
            }),
            unit,
            None,
        );
        assert!(custom.has_custom_preset && !custom.has_preset);
    }
}
