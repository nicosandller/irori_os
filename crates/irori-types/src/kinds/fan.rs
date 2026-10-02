//! `fan`: a fan, e.g. a ceiling fan or an air purifier's: on or off, and often a speed, a swing
//! and preset modes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Typed};
use crate::{Service, ServiceName};

use crate::InvariantError;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FanCapabilities {
    /// How many speeds it has, 0 when its speed can't be set. Speeds are set and reported as a
    /// percentage; this says how many steps there really are.
    #[serde(default)]
    pub speed_count: u16,
    /// Can swing from side to side.
    #[serde(default)]
    pub oscillate: bool,
    /// Can turn the other way.
    #[serde(default)]
    pub direction: bool,
    /// Modes beyond its speed, e.g. `auto`, `sleep`, `breeze`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 256))]
    pub preset_modes: Vec<String>,
}

impl FanCapabilities {
    /// Deserialization of an entity runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        super::sensor::validate_options(&self.preset_modes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FanDirection {
    Forward,
    Reverse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FanState {
    pub on: bool,
    /// Its speed, 1-100, when it has speeds. Kept while off: the speed it comes back on at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 100))]
    pub percentage: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oscillating: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<FanDirection>,
    /// One of its `preset_modes`, when it's in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_mode: Option<String>,
}

impl FanState {
    /// Deserialization runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        match self.percentage {
            Some(percentage) if percentage > 100 => Err(InvariantError(format!(
                "a fan's percentage is 0-100, not {percentage}"
            ))),
            _ => Ok(()),
        }
    }
}

/// Data for `fan.turn_on`: optionally, the speed or mode to come on in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FanTurnOn {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 100))]
    pub percentage: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_mode: Option<String>,
}

/// Data for `fan.set_percentage`. 0 turns it off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FanPercentage {
    #[schemars(range(max = 100))]
    pub percentage: u8,
}

/// Data for `fan.oscillate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FanOscillate {
    pub oscillating: bool,
}

/// Data for `fan.set_direction`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FanSetDirection {
    pub direction: FanDirection,
}

/// Data for `fan.set_preset_mode`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FanPresetMode {
    pub preset_mode: String,
}

/// Whether it can be set to `percentage`. `Err` follows the entity's name.
pub(crate) fn supports_percentage(caps: &FanCapabilities, percentage: u8) -> Result<(), String> {
    if caps.speed_count == 0 {
        return Err("has no speeds to set".into());
    }
    if percentage > 100 {
        return Err(format!("takes a speed from 0 to 100%, not {percentage}%"));
    }
    Ok(())
}

pub(crate) fn supports_preset(caps: &FanCapabilities, mode: &str) -> Result<(), String> {
    if caps.preset_modes.iter().any(|m| m == mode) {
        Ok(())
    } else if caps.preset_modes.is_empty() {
        Err("has no preset modes".into())
    } else {
        Err(format!(
            "has the modes {}, not {mode:?}",
            caps.preset_modes.join(", ")
        ))
    }
}

pub(crate) fn supports_turn_on(caps: &FanCapabilities, data: &FanTurnOn) -> Result<(), String> {
    if let Some(percentage) = data.percentage {
        supports_percentage(caps, percentage)?;
    }
    if let Some(mode) = &data.preset_mode {
        supports_preset(caps, mode)?;
    }
    Ok(())
}

pub(crate) fn supports_oscillate(caps: &FanCapabilities) -> Result<(), String> {
    if caps.oscillate {
        Ok(())
    } else {
        Err("doesn't swing".into())
    }
}

pub(crate) fn supports_direction(caps: &FanCapabilities) -> Result<(), String> {
    if caps.direction {
        Ok(())
    } else {
        Err("only turns one way".into())
    }
}

/// Whether a reported state is one this fan can be in.
pub(crate) fn fits(caps: &FanCapabilities, state: &FanState) -> Result<(), String> {
    if state.percentage.is_some() && caps.speed_count == 0 {
        return Err("it reports a speed, but said it has none".into());
    }
    if let Some(mode) = &state.preset_mode {
        supports_preset(caps, mode).map_err(|what| format!("it {what}"))?;
    }
    Ok(())
}

/// A percentage as the speed it really is, of `count`: 1-100% across `count` steps, rounded up so
/// any speed above 0 is at least the first. Home Assistant's own conversion.
pub fn percentage_to_speed(percentage: u8, count: u16) -> u16 {
    (u32::from(percentage) * u32::from(count))
        .div_ceil(100)
        .try_into()
        .unwrap_or(count)
}

/// A speed of `count` as a percentage, the other way, rounded down: speed 1 of 3 is 33%.
pub fn speed_to_percentage(speed: u16, count: u16) -> u8 {
    if count == 0 {
        return 0;
    }
    (u32::from(speed.min(count)) * 100 / u32::from(count))
        .try_into()
        .unwrap_or(100)
}

pub(crate) fn primary(state: &FanState) -> Typed {
    Typed::Bool(state.on)
}

/// Keeps its speed and settings as it said them last.
pub(crate) fn with_primary(previous: Option<&FanState>, value: &Typed) -> Option<FanState> {
    let Typed::Bool(on) = value else {
        return None;
    };
    Some(match previous {
        Some(fan) => FanState {
            on: *on,
            ..fan.clone()
        },
        None => FanState {
            on: *on,
            percentage: None,
            oscillating: None,
            direction: None,
            preset_mode: None,
        },
    })
}

pub(crate) fn toggle(current: Option<&Typed>) -> ServiceName {
    super::on_off_toggle(current, ServiceName::FanTurnOn, ServiceName::FanTurnOff)
}

pub(crate) fn data_of(name: ServiceName) -> Data {
    match name {
        ServiceName::FanTurnOn => Data::Optional,
        ServiceName::FanTurnOff => Data::None,
        _ => Data::Required,
    }
}

pub(crate) fn service(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    Ok(match name {
        ServiceName::FanTurnOn => Service::FanTurnOn(super::parse(name, data)?),
        ServiceName::FanTurnOff => Service::FanTurnOff,
        ServiceName::FanSetPercentage => Service::FanSetPercentage(super::parse(name, data)?),
        ServiceName::FanOscillate => Service::FanOscillate(super::parse(name, data)?),
        ServiceName::FanSetDirection => Service::FanSetDirection(super::parse(name, data)?),
        ServiceName::FanSetPresetMode => Service::FanSetPresetMode(super::parse(name, data)?),
        _ => return Err(super::not_mine(name)),
    })
}

/// Oscillation, direction and a preset leave on and off as they are.
pub(crate) fn asks_for(service: &Service) -> Option<Typed> {
    match service {
        Service::FanTurnOn(_) => Some(Typed::Bool(true)),
        Service::FanTurnOff => Some(Typed::Bool(false)),
        Service::FanSetPercentage(data) => Some(Typed::Bool(data.percentage > 0)),
        _ => None,
    }
}

pub(crate) fn supports_service(caps: &FanCapabilities, service: &Service) -> Result<(), String> {
    match service {
        Service::FanTurnOn(data) => supports_turn_on(caps, data),
        Service::FanSetPercentage(data) => supports_percentage(caps, data.percentage),
        Service::FanOscillate(_) => supports_oscillate(caps),
        Service::FanSetDirection(_) => supports_direction(caps),
        Service::FanSetPresetMode(data) => supports_preset(caps, &data.preset_mode),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speeds_and_percentages_cross_like_home_assistants() {
        // Three speeds: 33%, 66%, 100%.
        assert_eq!(speed_to_percentage(1, 3), 33);
        assert_eq!(speed_to_percentage(2, 3), 66);
        assert_eq!(speed_to_percentage(3, 3), 100);
        assert_eq!(percentage_to_speed(34, 3), 2);
        assert_eq!(percentage_to_speed(33, 3), 1);
        assert_eq!(
            percentage_to_speed(1, 3),
            1,
            "any speed is at least the first"
        );
        assert_eq!(percentage_to_speed(0, 3), 0);
    }

    #[test]
    fn a_fan_does_only_what_it_says_it_can() {
        let purifier = FanCapabilities {
            speed_count: 3,
            oscillate: false,
            direction: false,
            preset_modes: vec!["auto".into(), "sleep".into()],
        };
        assert!(supports_percentage(&purifier, 50).is_ok());
        assert_eq!(
            supports_preset(&purifier, "turbo"),
            Err(r#"has the modes auto, sleep, not "turbo""#.to_owned())
        );
        assert!(supports_oscillate(&purifier).is_err());
        assert!(supports_percentage(&FanCapabilities::default(), 50).is_err());
    }
}
