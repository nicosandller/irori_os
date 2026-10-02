//! `humidifier`: a humidifier or a dehumidifier: on or off, aiming for a humidity, sometimes with
//! modes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Typed};
use crate::{Service, ServiceName};

use super::climate::HumidityRange;
use crate::InvariantError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HumidifierCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_class: Option<HumidifierClass>,
    /// The target humidities it takes, in %.
    pub humidity: HumidityRange,
    /// Modes, e.g. `normal`, `eco`, `sleep`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 256))]
    pub modes: Vec<String>,
}

impl HumidifierCapabilities {
    /// Deserialization of an entity runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        self.humidity.validate("its")?;
        super::sensor::validate_options(&self.modes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HumidifierClass {
    Humidifier,
    Dehumidifier,
}

impl HumidifierClass {
    /// The class Home Assistant calls `name`.
    pub fn from_ha(name: &str) -> Option<Self> {
        super::from_ha(name)
    }
}

/// What it's doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HumidifierAction {
    Off,
    Humidifying,
    Drying,
    Idle,
}

impl HumidifierAction {
    pub fn parse(text: &str) -> Option<Self> {
        super::from_ha(text)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HumidifierState {
    pub on: bool,
    /// In %.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_humidity: Option<f64>,
    /// The room's humidity, as it measures it, in %.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_humidity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<HumidifierAction>,
}

impl HumidifierState {
    /// Deserialization runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        for (what, value) in [
            ("target_humidity", self.target_humidity),
            ("current_humidity", self.current_humidity),
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

/// Data for `humidifier.set_mode`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HumidifierMode {
    pub mode: String,
}

pub(crate) fn supports_humidity(
    caps: &HumidifierCapabilities,
    humidity: f64,
) -> Result<(), String> {
    caps.humidity.takes(humidity)
}

pub(crate) fn supports_mode(caps: &HumidifierCapabilities, mode: &str) -> Result<(), String> {
    if caps.modes.iter().any(|m| m == mode) {
        Ok(())
    } else if caps.modes.is_empty() {
        Err("has no modes".into())
    } else {
        Err(format!(
            "has the modes {}, not {mode:?}",
            caps.modes.join(", ")
        ))
    }
}

pub(crate) fn fits(caps: &HumidifierCapabilities, state: &HumidifierState) -> Result<(), String> {
    if let Some(mode) = &state.mode {
        supports_mode(caps, mode).map_err(|what| format!("it {what}"))?;
    }
    Ok(())
}

pub(crate) fn primary(state: &HumidifierState) -> Typed {
    Typed::Bool(state.on)
}

/// Keeps its target, reading and mode as it said them last.
pub(crate) fn with_primary(
    previous: Option<&HumidifierState>,
    value: &Typed,
) -> Option<HumidifierState> {
    let Typed::Bool(on) = value else {
        return None;
    };
    Some(match previous {
        Some(old) => HumidifierState {
            on: *on,
            ..old.clone()
        },
        None => HumidifierState {
            on: *on,
            target_humidity: None,
            current_humidity: None,
            mode: None,
            action: None,
        },
    })
}

pub(crate) fn toggle(current: Option<&Typed>) -> ServiceName {
    super::on_off_toggle(
        current,
        ServiceName::HumidifierTurnOn,
        ServiceName::HumidifierTurnOff,
    )
}

pub(crate) fn data_of(name: ServiceName) -> Data {
    match name {
        ServiceName::HumidifierSetHumidity | ServiceName::HumidifierSetMode => Data::Required,
        _ => Data::None,
    }
}

pub(crate) fn service(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    Ok(match name {
        ServiceName::HumidifierTurnOn => Service::HumidifierTurnOn,
        ServiceName::HumidifierTurnOff => Service::HumidifierTurnOff,
        ServiceName::HumidifierSetHumidity => {
            Service::HumidifierSetHumidity(super::parse(name, data)?)
        }
        ServiceName::HumidifierSetMode => Service::HumidifierSetMode(super::parse(name, data)?),
        _ => return Err(super::not_mine(name)),
    })
}

/// A target or a mode leaves on and off as they are.
pub(crate) fn asks_for(service: &Service) -> Option<Typed> {
    match service {
        Service::HumidifierTurnOn => Some(Typed::Bool(true)),
        Service::HumidifierTurnOff => Some(Typed::Bool(false)),
        _ => None,
    }
}

pub(crate) fn supports_service(
    caps: &HumidifierCapabilities,
    service: &Service,
) -> Result<(), String> {
    match service {
        Service::HumidifierSetHumidity(data) => supports_humidity(caps, data.humidity),
        Service::HumidifierSetMode(data) => supports_mode(caps, &data.mode),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dehumidifier_takes_what_it_says_it_can() {
        let dryer = HumidifierCapabilities {
            device_class: Some(HumidifierClass::Dehumidifier),
            humidity: HumidityRange {
                min: 35.0,
                max: 80.0,
            },
            modes: vec!["normal".into(), "sleep".into()],
        };
        assert!(supports_humidity(&dryer, 50.0).is_ok());
        assert!(supports_humidity(&dryer, 20.0).is_err());
        assert_eq!(
            supports_mode(&dryer, "turbo"),
            Err(r#"has the modes normal, sleep, not "turbo""#.to_owned())
        );
    }
}
