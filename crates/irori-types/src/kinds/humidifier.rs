//! `humidifier`: a humidifier or a dehumidifier: on or off, aiming for a humidity, sometimes with
//! modes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
