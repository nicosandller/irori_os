//! `number`: a value a person (or a rule) sets within a range, e.g. a motion sensor's timeout, a
//! thermostat's calibration offset, a speaker's volume. Usually one of a device's settings.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Typed};
use crate::{Service, ServiceName};

use super::sensor::SensorClass;
use crate::InvariantError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NumberCapabilities {
    /// The lowest value it takes.
    pub min: f64,
    /// The highest value it takes. Not below `min`.
    pub max: f64,
    /// The smallest change that means anything, e.g. `0.5`. Above zero.
    pub step: f64,
    /// Unit of the value, e.g. `s`, `°C`, `%`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// What it measures, from the same list as a sensor's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_class: Option<SensorClass>,
    /// How a page should offer it.
    #[serde(default, skip_serializing_if = "NumberMode::is_auto")]
    pub mode: NumberMode,
}

/// How a page offers a number: a slider, a box to type in, or whichever suits the range.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NumberMode {
    #[default]
    Auto,
    Slider,
    Box,
}

impl NumberMode {
    #[allow(clippy::trivially_copy_pass_by_ref)] // serde's `skip_serializing_if` passes a reference
    fn is_auto(&self) -> bool {
        *self == Self::Auto
    }
}

impl NumberCapabilities {
    /// Deserialization of an entity runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if !(self.min.is_finite() && self.max.is_finite() && self.step.is_finite()) {
            return Err(InvariantError(
                "a number's min, max and step must be finite".into(),
            ));
        }
        if self.min > self.max {
            return Err(InvariantError(format!(
                "a number's min {} is above its max {}",
                self.min, self.max
            )));
        }
        if self.step <= 0.0 {
            return Err(InvariantError(format!(
                "a number's step must be above zero, not {}",
                self.step
            )));
        }
        Ok(())
    }

    /// Whether it can be `value`: finite and within its range. A device that rounds to its step
    /// does so itself; the step is a hint for pages, not a rule.
    fn takes(&self, value: f64) -> Result<(), String> {
        if !value.is_finite() {
            return Err(format!("{value} isn't a number it can have"));
        }
        if value < self.min || value > self.max {
            return Err(format!(
                "goes from {} to {}, not {value}",
                number(self.min),
                number(self.max)
            ));
        }
        Ok(())
    }
}

/// `21` rather than `21.0`, as people write it.
fn number(n: f64) -> String {
    let text = n.to_string();
    text.strip_suffix(".0").map_or(text.clone(), str::to_owned)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NumberState {
    pub value: f64,
}

impl NumberState {
    /// JSON can't carry NaN or infinity, but code can build them; they'd fail to serialize.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.value.is_finite() {
            Ok(())
        } else {
            Err(InvariantError(format!(
                "number value {} is not a finite number",
                self.value
            )))
        }
    }
}

/// Data for `number.set_value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NumberSetValue {
    /// Within the number's `min` and `max`.
    pub value: f64,
}

impl NumberSetValue {
    /// Deserialization of a service call runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.value.is_finite() {
            Ok(())
        } else {
            Err(InvariantError(format!(
                "value {} is not a finite number",
                self.value
            )))
        }
    }
}

/// Whether a number can be asked for this value. `Err` follows the entity's name.
pub(crate) fn supports(caps: &NumberCapabilities, data: &NumberSetValue) -> Result<(), String> {
    caps.takes(data.value)
}

/// Whether a reported value is one this number can have.
pub(crate) fn fits(caps: &NumberCapabilities, state: &NumberState) -> Result<(), String> {
    caps.takes(state.value).map_err(|what| format!("it {what}"))
}

pub(crate) fn primary(state: &NumberState) -> Typed {
    Typed::Number(state.value)
}

pub(crate) fn with_primary(value: &Typed) -> Option<NumberState> {
    match value {
        Typed::Number(value) => Some(NumberState { value: *value }),
        _ => None,
    }
}

pub(crate) fn data_of(_: ServiceName) -> Data {
    Data::Required
}

pub(crate) fn service(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    match name {
        ServiceName::NumberSetValue => Ok(Service::NumberSetValue(super::parse(name, data)?)),
        _ => Err(super::not_mine(name)),
    }
}

pub(crate) fn asks_for(service: &Service) -> Option<Typed> {
    match service {
        Service::NumberSetValue(data) => Some(Typed::Number(data.value)),
        _ => None,
    }
}

pub(crate) fn supports_service(caps: &NumberCapabilities, service: &Service) -> Result<(), String> {
    match service {
        Service::NumberSetValue(data) => supports(caps, data),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeout() -> NumberCapabilities {
        NumberCapabilities {
            min: 5.0,
            max: 600.0,
            step: 5.0,
            unit: Some("s".into()),
            device_class: Some(SensorClass::Duration),
            mode: NumberMode::Box,
        }
    }

    #[test]
    fn a_number_takes_only_values_in_its_range() {
        let caps = timeout();
        assert!(supports(&caps, &NumberSetValue { value: 60.0 }).is_ok());
        assert_eq!(
            supports(&caps, &NumberSetValue { value: 900.0 }),
            Err("goes from 5 to 600, not 900".to_owned())
        );
        assert_eq!(
            fits(&caps, &NumberState { value: 1.0 }),
            Err("it goes from 5 to 600, not 1".to_owned())
        );
    }

    #[test]
    fn a_range_has_to_make_sense() {
        let backwards = NumberCapabilities {
            min: 10.0,
            max: 1.0,
            ..timeout()
        };
        assert!(backwards.validate().is_err());
        let still = NumberCapabilities {
            step: 0.0,
            ..timeout()
        };
        assert!(still.validate().is_err());
        assert!(timeout().validate().is_ok());
    }
}
