//! `water_heater`: a boiler, a hot water tank or a heat pump's cylinder. Its operation mode is
//! what automations read; temperatures are °C ([`crate::units`]).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Typed};
use crate::{Service, ServiceName};

use crate::InvariantError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaterHeaterCapabilities {
    /// The modes it can be put in, at least one.
    #[schemars(length(min = 1))]
    pub operation_modes: Vec<WaterHeaterMode>,
    /// The lowest target it takes, in °C.
    pub min_temp: f64,
    /// The highest target it takes, in °C.
    pub max_temp: f64,
    /// How finely a target can be set, in °C.
    pub temp_step: f64,
    /// Takes a target temperature.
    #[serde(default)]
    pub target_temperature: bool,
    /// Has its own on and off, apart from its modes. While it's off its mode reads `off`, even
    /// when `off` isn't one of its `operation_modes`.
    #[serde(default)]
    pub on_off: bool,
}

impl WaterHeaterCapabilities {
    /// Deserialization of an entity runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.operation_modes.is_empty() {
            return Err(InvariantError(
                "a water heater has at least one mode".into(),
            ));
        }
        super::climate::validate_temperatures(
            self.min_temp,
            self.max_temp,
            Some(self.temp_step),
            "its",
        )
    }

    /// The mode `turn_on` picks: the one it was in last if that wasn't `off`, otherwise its
    /// first mode that isn't.
    pub fn mode_to_turn_on(&self, last: Option<WaterHeaterMode>) -> Option<WaterHeaterMode> {
        last.filter(|mode| *mode != WaterHeaterMode::Off && self.operation_modes.contains(mode))
            .or_else(|| {
                self.operation_modes
                    .iter()
                    .copied()
                    .find(|mode| *mode != WaterHeaterMode::Off)
            })
    }
}

/// Home Assistant's operation modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WaterHeaterMode {
    Off,
    Eco,
    Electric,
    Gas,
    HeatPump,
    HighDemand,
    Performance,
}

/// Every text a water heater's primary value can be, for rules to check against.
pub(crate) static OPERATION_MODES: std::sync::LazyLock<Vec<String>> =
    std::sync::LazyLock::new(|| {
        WATER_HEATER_MODES
            .iter()
            .map(|m| m.as_str().to_owned())
            .collect()
    });

const WATER_HEATER_MODES: [WaterHeaterMode; 7] = [
    WaterHeaterMode::Off,
    WaterHeaterMode::Eco,
    WaterHeaterMode::Electric,
    WaterHeaterMode::Gas,
    WaterHeaterMode::HeatPump,
    WaterHeaterMode::HighDemand,
    WaterHeaterMode::Performance,
];

impl WaterHeaterMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Eco => "eco",
            Self::Electric => "electric",
            Self::Gas => "gas",
            Self::HeatPump => "heat_pump",
            Self::HighDemand => "high_demand",
            Self::Performance => "performance",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        super::from_ha(text)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaterHeaterState {
    pub operation_mode: WaterHeaterMode,
    /// The water's temperature, in °C.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_temperature: Option<f64>,
    /// In °C.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_temperature: Option<f64>,
}

impl WaterHeaterState {
    /// Deserialization runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        for value in [self.current_temperature, self.target_temperature]
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

/// Data for `water_heater.set_temperature`, in °C; optionally the mode to switch to with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaterHeaterSetTemperature {
    pub temperature: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_mode: Option<WaterHeaterMode>,
}

/// Data for `water_heater.set_operation_mode`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaterHeaterOperationMode {
    pub operation_mode: WaterHeaterMode,
}

pub(crate) fn supports_mode(
    caps: &WaterHeaterCapabilities,
    mode: WaterHeaterMode,
) -> Result<(), String> {
    if caps.operation_modes.contains(&mode) {
        Ok(())
    } else {
        Err(format!(
            "has the modes {}, not {:?}",
            caps.operation_modes
                .iter()
                .map(|m| m.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            mode.as_str()
        ))
    }
}

pub(crate) fn supports_temperature(
    caps: &WaterHeaterCapabilities,
    data: &WaterHeaterSetTemperature,
) -> Result<(), String> {
    if !caps.target_temperature {
        return Err("has no target temperature".into());
    }
    if let Some(mode) = data.operation_mode {
        supports_mode(caps, mode)?;
    }
    if (caps.min_temp..=caps.max_temp).contains(&data.temperature) {
        Ok(())
    } else {
        Err(format!(
            "takes a temperature from {} to {} °C, not {} °C",
            caps.min_temp, caps.max_temp, data.temperature
        ))
    }
}

pub(crate) fn supports_turn_off(caps: &WaterHeaterCapabilities) -> Result<(), String> {
    if caps.on_off {
        return Ok(());
    }
    supports_mode(caps, WaterHeaterMode::Off).map_err(|_| "can't be turned off".into())
}

pub(crate) fn supports_turn_on(caps: &WaterHeaterCapabilities) -> Result<(), String> {
    match caps.mode_to_turn_on(None) {
        _ if caps.on_off => Ok(()),
        Some(_) => Ok(()),
        None => Err("has no mode to turn on to".into()),
    }
}

pub(crate) fn fits(caps: &WaterHeaterCapabilities, state: &WaterHeaterState) -> Result<(), String> {
    if caps.on_off && state.operation_mode == WaterHeaterMode::Off {
        return Ok(());
    }
    supports_mode(caps, state.operation_mode).map_err(|what| format!("it {what}"))
}

pub(crate) fn primary(state: &WaterHeaterState) -> Typed {
    Typed::Text(state.operation_mode.as_str().to_owned())
}

/// Keeps its temperatures as it said them last.
pub(crate) fn with_primary(
    previous: Option<&WaterHeaterState>,
    value: &Typed,
) -> Option<WaterHeaterState> {
    let operation_mode = match value {
        Typed::Text(text) => WaterHeaterMode::parse(text)?,
        _ => return None,
    };
    Some(match previous {
        Some(heater) => WaterHeaterState {
            operation_mode,
            ..heater.clone()
        },
        None => WaterHeaterState {
            operation_mode,
            current_temperature: None,
            target_temperature: None,
        },
    })
}

/// Turns it off when it's in any mode but off, on otherwise.
pub(crate) fn toggle(current: Option<&Typed>) -> ServiceName {
    match current {
        Some(Typed::Text(text)) if WaterHeaterMode::parse(text) != Some(WaterHeaterMode::Off) => {
            ServiceName::WaterHeaterTurnOff
        }
        _ => ServiceName::WaterHeaterTurnOn,
    }
}

pub(crate) fn data_of(name: ServiceName) -> Data {
    match name {
        ServiceName::WaterHeaterSetTemperature | ServiceName::WaterHeaterSetOperationMode => {
            Data::Required
        }
        _ => Data::None,
    }
}

pub(crate) fn service(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    Ok(match name {
        ServiceName::WaterHeaterSetTemperature => {
            Service::WaterHeaterSetTemperature(super::parse(name, data)?)
        }
        ServiceName::WaterHeaterSetOperationMode => {
            Service::WaterHeaterSetOperationMode(super::parse(name, data)?)
        }
        ServiceName::WaterHeaterTurnOn => Service::WaterHeaterTurnOn,
        ServiceName::WaterHeaterTurnOff => Service::WaterHeaterTurnOff,
        _ => return Err(super::not_mine(name)),
    })
}

/// Which mode `turn_on` lands in is the device's to say.
pub(crate) fn asks_for(service: &Service) -> Option<Typed> {
    let mode = match service {
        Service::WaterHeaterSetOperationMode(WaterHeaterOperationMode { operation_mode })
        | Service::WaterHeaterSetTemperature(WaterHeaterSetTemperature {
            operation_mode: Some(operation_mode),
            ..
        }) => *operation_mode,
        Service::WaterHeaterTurnOff => WaterHeaterMode::Off,
        _ => return None,
    };
    Some(Typed::Text(mode.as_str().to_owned()))
}

pub(crate) fn supports_service(
    caps: &WaterHeaterCapabilities,
    service: &Service,
) -> Result<(), String> {
    match service {
        Service::WaterHeaterSetTemperature(data) => supports_temperature(caps, data),
        Service::WaterHeaterSetOperationMode(data) => supports_mode(caps, data.operation_mode),
        Service::WaterHeaterTurnOn => supports_turn_on(caps),
        Service::WaterHeaterTurnOff => supports_turn_off(caps),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_water_heater_takes_what_it_says_it_can() {
        let tank = WaterHeaterCapabilities {
            operation_modes: vec![WaterHeaterMode::Off, WaterHeaterMode::Eco],
            min_temp: 40.0,
            max_temp: 65.0,
            temp_step: 1.0,
            target_temperature: true,
            on_off: false,
        };
        let hot = WaterHeaterSetTemperature {
            temperature: 55.0,
            operation_mode: None,
        };
        assert!(supports_temperature(&tank, &hot).is_ok());
        let scalding = WaterHeaterSetTemperature {
            temperature: 80.0,
            operation_mode: None,
        };
        assert!(supports_temperature(&tank, &scalding).is_err());
        assert!(supports_mode(&tank, WaterHeaterMode::Gas).is_err());
        assert_eq!(tank.mode_to_turn_on(None), Some(WaterHeaterMode::Eco));
        assert_eq!(
            WaterHeaterMode::parse("heat_pump"),
            Some(WaterHeaterMode::HeatPump)
        );
        let switched = WaterHeaterCapabilities {
            operation_modes: vec![WaterHeaterMode::Eco],
            on_off: true,
            ..tank
        };
        assert!(supports_turn_off(&switched).is_ok());
        let off = WaterHeaterState {
            operation_mode: WaterHeaterMode::Off,
            current_temperature: None,
            target_temperature: None,
        };
        assert!(fits(&switched, &off).is_ok(), "off by its own switch");
    }
}
