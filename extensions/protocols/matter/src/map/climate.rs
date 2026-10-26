//! A thermostat: Thermostat (0x0201). Following Home Assistant's `climate.py`: the modes come
//! from its features, a mode and a setpoint are attributes it's written, and temperatures are
//! in hundredths of a degree.

use serde_json::json;

use crate::model::{Command, Node, cluster};
use irori_types::{ClimateCapabilities, ClimateState, HvacAction, HvacMode, Service, State};

/// Thermostat's feature bits.
mod feature {
    pub const HEATING: u64 = 1;
    pub const COOLING: u64 = 2;
    pub const AUTO_MODE: u64 = 32;
}

const LOCAL_TEMPERATURE: u32 = 0x0000;
const ABS_MIN_HEAT: u32 = 0x0003;
const ABS_MAX_HEAT: u32 = 0x0004;
const ABS_MIN_COOL: u32 = 0x0005;
const ABS_MAX_COOL: u32 = 0x0006;
const COOLING_SETPOINT: u32 = 0x0011;
const HEATING_SETPOINT: u32 = 0x0012;
const MIN_HEAT: u32 = 0x0015;
const MAX_HEAT: u32 = 0x0016;
const MIN_COOL: u32 = 0x0017;
const MAX_COOL: u32 = 0x0018;
const CONTROL_SEQUENCE: u32 = 0x001B;
const SYSTEM_MODE: u32 = 0x001C;
const RUNNING_STATE: u32 = 0x0029;

/// What Irori shows when the thermostat gives no limits, as Home Assistant does.
const DEFAULT_MIN: f64 = 7.0;
const DEFAULT_MAX: f64 = 35.0;

pub(super) fn capabilities(node: &Node, endpoint: u16) -> ClimateCapabilities {
    let features = node.features(endpoint, cluster::THERMOSTAT);
    let read = |attribute| {
        node.number(endpoint, cluster::THERMOSTAT, attribute)
            .map(|hundredths| hundredths / 100.0)
    };
    // ControlSequenceOfOperation narrows what the features allow: 0-1 cool only, 2-3 heat only.
    let sequence = node.number(endpoint, cluster::THERMOSTAT, CONTROL_SEQUENCE);
    let heats = features & feature::HEATING != 0 && !matches!(sequence, Some(s) if s < 2.0);
    let cools =
        features & feature::COOLING != 0 && !matches!(sequence, Some(s) if (2.0..4.0).contains(&s));
    let mut hvac_modes = vec![HvacMode::Off];
    if heats {
        hvac_modes.push(HvacMode::Heat);
    }
    if cools {
        hvac_modes.push(HvacMode::Cool);
    }
    let range = heats && cools && features & feature::AUTO_MODE != 0;
    if range {
        hvac_modes.push(HvacMode::HeatCool);
    }
    let first = |attributes: &[u32]| attributes.iter().find_map(|&attribute| read(attribute));
    let min_temp = if heats {
        first(&[MIN_HEAT, ABS_MIN_HEAT])
    } else {
        first(&[MIN_COOL, ABS_MIN_COOL])
    }
    .unwrap_or(DEFAULT_MIN);
    let max_temp = if cools {
        first(&[MAX_COOL, ABS_MAX_COOL])
    } else {
        first(&[MAX_HEAT, ABS_MAX_HEAT])
    }
    .unwrap_or(DEFAULT_MAX);
    ClimateCapabilities {
        hvac_modes,
        min_temp: min_temp.min(max_temp),
        max_temp: max_temp.max(min_temp),
        temp_step: 0.5,
        target_temperature: heats || cools,
        target_temperature_range: range,
        target_humidity: None,
        fan_modes: Vec::new(),
        swing_modes: Vec::new(),
        preset_modes: Vec::new(),
    }
}

/// `SystemMode`, as Home Assistant reads it: auto is heating or cooling to stay in a range.
fn mode(system_mode: u64) -> Option<HvacMode> {
    Some(match system_mode {
        0 => HvacMode::Off,
        1 => HvacMode::HeatCool,
        3 | 6 => HvacMode::Cool,
        4 | 5 => HvacMode::Heat,
        7 => HvacMode::FanOnly,
        8 => HvacMode::Dry,
        _ => return None,
    })
}

fn system_mode(mode: HvacMode) -> u8 {
    match mode {
        HvacMode::Off => 0,
        HvacMode::HeatCool | HvacMode::Auto => 1,
        HvacMode::Cool => 3,
        HvacMode::Heat => 4,
        HvacMode::FanOnly => 7,
        HvacMode::Dry => 8,
    }
}

pub(super) fn state(node: &Node, endpoint: u16, known: &ClimateCapabilities) -> Option<State> {
    let read = |attribute| {
        node.number(endpoint, cluster::THERMOSTAT, attribute)
            .map(|hundredths| (hundredths).round() / 100.0)
    };
    let hvac_mode = mode(
        node.value(endpoint, cluster::THERMOSTAT, SYSTEM_MODE)?
            .as_u64()?,
    )?;
    // ThermostatRunningState: heat, cool and fan, each with second stages.
    let hvac_action = node
        .value(endpoint, cluster::THERMOSTAT, RUNNING_STATE)
        .and_then(serde_json::Value::as_u64)
        .map(|running| match running {
            _ if hvac_mode == HvacMode::Off => HvacAction::Off,
            r if r & 0b0_1001 != 0 => HvacAction::Heating,
            r if r & 0b1_0010 != 0 => HvacAction::Cooling,
            r if r & 0b110_0100 != 0 => HvacAction::Fan,
            _ => HvacAction::Idle,
        });
    let mut state = ClimateState::in_mode(hvac_mode);
    state.hvac_action = hvac_action;
    state.current_temperature = read(LOCAL_TEMPERATURE);
    let heating = read(HEATING_SETPOINT);
    let cooling = read(COOLING_SETPOINT);
    match hvac_mode {
        HvacMode::HeatCool if known.target_temperature_range => {
            state.target_temp_low = heating;
            state.target_temp_high = cooling;
        }
        HvacMode::Cool => state.target_temperature = cooling,
        _ if known.target_temperature => {
            state.target_temperature = if known.hvac_modes.contains(&HvacMode::Heat) {
                heating
            } else {
                cooling
            };
        }
        _ => {}
    }
    Some(State::Climate(state))
}

/// A temperature as the thermostat takes it, in hundredths of a degree.
fn hundredths(celsius: f64) -> i64 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a room's temperature, far inside i64"
    )]
    let value = (celsius * 100.0).round() as i64;
    value
}

/// What a call writes. `current` is its mode now, and `last_on` the mode it was last in that
/// wasn't off, for `turn_on`.
pub(super) fn command(
    endpoint: u16,
    known: &ClimateCapabilities,
    current: Option<HvacMode>,
    last_on: Option<HvacMode>,
    service: &Service,
) -> Result<Vec<Command>, String> {
    let write = |attribute, value| Command::write(endpoint, cluster::THERMOSTAT, attribute, value);
    let set_mode = |mode| write(SYSTEM_MODE, json!(system_mode(mode)));
    Ok(match service {
        Service::ClimateSetHvacMode(set) => vec![set_mode(set.hvac_mode)],
        Service::ClimateTurnOff => vec![set_mode(HvacMode::Off)],
        Service::ClimateTurnOn => {
            let mode = known
                .mode_to_turn_on(last_on)
                .ok_or("it has no mode to turn on to")?;
            vec![set_mode(mode)]
        }
        Service::ClimateSetTemperature(set) => {
            let mut commands = Vec::new();
            if let Some(mode) = set.hvac_mode {
                commands.push(set_mode(mode));
            }
            let mode = set.hvac_mode.or(current);
            if let Some(temperature) = set.temperature {
                let cools =
                    mode == Some(HvacMode::Cool) || !known.hvac_modes.contains(&HvacMode::Heat);
                let attribute = if cools {
                    COOLING_SETPOINT
                } else {
                    HEATING_SETPOINT
                };
                commands.push(write(attribute, json!(hundredths(temperature))));
            }
            if let Some(low) = set.target_temp_low {
                commands.push(write(HEATING_SETPOINT, json!(hundredths(low))));
            }
            if let Some(high) = set.target_temp_high {
                commands.push(write(COOLING_SETPOINT, json!(hundredths(high))));
            }
            commands
        }
        other => return Err(format!("a thermostat doesn't take {}", other.name())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::{ClimateHvacMode, ClimateSetTemperature};

    fn caps(modes: &[HvacMode]) -> ClimateCapabilities {
        ClimateCapabilities {
            hvac_modes: modes.to_vec(),
            min_temp: 7.0,
            max_temp: 30.0,
            temp_step: 0.5,
            target_temperature: true,
            target_temperature_range: modes.contains(&HvacMode::HeatCool),
            target_humidity: None,
            fan_modes: Vec::new(),
            swing_modes: Vec::new(),
            preset_modes: Vec::new(),
        }
    }

    #[test]
    fn a_setpoint_is_written_in_hundredths_to_the_one_the_mode_uses() {
        let both = caps(&[
            HvacMode::Off,
            HvacMode::Heat,
            HvacMode::Cool,
            HvacMode::HeatCool,
        ]);
        let set = |temperature, hvac_mode| {
            Service::ClimateSetTemperature(ClimateSetTemperature {
                temperature: Some(temperature),
                hvac_mode,
                ..ClimateSetTemperature::default()
            })
        };
        assert_eq!(
            command(1, &both, Some(HvacMode::Heat), None, &set(21.5, None)),
            Ok(vec![Command::write(
                1,
                cluster::THERMOSTAT,
                HEATING_SETPOINT,
                json!(2150)
            )])
        );
        assert_eq!(
            command(
                1,
                &both,
                Some(HvacMode::Heat),
                None,
                &set(24.0, Some(HvacMode::Cool))
            ),
            Ok(vec![
                Command::write(1, cluster::THERMOSTAT, SYSTEM_MODE, json!(3)),
                Command::write(1, cluster::THERMOSTAT, COOLING_SETPOINT, json!(2400)),
            ])
        );
        let range = Service::ClimateSetTemperature(ClimateSetTemperature {
            target_temp_low: Some(19.0),
            target_temp_high: Some(25.0),
            ..ClimateSetTemperature::default()
        });
        assert_eq!(
            command(1, &both, Some(HvacMode::HeatCool), None, &range),
            Ok(vec![
                Command::write(1, cluster::THERMOSTAT, HEATING_SETPOINT, json!(1900)),
                Command::write(1, cluster::THERMOSTAT, COOLING_SETPOINT, json!(2500)),
            ])
        );
    }

    #[test]
    fn turning_on_goes_back_to_the_last_mode() {
        let both = caps(&[HvacMode::Off, HvacMode::Heat, HvacMode::Cool]);
        assert_eq!(
            command(
                1,
                &both,
                Some(HvacMode::Off),
                Some(HvacMode::Cool),
                &Service::ClimateTurnOn
            ),
            Ok(vec![Command::write(
                1,
                cluster::THERMOSTAT,
                SYSTEM_MODE,
                json!(3)
            )])
        );
        assert_eq!(
            command(
                1,
                &both,
                None,
                None,
                &Service::ClimateSetHvacMode(ClimateHvacMode {
                    hvac_mode: HvacMode::Off
                })
            ),
            Ok(vec![Command::write(
                1,
                cluster::THERMOSTAT,
                SYSTEM_MODE,
                json!(0)
            )])
        );
    }
}
