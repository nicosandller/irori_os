//! `climate`: something that heats or cools a room, e.g. a thermostat, a radiator valve or an
//! air conditioner. Its mode is what automations read; temperatures are °C
//! ([`crate::units`]).

use std::sync::LazyLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Typed};
use crate::{Service, ServiceName};

use crate::InvariantError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClimateCapabilities {
    /// The modes it can be put in, at least one.
    #[schemars(length(min = 1))]
    pub hvac_modes: Vec<HvacMode>,
    /// The lowest target it takes, in °C.
    pub min_temp: f64,
    /// The highest target it takes, in °C.
    pub max_temp: f64,
    /// How finely a target can be set, in °C, e.g. 0.5.
    pub temp_step: f64,
    /// Takes one target temperature.
    #[serde(default)]
    pub target_temperature: bool,
    /// Takes a range: heat below the low end, cool above the high one.
    #[serde(default)]
    pub target_temperature_range: bool,
    /// Takes a target humidity, within this range in %.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_humidity: Option<HumidityRange>,
    /// Its fan's modes, e.g. `auto`, `low`, `high`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 256))]
    pub fan_modes: Vec<String>,
    /// How its louvres swing, e.g. `off`, `vertical`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 256))]
    pub swing_modes: Vec<String>,
    /// Presets, e.g. `eco`, `away`, `boost`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 256))]
    pub preset_modes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HumidityRange {
    #[schemars(range(min = 0, max = 100))]
    pub min: f64,
    #[schemars(range(min = 0, max = 100))]
    pub max: f64,
}

impl HumidityRange {
    pub(crate) fn validate(&self, whose: &str) -> Result<(), InvariantError> {
        if !(0.0..=100.0).contains(&self.min)
            || !(0.0..=100.0).contains(&self.max)
            || self.min > self.max
        {
            return Err(InvariantError(format!(
                "{whose} humidity range is {}-{}%, which isn't within 0-100%",
                self.min, self.max
            )));
        }
        Ok(())
    }

    pub(crate) fn takes(&self, humidity: f64) -> Result<(), String> {
        if (self.min..=self.max).contains(&humidity) {
            Ok(())
        } else {
            Err(format!(
                "takes a humidity from {}% to {}%, not {humidity}%",
                self.min, self.max
            ))
        }
    }
}

/// Checks a temperature range from a device, as an entity's deserialization does.
pub(crate) fn validate_temperatures(
    min: f64,
    max: f64,
    step: Option<f64>,
    whose: &str,
) -> Result<(), InvariantError> {
    if !(min.is_finite() && max.is_finite()) || min > max {
        return Err(InvariantError(format!(
            "{whose} temperatures go from {min} to {max} °C, which isn't a range"
        )));
    }
    if let Some(step) = step
        && !(step.is_finite() && step > 0.0)
    {
        return Err(InvariantError(format!(
            "{whose} temperature step must be above zero, not {step}"
        )));
    }
    Ok(())
}

impl ClimateCapabilities {
    /// Deserialization of an entity runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.hvac_modes.is_empty() {
            return Err(InvariantError(
                "a climate entity has at least one mode".into(),
            ));
        }
        validate_temperatures(self.min_temp, self.max_temp, Some(self.temp_step), "its")?;
        if let Some(range) = &self.target_humidity {
            range.validate("its target")?;
        }
        for list in [&self.fan_modes, &self.swing_modes, &self.preset_modes] {
            super::sensor::validate_options(list)?;
        }
        Ok(())
    }

    /// The mode `turn_on` picks: the one it was in last if that wasn't `off`, otherwise its
    /// first mode that isn't. `None` when every mode is `off`.
    pub fn mode_to_turn_on(&self, last: Option<HvacMode>) -> Option<HvacMode> {
        super::mode_to_turn_on(&self.hvac_modes, HvacMode::Off, last)
    }
}

/// What it's set to do: Home Assistant's modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HvacMode {
    Off,
    Heat,
    Cool,
    /// Heats or cools to stay within a range.
    HeatCool,
    /// Follows its own schedule or logic.
    Auto,
    Dry,
    FanOnly,
}

/// Every text a climate entity's primary value can be, for rules to check against.
pub(crate) static CLIMATE_MODES: LazyLock<Vec<String>> =
    LazyLock::new(|| HVAC_MODES.iter().map(|m| m.as_str().to_owned()).collect());

const HVAC_MODES: [HvacMode; 7] = [
    HvacMode::Off,
    HvacMode::Heat,
    HvacMode::Cool,
    HvacMode::HeatCool,
    HvacMode::Auto,
    HvacMode::Dry,
    HvacMode::FanOnly,
];

impl HvacMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Heat => "heat",
            Self::Cool => "cool",
            Self::HeatCool => "heat_cool",
            Self::Auto => "auto",
            Self::Dry => "dry",
            Self::FanOnly => "fan_only",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        super::from_ha(text)
    }
}

/// What it's doing right now, which can differ from its mode: set to heat, but idle because
/// the room is warm enough.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HvacAction {
    Off,
    Preheating,
    Heating,
    Cooling,
    Drying,
    Idle,
    Fan,
    Defrosting,
}

impl HvacAction {
    pub fn parse(text: &str) -> Option<Self> {
        super::from_ha(text)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClimateState {
    pub hvac_mode: HvacMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hvac_action: Option<HvacAction>,
    /// The room's temperature, as it measures it, in °C.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_temperature: Option<f64>,
    /// In °C, for one that takes a single target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_temperature: Option<f64>,
    /// In °C, for one that takes a range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_temp_low: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_temp_high: Option<f64>,
    /// In %.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_humidity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_humidity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fan_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swing_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_mode: Option<String>,
}

impl ClimateState {
    /// A state with only its mode known.
    pub fn in_mode(hvac_mode: HvacMode) -> Self {
        Self {
            hvac_mode,
            hvac_action: None,
            current_temperature: None,
            target_temperature: None,
            target_temp_low: None,
            target_temp_high: None,
            current_humidity: None,
            target_humidity: None,
            fan_mode: None,
            swing_mode: None,
            preset_mode: None,
        }
    }

    /// Deserialization runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        for (what, value) in [
            ("current_temperature", self.current_temperature),
            ("target_temperature", self.target_temperature),
            ("target_temp_low", self.target_temp_low),
            ("target_temp_high", self.target_temp_high),
        ] {
            if let Some(value) = value
                && !value.is_finite()
            {
                return Err(InvariantError(format!(
                    "{what} {value} isn't a temperature"
                )));
            }
        }
        for (what, value) in [
            ("current_humidity", self.current_humidity),
            ("target_humidity", self.target_humidity),
        ] {
            if let Some(value) = value
                && !(0.0..=100.0).contains(&value)
            {
                return Err(InvariantError(format!("{what} is 0-100%, not {value}")));
            }
        }
        Ok(())
    }
}

/// Data for `climate.set_hvac_mode`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClimateHvacMode {
    pub hvac_mode: HvacMode,
}

/// Data for `climate.set_temperature`: a single target, or a range, in °C; optionally the mode
/// to switch to at the same time.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClimateSetTemperature {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_temp_low: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_temp_high: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hvac_mode: Option<HvacMode>,
}

impl ClimateSetTemperature {
    /// Deserialization of a call runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        match (
            self.temperature,
            self.target_temp_low,
            self.target_temp_high,
        ) {
            (Some(_), None, None) => {}
            (None, Some(low), Some(high)) if low <= high => {}
            (None, Some(low), Some(high)) => {
                return Err(InvariantError(format!(
                    "target_temp_low {low} is above target_temp_high {high}"
                )));
            }
            _ => {
                return Err(InvariantError(
                    "set_temperature takes `temperature`, or both `target_temp_low` and \
                     `target_temp_high`"
                        .into(),
                ));
            }
        }
        for value in [
            self.temperature,
            self.target_temp_low,
            self.target_temp_high,
        ]
        .into_iter()
        .flatten()
        {
            if !value.is_finite() {
                return Err(InvariantError(format!("{value} isn't a temperature")));
            }
        }
        Ok(())
    }
}

/// Data for `climate.set_humidity` and `humidifier.set_humidity`, in %.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetHumidity {
    #[schemars(range(min = 0, max = 100))]
    pub humidity: f64,
}

impl SetHumidity {
    /// Deserialization runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if (0.0..=100.0).contains(&self.humidity) {
            Ok(())
        } else {
            Err(InvariantError(format!(
                "humidity is 0-100%, not {}",
                self.humidity
            )))
        }
    }
}

/// Data for `climate.set_fan_mode`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClimateFanMode {
    pub fan_mode: String,
}

/// Data for `climate.set_swing_mode`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClimateSwingMode {
    pub swing_mode: String,
}

/// Data for `climate.set_preset_mode`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClimatePresetMode {
    pub preset_mode: String,
}

/// `text` as one of `list`, which is called `what` (`fan modes`). `Err` follows the entity's
/// name.
fn one_of(list: &[String], text: &str, what: &str) -> Result<(), String> {
    if list.iter().any(|item| item == text) {
        Ok(())
    } else if list.is_empty() {
        Err(format!("has no {what}"))
    } else {
        Err(format!("has the {what} {}, not {text:?}", list.join(", ")))
    }
}

pub(crate) fn supports_mode(caps: &ClimateCapabilities, mode: HvacMode) -> Result<(), String> {
    if caps.hvac_modes.contains(&mode) {
        Ok(())
    } else {
        Err(format!(
            "has the modes {}, not {:?}",
            caps.hvac_modes
                .iter()
                .map(|m| m.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            mode.as_str()
        ))
    }
}

pub(crate) fn supports_turn_off(caps: &ClimateCapabilities) -> Result<(), String> {
    supports_mode(caps, HvacMode::Off).map_err(|_| "can't be turned off".into())
}

pub(crate) fn supports_turn_on(caps: &ClimateCapabilities) -> Result<(), String> {
    match caps.mode_to_turn_on(None) {
        Some(_) => Ok(()),
        None => Err("has no mode to turn on to".into()),
    }
}

fn takes_temperature(caps: &ClimateCapabilities, value: f64) -> Result<(), String> {
    if (caps.min_temp..=caps.max_temp).contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "takes a temperature from {} to {} °C, not {value} °C",
            caps.min_temp, caps.max_temp
        ))
    }
}

pub(crate) fn supports_temperature(
    caps: &ClimateCapabilities,
    data: &ClimateSetTemperature,
) -> Result<(), String> {
    if let Some(mode) = data.hvac_mode {
        supports_mode(caps, mode)?;
    }
    if let Some(value) = data.temperature {
        if !caps.target_temperature {
            return Err(if caps.target_temperature_range {
                "takes a range, `target_temp_low` and `target_temp_high`, not one temperature"
                    .into()
            } else {
                "has no target temperature".into()
            });
        }
        takes_temperature(caps, value)?;
    }
    if let (Some(low), Some(high)) = (data.target_temp_low, data.target_temp_high) {
        if !caps.target_temperature_range {
            return Err(if caps.target_temperature {
                "takes one `temperature`, not a range".into()
            } else {
                "has no target temperature".into()
            });
        }
        takes_temperature(caps, low)?;
        takes_temperature(caps, high)?;
    }
    Ok(())
}

pub(crate) fn supports_humidity(caps: &ClimateCapabilities, humidity: f64) -> Result<(), String> {
    match &caps.target_humidity {
        Some(range) => range.takes(humidity),
        None => Err("has no target humidity".into()),
    }
}

pub(crate) fn supports_fan_mode(caps: &ClimateCapabilities, mode: &str) -> Result<(), String> {
    one_of(&caps.fan_modes, mode, "fan modes")
}

pub(crate) fn supports_swing_mode(caps: &ClimateCapabilities, mode: &str) -> Result<(), String> {
    one_of(&caps.swing_modes, mode, "swing modes")
}

pub(crate) fn supports_preset(caps: &ClimateCapabilities, mode: &str) -> Result<(), String> {
    one_of(&caps.preset_modes, mode, "presets")
}

/// Whether a reported state is one this climate entity can be in. Targets outside its range are
/// let through: the device is the judge of what it's set to.
pub(crate) fn fits(caps: &ClimateCapabilities, state: &ClimateState) -> Result<(), String> {
    supports_mode(caps, state.hvac_mode).map_err(|what| format!("it {what}"))?;
    for (mode, check) in [
        (&state.fan_mode, supports_fan_mode as fn(&_, &_) -> _),
        (&state.swing_mode, supports_swing_mode),
        (&state.preset_mode, supports_preset),
    ] {
        if let Some(mode) = mode {
            check(caps, mode).map_err(|what| format!("it {what}"))?;
        }
    }
    Ok(())
}

pub(crate) fn primary(state: &ClimateState) -> Typed {
    Typed::Text(state.hvac_mode.as_str().to_owned())
}

/// Keeps its targets and readings as it said them last.
pub(crate) fn with_primary(previous: Option<&ClimateState>, value: &Typed) -> Option<ClimateState> {
    let hvac_mode = match value {
        Typed::Text(text) => HvacMode::parse(text)?,
        _ => return None,
    };
    Some(match previous {
        Some(climate) => ClimateState {
            hvac_mode,
            ..climate.clone()
        },
        None => ClimateState::in_mode(hvac_mode),
    })
}

/// Turns it off when it's in any mode but off, on otherwise.
pub(crate) fn toggle(current: Option<&Typed>) -> ServiceName {
    match current {
        Some(Typed::Text(text)) if HvacMode::parse(text) != Some(HvacMode::Off) => {
            ServiceName::ClimateTurnOff
        }
        _ => ServiceName::ClimateTurnOn,
    }
}

pub(crate) fn data_of(name: ServiceName) -> Data {
    match name {
        ServiceName::ClimateTurnOn | ServiceName::ClimateTurnOff => Data::None,
        _ => Data::Required,
    }
}

pub(crate) fn service(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    use super::parse;
    Ok(match name {
        ServiceName::ClimateSetHvacMode => Service::ClimateSetHvacMode(parse(name, data)?),
        ServiceName::ClimateSetTemperature => Service::ClimateSetTemperature(parse(name, data)?),
        ServiceName::ClimateSetHumidity => Service::ClimateSetHumidity(parse(name, data)?),
        ServiceName::ClimateSetFanMode => Service::ClimateSetFanMode(parse(name, data)?),
        ServiceName::ClimateSetSwingMode => Service::ClimateSetSwingMode(parse(name, data)?),
        ServiceName::ClimateSetPresetMode => Service::ClimateSetPresetMode(parse(name, data)?),
        ServiceName::ClimateTurnOn => Service::ClimateTurnOn,
        ServiceName::ClimateTurnOff => Service::ClimateTurnOff,
        _ => return Err(super::not_mine(name)),
    })
}

/// Which mode `turn_on` lands in is the device's to say, and a target or a fan mode leaves the
/// mode as it is.
pub(crate) fn asks_for(service: &Service) -> Option<Typed> {
    let mode = match service {
        Service::ClimateSetHvacMode(ClimateHvacMode { hvac_mode })
        | Service::ClimateSetTemperature(ClimateSetTemperature {
            hvac_mode: Some(hvac_mode),
            ..
        }) => *hvac_mode,
        Service::ClimateTurnOff => HvacMode::Off,
        _ => return None,
    };
    Some(Typed::Text(mode.as_str().to_owned()))
}

pub(crate) fn supports_service(
    caps: &ClimateCapabilities,
    service: &Service,
) -> Result<(), String> {
    match service {
        Service::ClimateSetHvacMode(data) => supports_mode(caps, data.hvac_mode),
        Service::ClimateSetTemperature(data) => supports_temperature(caps, data),
        Service::ClimateSetHumidity(data) => supports_humidity(caps, data.humidity),
        Service::ClimateSetFanMode(data) => supports_fan_mode(caps, &data.fan_mode),
        Service::ClimateSetSwingMode(data) => supports_swing_mode(caps, &data.swing_mode),
        Service::ClimateSetPresetMode(data) => supports_preset(caps, &data.preset_mode),
        Service::ClimateTurnOn => supports_turn_on(caps),
        Service::ClimateTurnOff => supports_turn_off(caps),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thermostat() -> ClimateCapabilities {
        ClimateCapabilities {
            hvac_modes: vec![HvacMode::Off, HvacMode::Heat, HvacMode::Auto],
            min_temp: 5.0,
            max_temp: 30.0,
            temp_step: 0.5,
            target_temperature: true,
            target_temperature_range: false,
            target_humidity: None,
            fan_modes: Vec::new(),
            swing_modes: Vec::new(),
            preset_modes: vec!["eco".into(), "away".into()],
        }
    }

    #[test]
    fn a_thermostat_takes_what_it_says_it_can() {
        let caps = thermostat();
        let warm = ClimateSetTemperature {
            temperature: Some(21.5),
            ..ClimateSetTemperature::default()
        };
        assert!(warm.validate().is_ok());
        assert!(supports_temperature(&caps, &warm).is_ok());
        let range = ClimateSetTemperature {
            target_temp_low: Some(19.0),
            target_temp_high: Some(24.0),
            ..ClimateSetTemperature::default()
        };
        assert_eq!(
            supports_temperature(&caps, &range),
            Err("takes one `temperature`, not a range".to_owned())
        );
        let hot = ClimateSetTemperature {
            temperature: Some(35.0),
            ..ClimateSetTemperature::default()
        };
        assert!(supports_temperature(&caps, &hot).is_err());
        assert!(supports_mode(&caps, HvacMode::Cool).is_err());
        assert!(supports_humidity(&caps, 40.0).is_err());
        assert!(ClimateSetTemperature::default().validate().is_err());
    }

    #[test]
    fn turning_on_goes_back_to_the_last_mode() {
        let caps = thermostat();
        assert_eq!(
            caps.mode_to_turn_on(Some(HvacMode::Auto)),
            Some(HvacMode::Auto)
        );
        assert_eq!(
            caps.mode_to_turn_on(Some(HvacMode::Off)),
            Some(HvacMode::Heat)
        );
        assert_eq!(
            caps.mode_to_turn_on(Some(HvacMode::Cool)),
            Some(HvacMode::Heat)
        );
        assert_eq!(caps.mode_to_turn_on(None), Some(HvacMode::Heat));
        let off_only = ClimateCapabilities {
            hvac_modes: vec![HvacMode::Off],
            ..thermostat()
        };
        assert!(supports_turn_on(&off_only).is_err());
    }

    #[test]
    fn a_report_in_a_mode_it_lacks_is_refused() {
        let caps = thermostat();
        assert!(fits(&caps, &ClimateState::in_mode(HvacMode::Heat)).is_ok());
        assert!(fits(&caps, &ClimateState::in_mode(HvacMode::Cool)).is_err());
        let mut boosted = ClimateState::in_mode(HvacMode::Heat);
        boosted.preset_mode = Some("boost".into());
        assert!(fits(&caps, &boosted).is_err());
        assert_eq!(HvacMode::parse("fan_only"), Some(HvacMode::FanOnly));
        assert_eq!(HvacMode::FanOnly.as_str(), "fan_only");
    }
}
