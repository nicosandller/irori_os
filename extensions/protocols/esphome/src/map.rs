//! Turning what ESPHome says into what Irori's model holds.
//!
//! ESPHome identifies an entity by a `key`: a hash its firmware computes from the entity's
//! object id, stable across reboots. The protocol's own `unique_id` field was removed upstream,
//! so identity here is the device's MAC address plus that key — stable as long as the entity
//! keeps its name, which is the same promise ESPHome makes to Home Assistant.

use esphome_client::types::{
    BinarySensorStateResponse, ClimateCommandRequest, ClimateStateResponse, CoverCommandRequest,
    CoverStateResponse, DeviceInfoResponse, EspHomeMessage, EventResponse, FanCommandRequest,
    FanStateResponse, LightStateResponse, ListEntitiesBinarySensorResponse,
    ListEntitiesButtonResponse, ListEntitiesClimateResponse, ListEntitiesCoverResponse,
    ListEntitiesEventResponse, ListEntitiesFanResponse, ListEntitiesLightResponse,
    ListEntitiesLockResponse, ListEntitiesNumberResponse, ListEntitiesSelectResponse,
    ListEntitiesSensorResponse, ListEntitiesSirenResponse, ListEntitiesSwitchResponse,
    ListEntitiesTextResponse, ListEntitiesTextSensorResponse, ListEntitiesValveResponse,
    LockCommandRequest, LockStateResponse, NumberStateResponse, SelectStateResponse,
    SensorStateResponse, SirenCommandRequest, SwitchStateResponse, TextSensorStateResponse,
    TextStateResponse, ValveCommandRequest, ValveStateResponse,
};
use irori_protocol::ProtocolError;
use irori_protocol::types::units::{TemperatureUnit, round_to};
use irori_protocol::types::{
    BinarySensorCapabilities, BinarySensorClass, BinarySensorState, ButtonCapabilities,
    ButtonClass, Capabilities, ClimateCapabilities, ClimateState, ColorMode, ColorTempRange,
    CoverCapabilities, CoverClass, CoverState, DeviceDescription, EntityCategory,
    EntityDescription, EntityKind, EventCapabilities, EventClass, EventState, FanCapabilities,
    FanDirection, FanPercentage, FanState, HumidityRange, HvacAction, HvacMode, LightCapabilities,
    LightState, LockCapabilities, LockCode, LockState, LockStatus, Name, NumberCapabilities,
    NumberMode, NumberState, ObjectId, OpenState, SelectCapabilities, SelectState,
    SensorCapabilities, SensorClass, SensorState, SensorValue, SensorValueType, Service,
    SirenCapabilities, State, StateClass, SwitchCapabilities, SwitchClass, SwitchState,
    TextCapabilities, TextMode, TextState, UniqueId, Unmodeled, ValveCapabilities, ValveClass,
    ValveState, percentage_to_speed, speed_to_percentage,
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

/// ESPHome's `select`: one choice out of a fixed list.
pub fn select(
    device: &UniqueId,
    entity: &ListEntitiesSelectResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Select, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Select(SelectCapabilities {
            options: entity.options.clone(),
        }),
        entity_category: category(entity.entity_category),
    })
}

/// `None` when the device has no choice to report right now.
pub fn select_state(state: &SelectStateResponse) -> Option<State> {
    (!state.missing_state).then(|| {
        State::Select(SelectState {
            option: state.state.clone(),
        })
    })
}

/// ESPHome's `cover`. It opens and closes, and may also go to a position and tilt.
pub fn cover(
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

/// 0.0-1.0 to 0-100.
fn to_percent(fraction: f32) -> u8 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        (fraction.clamp(0.0, 1.0) * 100.0).round() as u8
    }
}

/// A cover's state, trimmed to what it said it can do. ESPHome says closed is position 0, also
/// for a cover that can only open and close, and whether it's moving (`CoverOperation`: 1
/// opening, 2 closing).
pub fn cover_state(state: &CoverStateResponse, known: &CoverCapabilities) -> State {
    State::Cover(CoverState {
        state: match state.current_operation {
            1 => OpenState::Opening,
            2 => OpenState::Closing,
            _ if state.position > 0.0 => OpenState::Open,
            _ => OpenState::Closed,
        },
        position: known.position.then(|| to_percent(state.position)),
        tilt: known.tilt.then(|| to_percent(state.tilt)),
    })
}

/// A cover command: open and close are positions 1.0 and 0.0 to ESPHome, as Home Assistant sends.
pub fn cover_command(key: u32, service: &Service) -> CoverCommandRequest {
    let mut request = CoverCommandRequest {
        key,
        ..Default::default()
    };
    match service {
        Service::CoverOpen => (request.has_position, request.position) = (true, 1.0),
        Service::CoverClose => (request.has_position, request.position) = (true, 0.0),
        Service::CoverStop => request.stop = true,
        Service::CoverSetPosition(data) => {
            (request.has_position, request.position) = (true, f32::from(data.position) / 100.0);
        }
        Service::CoverSetTilt(data) => {
            (request.has_tilt, request.tilt) = (true, f32::from(data.tilt) / 100.0);
        }
        _ => {}
    }
    request
}

/// ESPHome's `fan`.
pub fn fan(
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
pub fn fan_state(state: &FanStateResponse, known: &FanCapabilities) -> State {
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
pub fn fan_command(
    key: u32,
    service: &Service,
    known: Option<&FanCapabilities>,
) -> FanCommandRequest {
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

/// The unit a climate entity or water heater speaks, from ESPHome's `TemperatureUnit`. Anything
/// unknown is °C, as Home Assistant reads it.
pub fn temperature_unit(unit: i32) -> TemperatureUnit {
    match unit {
        1 => TemperatureUnit::Fahrenheit,
        2 => TemperatureUnit::Kelvin,
        _ => TemperatureUnit::Celsius,
    }
}

/// A temperature from the device as °C, to a hundredth.
fn celsius(unit: TemperatureUnit, value: f32) -> f64 {
    round_to(unit.to_celsius(f64::from(value)), 2)
}

/// ESPHome's `climate`. Temperatures become °C; `unit` is what the device speaks.
pub fn climate(
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
pub fn climate_state(
    state: &ClimateStateResponse,
    known: &ClimateCapabilities,
    unit: TemperatureUnit,
    reads: ClimateReads,
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
pub struct ClimateReads {
    pub current_temperature: bool,
    pub current_humidity: bool,
    pub action: bool,
}

impl ClimateReads {
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
pub fn climate_command(
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

/// ESPHome's `valve`.
pub fn valve(
    device: &UniqueId,
    entity: &ListEntitiesValveResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Valve, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Valve(ValveCapabilities {
            device_class: ValveClass::from_ha(&entity.device_class),
            position: entity.supports_position,
            stop: entity.supports_stop,
        }),
        entity_category: category(entity.entity_category),
    })
}

/// A valve's state, like a cover's: closed is position 0, `current_operation` 1 opening, 2 closing.
pub fn valve_state(state: &ValveStateResponse, known: &ValveCapabilities) -> State {
    State::Valve(ValveState {
        state: match state.current_operation {
            1 => OpenState::Opening,
            2 => OpenState::Closing,
            _ if state.position > 0.0 => OpenState::Open,
            _ => OpenState::Closed,
        },
        position: known.position.then(|| to_percent(state.position)),
    })
}

pub fn valve_command(key: u32, service: &Service) -> ValveCommandRequest {
    let mut request = ValveCommandRequest {
        key,
        ..Default::default()
    };
    match service {
        Service::ValveOpen => (request.has_position, request.position) = (true, 1.0),
        Service::ValveClose => (request.has_position, request.position) = (true, 0.0),
        Service::ValveStop => request.stop = true,
        Service::ValveSetPosition(data) => {
            (request.has_position, request.position) = (true, f32::from(data.position) / 100.0);
        }
        _ => {}
    }
    request
}

/// ESPHome's `siren`.
pub fn siren(
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

pub fn siren_command(key: u32, service: &Service) -> SirenCommandRequest {
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

/// ESPHome's `lock`.
pub fn lock(
    device: &UniqueId,
    entity: &ListEntitiesLockResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Lock, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Lock(LockCapabilities {
            open: entity.supports_open,
            requires_code: entity.requires_code,
            code_format: optional(&entity.code_format),
        }),
        entity_category: category(entity.entity_category),
    })
}

/// ESPHome's `LockState` (api.proto); 0 is "none", which says nothing.
pub fn lock_state(state: &LockStateResponse) -> Option<State> {
    let state = match state.state {
        1 => LockStatus::Locked,
        2 => LockStatus::Unlocked,
        3 => LockStatus::Jammed,
        4 => LockStatus::Locking,
        5 => LockStatus::Unlocking,
        6 => LockStatus::Opening,
        7 => LockStatus::Open,
        _ => return None,
    };
    Some(State::Lock(LockState { state }))
}

/// A lock command (`LockCommand`: 0 unlock, 1 lock, 2 open), with its code when one was given.
pub fn lock_command(key: u32, service: &Service) -> LockCommandRequest {
    let (command, code) = match service {
        Service::LockUnlock(code) => (0, code),
        Service::LockOpen(code) => (2, code),
        Service::LockLock(code) => (1, code),
        _ => (1, &LockCode::default()),
    };
    LockCommandRequest {
        key,
        command,
        has_code: code.code.is_some(),
        code: code.code.clone().unwrap_or_default(),
        ..Default::default()
    }
}

/// ESPHome's `event`: something that happens, e.g. a button's single or double press.
pub fn event(
    device: &UniqueId,
    entity: &ListEntitiesEventResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Event, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Event(EventCapabilities {
            event_types: entity.event_types.clone(),
            device_class: EventClass::from_ha(&entity.device_class),
        }),
        entity_category: category(entity.entity_category),
    })
}

/// A fired event. ESPHome sends these only as they happen, never again on reconnect, so none is
/// a replay.
pub fn event_state(event: &EventResponse) -> State {
    State::Event(EventState {
        event_type: event.event_type.clone(),
    })
}

/// ESPHome's `button`: something to press. It has no state, so nothing is ever reported for it.
pub fn button(
    device: &UniqueId,
    entity: &ListEntitiesButtonResponse,
) -> Result<EntityDescription, ProtocolError> {
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Button, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Button(ButtonCapabilities {
            device_class: ButtonClass::from_ha(&entity.device_class),
        }),
        entity_category: category(entity.entity_category),
    })
}

/// ESPHome's `text`: a piece of text set from outside, unlike a `text_sensor`.
pub fn text(
    device: &UniqueId,
    entity: &ListEntitiesTextResponse,
) -> Result<EntityDescription, ProtocolError> {
    // Irori holds at most 255 characters, as Home Assistant does.
    let max_length = entity.max_length.min(255);
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::Text, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::Text(TextCapabilities {
            min_length: entity.min_length.min(max_length),
            max_length,
            pattern: optional(&entity.pattern),
            // ESPHome's `TextMode` (api.proto): 0 text, 1 password.
            mode: if entity.mode == 1 {
                TextMode::Password
            } else {
                TextMode::Text
            },
        }),
        entity_category: category(entity.entity_category),
    })
}

/// `None` when the device has no text right now.
pub fn text_state(state: &TextStateResponse) -> Option<State> {
    (!state.missing_state).then(|| {
        State::Text(TextState {
            value: state.state.clone(),
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
        M::ListEntitiesCameraResponse(e) => ("camera", &e.name),
        M::ListEntitiesClimateResponse(e) => ("climate", &e.name),
        M::ListEntitiesDateResponse(e) => ("date", &e.name),
        M::ListEntitiesDateTimeResponse(e) => ("datetime", &e.name),
        M::ListEntitiesInfraredResponse(e) => ("infrared", &e.name),
        M::ListEntitiesMediaPlayerResponse(e) => ("media_player", &e.name),
        M::ListEntitiesRadioFrequencyResponse(e) => ("radio_frequency", &e.name),
        M::ListEntitiesTimeResponse(e) => ("time", &e.name),
        M::ListEntitiesUpdateResponse(e) => ("update", &e.name),
        M::ListEntitiesWaterHeaterResponse(e) => ("water_heater", &e.name),
        _ => return None,
    };
    Some(Unmodeled {
        device_unique_id: Some(device.clone()),
        platform: ObjectId::try_from(platform).ok()?,
        name: Name::try_from(name.trim()).ok(),
        reason: None,
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
    fn a_select_offers_the_devices_options() {
        let device = UniqueId::try_from("00:11:22:33:44:55").expect("valid");
        let listed = ListEntitiesSelectResponse {
            key: 4,
            name: "Power-on behaviour".into(),
            options: vec!["off".into(), "on".into(), "previous".into()],
            entity_category: 1,
            ..Default::default()
        };
        let described = select(&device, &listed).expect("valid");
        assert_eq!(described.unique_id.as_str(), "00:11:22:33:44:55-select-4");
        assert_eq!(
            described.capabilities,
            Capabilities::Select(SelectCapabilities {
                options: vec!["off".into(), "on".into(), "previous".into()]
            })
        );
        let reported = SelectStateResponse {
            key: 4,
            state: "previous".into(),
            ..Default::default()
        };
        assert_eq!(
            select_state(&reported),
            Some(State::Select(SelectState {
                option: "previous".into()
            }))
        );
    }

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
            fan_state(&state, &known),
            State::Fan(FanState {
                on: true,
                percentage: Some(66),
                oscillating: Some(true),
                direction: None,
                preset_mode: Some("sleep".into()),
            })
        );
        let request = fan_command(
            5,
            &Service::FanSetPercentage(FanPercentage { percentage: 100 }),
            Some(&known),
        );
        assert!(request.has_speed_level && request.speed_level == 3);
        let off = fan_command(5, &Service::FanTurnOff, Some(&known));
        assert!(off.has_state && !off.state);
    }

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
        let described = climate(&device, &listed).expect("valid");
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
        let Some(State::Climate(state)) =
            climate_state(&reported, caps, unit, ClimateReads::of(&listed))
        else {
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

        let warmer = climate_command(
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
        let on = climate_command(9, &Service::ClimateTurnOn, unit, caps.mode_to_turn_on(None));
        assert_eq!((on.has_mode, on.mode), (true, 3));
        let custom = climate_command(
            9,
            &Service::ClimateSetPresetMode(irori_protocol::types::ClimatePresetMode {
                preset_mode: "holiday".into(),
            }),
            unit,
            None,
        );
        assert!(custom.has_custom_preset && !custom.has_preset);
    }

    #[test]
    fn a_siren_is_told_its_tone_volume_and_duration() {
        let request = siren_command(
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
        let off = siren_command(7, &Service::SirenTurnOff);
        assert!(off.has_state && !off.state && !off.has_tone);
    }

    #[test]
    fn a_valve_reports_and_is_sent_like_a_cover() {
        let zone = ValveCapabilities {
            device_class: Some(ValveClass::Water),
            position: true,
            stop: false,
        };
        let opening = ValveStateResponse {
            key: 6,
            position: 0.3,
            current_operation: 1,
            ..Default::default()
        };
        assert_eq!(
            valve_state(&opening, &zone),
            State::Valve(ValveState {
                state: OpenState::Opening,
                position: Some(30),
            })
        );
        let close = valve_command(6, &Service::ValveClose);
        assert!(close.has_position && close.position == 0.0);
    }

    #[test]
    fn a_lock_reports_its_state_and_takes_its_code() {
        let jammed = LockStateResponse {
            key: 4,
            state: 3,
            ..Default::default()
        };
        assert_eq!(
            lock_state(&jammed),
            Some(State::Lock(LockState {
                state: LockStatus::Jammed
            }))
        );
        let nothing = LockStateResponse {
            key: 4,
            state: 0,
            ..Default::default()
        };
        assert_eq!(lock_state(&nothing), None);
        let unlock = lock_command(
            4,
            &Service::LockUnlock(LockCode {
                code: Some("1234".into()),
            }),
        );
        assert_eq!((unlock.command, unlock.has_code), (0, true));
        assert_eq!(unlock.code, "1234");
    }

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
            cover_state(&moving, &blind),
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
            cover_state(&shut, &garage),
            State::Cover(CoverState {
                state: OpenState::Closed,
                position: None,
                tilt: None,
            })
        );
        let open = cover_command(3, &Service::CoverOpen);
        assert!(open.has_position && (open.position - 1.0).abs() < f32::EPSILON);
        assert!(cover_command(3, &Service::CoverStop).stop);
    }

    #[test]
    fn an_event_lists_what_can_happen() {
        let device = UniqueId::try_from("00:11:22:33:44:55").expect("valid");
        let listed = ListEntitiesEventResponse {
            key: 9,
            name: "Doorbell".into(),
            event_types: vec!["ring".into()],
            device_class: "doorbell".into(),
            ..Default::default()
        };
        let described = event(&device, &listed).expect("valid");
        assert_eq!(
            described.capabilities,
            Capabilities::Event(EventCapabilities {
                event_types: vec!["ring".into()],
                device_class: Some(EventClass::Doorbell),
            })
        );
        let rang = EventResponse {
            key: 9,
            event_type: "ring".into(),
            ..Default::default()
        };
        assert_eq!(
            event_state(&rang),
            State::Event(EventState {
                event_type: "ring".into()
            })
        );
    }

    #[test]
    fn a_button_says_what_it_does() {
        let device = UniqueId::try_from("00:11:22:33:44:55").expect("valid");
        let listed = ListEntitiesButtonResponse {
            key: 8,
            name: "Restart".into(),
            device_class: "restart".into(),
            entity_category: 1,
            ..Default::default()
        };
        let described = button(&device, &listed).expect("valid");
        assert_eq!(described.unique_id.as_str(), "00:11:22:33:44:55-button-8");
        assert_eq!(
            described.capabilities,
            Capabilities::Button(ButtonCapabilities {
                device_class: Some(ButtonClass::Restart)
            })
        );
    }

    #[test]
    fn a_text_keeps_its_lengths_and_hides_a_password() {
        let device = UniqueId::try_from("00:11:22:33:44:55").expect("valid");
        let listed = ListEntitiesTextResponse {
            key: 7,
            name: "Wi-Fi password".into(),
            min_length: 8,
            max_length: 64,
            mode: 1,
            ..Default::default()
        };
        let Capabilities::Text(caps) = text(&device, &listed).expect("valid").capabilities else {
            panic!("a text");
        };
        assert_eq!((caps.min_length, caps.max_length), (8, 64));
        assert_eq!(caps.mode, TextMode::Password);
        let long = ListEntitiesTextResponse {
            max_length: 1000,
            ..listed
        };
        let Capabilities::Text(caps) = text(&device, &long).expect("valid").capabilities else {
            panic!("a text");
        };
        assert_eq!(caps.max_length, 255, "Irori holds at most 255 characters");
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
