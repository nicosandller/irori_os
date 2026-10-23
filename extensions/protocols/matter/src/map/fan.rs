//! A fan, or an air purifier's: Fan Control (0x0202). Following Home Assistant's `fan.py`:
//! the speed is a percentage the fan is written, its mode says whether it's on, and `auto` and
//! the wind modes are presets.

use serde_json::json;

use crate::model::{Command, Node, cluster};
use irori_types::{FanCapabilities, FanDirection, FanState, Service, State};

/// Fan Control's feature bits.
mod feature {
    pub const MULTI_SPEED: u64 = 1;
    pub const AUTO: u64 = 2;
    pub const ROCKING: u64 = 4;
    pub const WIND: u64 = 8;
    pub const AIRFLOW_DIRECTION: u64 = 32;
}

const FAN_MODE: u32 = 0x0000;
const FAN_MODE_SEQUENCE: u32 = 0x0001;
const PERCENT_SETTING: u32 = 0x0002;
const PERCENT_CURRENT: u32 = 0x0003;
const SPEED_MAX: u32 = 0x0004;
const ROCK_SUPPORT: u32 = 0x0007;
const ROCK_SETTING: u32 = 0x0008;
const WIND_SUPPORT: u32 = 0x0009;
const WIND_SETTING: u32 = 0x000A;
const AIRFLOW_DIRECTION: u32 = 0x000B;

/// `FanMode`'s values.
const OFF: u64 = 0;
const ON: u64 = 4;
const AUTO: u64 = 5;

const PRESET_AUTO: &str = "auto";
const PRESET_SLEEP_WIND: &str = "sleep_wind";
const PRESET_NATURAL_WIND: &str = "natural_wind";

pub(super) fn capabilities(node: &Node, endpoint: u16) -> FanCapabilities {
    let features = node.features(endpoint, cluster::FAN_CONTROL);
    let read = |attribute| {
        node.value(endpoint, cluster::FAN_CONTROL, attribute)
            .and_then(serde_json::Value::as_u64)
    };
    // Without MultiSpeed, the speeds its FanModeSequence names: low, medium and high, or two.
    let speed_count = if features & feature::MULTI_SPEED != 0 {
        read(SPEED_MAX).and_then(|max| u16::try_from(max).ok())
    } else {
        read(FAN_MODE_SEQUENCE).map(|sequence| match sequence {
            0 | 2 => 3,
            1 | 3 => 2,
            _ => 1,
        })
    }
    .unwrap_or(100);
    let mut preset_modes = Vec::new();
    if features & feature::AUTO != 0 {
        preset_modes.push(PRESET_AUTO.to_owned());
    }
    if features & feature::WIND != 0 {
        let winds = read(WIND_SUPPORT).unwrap_or(0);
        if winds & 1 != 0 {
            preset_modes.push(PRESET_SLEEP_WIND.to_owned());
        }
        if winds & 2 != 0 {
            preset_modes.push(PRESET_NATURAL_WIND.to_owned());
        }
    }
    FanCapabilities {
        speed_count,
        oscillate: features & feature::ROCKING != 0,
        direction: features & feature::AIRFLOW_DIRECTION != 0,
        preset_modes,
    }
}

pub(super) fn state(node: &Node, endpoint: u16, known: &FanCapabilities) -> Option<State> {
    let read = |attribute| {
        node.value(endpoint, cluster::FAN_CONTROL, attribute)
            .and_then(serde_json::Value::as_u64)
    };
    let mode = read(FAN_MODE)?;
    // How fast it runs, as Home Assistant reads it; what it was set to while that's unknown.
    // A percentage past 100 is a device saying it doesn't know.
    let percentage = read(PERCENT_CURRENT)
        .filter(|percent| *percent <= 100)
        .or_else(|| read(PERCENT_SETTING))
        .filter(|percent| *percent <= 100)
        .and_then(|percent| u8::try_from(percent).ok());
    let wind = read(WIND_SETTING).unwrap_or(0);
    let preset_mode = if mode == AUTO && known.preset_modes.iter().any(|p| p == PRESET_AUTO) {
        Some(PRESET_AUTO)
    } else if wind & 2 != 0 {
        Some(PRESET_NATURAL_WIND)
    } else if wind & 1 != 0 {
        Some(PRESET_SLEEP_WIND)
    } else {
        None
    }
    .filter(|preset| known.preset_modes.iter().any(|p| p == preset))
    .map(str::to_owned);
    // On when it turns, or runs a preset; a fan in a speed mode at 0 % is standing still.
    let on = mode != OFF && (percentage.is_none_or(|percent| percent > 0) || preset_mode.is_some());
    Some(State::Fan(FanState {
        on,
        percentage,
        oscillating: known
            .oscillate
            .then(|| read(ROCK_SETTING).unwrap_or(0) != 0),
        direction: known
            .direction
            .then(|| read(AIRFLOW_DIRECTION))
            .flatten()
            .map(|direction| {
                if direction == 1 {
                    FanDirection::Reverse
                } else {
                    FanDirection::Forward
                }
            }),
        preset_mode,
    }))
}

pub(super) fn command(
    node: &Node,
    endpoint: u16,
    service: &Service,
) -> Result<Vec<Command>, String> {
    let write = |attribute, value| Command::write(endpoint, cluster::FAN_CONTROL, attribute, value);
    let preset = |name: &str| -> Result<Command, String> {
        match name {
            PRESET_AUTO => Ok(write(FAN_MODE, json!(AUTO))),
            PRESET_SLEEP_WIND => Ok(write(WIND_SETTING, json!(1))),
            PRESET_NATURAL_WIND => Ok(write(WIND_SETTING, json!(2))),
            other => Err(format!("it has no preset `{other}`")),
        }
    };
    Ok(match service {
        Service::FanTurnOn(on) => match (&on.preset_mode, on.percentage) {
            (Some(name), _) => vec![preset(name)?],
            (None, Some(percent)) => vec![write(PERCENT_SETTING, json!(percent))],
            (None, None) => vec![write(FAN_MODE, json!(ON))],
        },
        Service::FanTurnOff => vec![write(FAN_MODE, json!(OFF))],
        Service::FanSetPercentage(set) => vec![write(PERCENT_SETTING, json!(set.percentage))],
        Service::FanSetPresetMode(set) => vec![preset(&set.preset_mode)?],
        Service::FanOscillate(set) => {
            // Rocks every way it can, or not at all.
            let support = node
                .value(endpoint, cluster::FAN_CONTROL, ROCK_SUPPORT)
                .and_then(serde_json::Value::as_u64)
                .filter(|support| *support != 0)
                .unwrap_or(1);
            vec![write(
                ROCK_SETTING,
                json!(if set.oscillating { support } else { 0 }),
            )]
        }
        Service::FanSetDirection(set) => vec![write(
            AIRFLOW_DIRECTION,
            json!(match set.direction {
                FanDirection::Forward => 0,
                FanDirection::Reverse => 1,
            }),
        )],
        other => return Err(format!("a fan doesn't take {}", other.name())),
    })
}
