//! ESPHome's `water_heater`. Temperatures become °C.

use esphome_client::types::{
    ListEntitiesWaterHeaterResponse, WaterHeaterCommandRequest, WaterHeaterStateResponse,
};
use irori_protocol::ProtocolError;
use irori_protocol::types::units::{TemperatureUnit, round_to};
use irori_protocol::types::{
    Capabilities, EntityDescription, EntityKind, Name, Service, State, UniqueId,
    WaterHeaterCapabilities, WaterHeaterMode, WaterHeaterState,
};

use super::{category, celsius, entity_id, temperature_unit};

/// ESPHome's `WaterHeaterMode`, by number.
const WATER_HEATER_MODES: [(i32, WaterHeaterMode); 7] = [
    (0, WaterHeaterMode::Off),
    (1, WaterHeaterMode::Eco),
    (2, WaterHeaterMode::Electric),
    (3, WaterHeaterMode::Performance),
    (4, WaterHeaterMode::HighDemand),
    (5, WaterHeaterMode::HeatPump),
    (6, WaterHeaterMode::Gas),
];

/// ESPHome's `WaterHeaterFeature` bits, and its state and command bits.
mod water_heater_bits {
    pub const SUPPORTS_CURRENT_TEMPERATURE: u32 = 1;
    pub const SUPPORTS_ON_OFF: u32 = 1 << 4;
    /// In a state or command's `state`.
    pub const ON: u32 = 1 << 1;
    /// In a command's `has_fields`.
    pub const HAS_MODE: u32 = 1;
    pub const HAS_TARGET_TEMPERATURE: u32 = 2;
    pub const HAS_ON_STATE: u32 = 32;
}

/// ESPHome's `water_heater`. Temperatures become °C.
pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesWaterHeaterResponse,
) -> Result<EntityDescription, ProtocolError> {
    let unit = temperature_unit(entity.temperature_unit);
    let modes: Vec<WaterHeaterMode> = WATER_HEATER_MODES
        .iter()
        .filter(|(number, _)| entity.supported_modes.contains(number))
        .map(|(_, mode)| *mode)
        .collect();
    let step = if entity.target_temperature_step > 0.0 {
        f64::from(entity.target_temperature_step)
    } else {
        1.0
    };
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::WaterHeater, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::WaterHeater(WaterHeaterCapabilities {
            operation_modes: if modes.is_empty() {
                vec![WaterHeaterMode::Off]
            } else {
                modes
            },
            min_temp: celsius(unit, entity.min_temperature),
            max_temp: celsius(unit, entity.max_temperature),
            temp_step: round_to(unit.step_to_celsius(step), 2),
            // Home Assistant always offers a water heater from ESPHome a target.
            target_temperature: true,
            on_off: entity.supported_features & water_heater_bits::SUPPORTS_ON_OFF != 0,
        }),
        entity_category: category(entity.entity_category),
    })
}

/// A water heater's state, in °C. One switched off by its own switch reads `off`.
pub fn state(
    state: &WaterHeaterStateResponse,
    known: &WaterHeaterCapabilities,
    unit: TemperatureUnit,
    reads_current: bool,
) -> Option<State> {
    let mode = WATER_HEATER_MODES
        .iter()
        .find(|(number, _)| *number == state.mode)
        .map(|(_, mode)| *mode)?;
    let switched_off = known.on_off && state.state & water_heater_bits::ON == 0;
    let temperature = |value: f32| (!value.is_nan()).then(|| celsius(unit, value));
    Some(State::WaterHeater(WaterHeaterState {
        operation_mode: if switched_off {
            WaterHeaterMode::Off
        } else {
            mode
        },
        current_temperature: if reads_current {
            temperature(state.current_temperature)
        } else {
            None
        },
        target_temperature: temperature(state.target_temperature),
    }))
}

/// Whether a water heater said it reports the water's temperature.
pub fn reads_current(entity: &ListEntitiesWaterHeaterResponse) -> bool {
    entity.supported_features & water_heater_bits::SUPPORTS_CURRENT_TEMPERATURE != 0
}

/// A water heater command, with temperatures in the device's `unit`. `turn_on` uses its own
/// switch when it has one, otherwise goes to `on_mode`.
pub fn command(
    key: u32,
    service: &Service,
    known: Option<&WaterHeaterCapabilities>,
    unit: TemperatureUnit,
    on_mode: Option<WaterHeaterMode>,
) -> WaterHeaterCommandRequest {
    use water_heater_bits::{HAS_MODE, HAS_ON_STATE, HAS_TARGET_TEMPERATURE, ON};
    let switch = known.is_some_and(|known| known.on_off);
    let number = |mode: WaterHeaterMode| {
        WATER_HEATER_MODES
            .iter()
            .find(|(_, m)| *m == mode)
            .map_or(0, |(number, _)| *number)
    };
    let mut request = WaterHeaterCommandRequest {
        key,
        ..Default::default()
    };
    let set_mode = |request: &mut WaterHeaterCommandRequest, mode: WaterHeaterMode| {
        request.has_fields |= HAS_MODE;
        request.mode = number(mode);
    };
    match service {
        Service::WaterHeaterSetOperationMode(data) => set_mode(&mut request, data.operation_mode),
        Service::WaterHeaterSetTemperature(data) => {
            if let Some(mode) = data.operation_mode {
                set_mode(&mut request, mode);
            }
            request.has_fields |= HAS_TARGET_TEMPERATURE;
            #[allow(clippy::cast_possible_truncation)] // a temperature fits an f32
            let target = unit.from_celsius(data.temperature) as f32;
            request.target_temperature = target;
        }
        Service::WaterHeaterTurnOn if switch => {
            request.has_fields |= HAS_ON_STATE;
            request.state = ON;
        }
        Service::WaterHeaterTurnOff if switch => request.has_fields |= HAS_ON_STATE,
        Service::WaterHeaterTurnOn => {
            if let Some(mode) = on_mode {
                set_mode(&mut request, mode);
            }
        }
        Service::WaterHeaterTurnOff => set_mode(&mut request, WaterHeaterMode::Off),
        _ => {}
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_water_heater_with_its_own_switch_reads_off_while_switched_off() {
        let device = UniqueId::try_from("aa:bb").expect("valid");
        let listed = esphome_client::types::ListEntitiesWaterHeaterResponse {
            key: 4,
            name: "Tank".into(),
            min_temperature: 40.0,
            max_temperature: 65.0,
            target_temperature_step: 0.5,
            supported_modes: vec![1, 5],
            // Current temperature and on/off.
            supported_features: 1 | 16,
            ..Default::default()
        };
        let described = describe(&device, &listed).expect("valid");
        let Capabilities::WaterHeater(caps) = &described.capabilities else {
            panic!("a water heater");
        };
        assert_eq!(
            caps.operation_modes,
            vec![WaterHeaterMode::Eco, WaterHeaterMode::HeatPump]
        );
        assert!(caps.on_off && caps.target_temperature);
        let unit = temperature_unit(listed.temperature_unit);
        let reported = WaterHeaterStateResponse {
            key: 4,
            mode: 5,
            current_temperature: 48.5,
            target_temperature: 55.0,
            state: 0,
            ..Default::default()
        };
        let Some(State::WaterHeater(off)) = state(&reported, caps, unit, true) else {
            panic!("a water heater state");
        };
        assert_eq!(off.operation_mode, WaterHeaterMode::Off);
        assert_eq!(off.current_temperature, Some(48.5));
        assert_eq!(
            Capabilities::WaterHeater(caps.clone()).fits(&State::WaterHeater(off)),
            Ok(())
        );
        let on = command(4, &Service::WaterHeaterTurnOn, Some(caps), unit, None);
        assert_eq!((on.has_fields, on.state), (32, 2));
        let eco = command(
            4,
            &Service::WaterHeaterSetTemperature(irori_protocol::types::WaterHeaterSetTemperature {
                temperature: 50.0,
                operation_mode: Some(WaterHeaterMode::Eco),
            }),
            Some(caps),
            unit,
            None,
        );
        assert_eq!((eco.has_fields, eco.mode), (1 | 2, 1));
        assert!((eco.target_temperature - 50.0).abs() < f32::EPSILON);
    }
}
