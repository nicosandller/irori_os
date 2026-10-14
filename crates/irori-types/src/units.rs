//! Temperatures in the model are °C (`docs/specs/entities.md` §4.4): protocols convert what a
//! device says in °F or K on the way in, and back on the way out. Pages show °C for now.
//! Otherwise `num('sensor.outside') < num('climate.hall', 'target_temperature')` could compare
//! °F with °C without anyone noticing.

use std::collections::BTreeMap;

use crate::{AttributeKey, SensorClass};

/// A temperature unit a device can speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemperatureUnit {
    Celsius,
    Fahrenheit,
    Kelvin,
}

impl TemperatureUnit {
    /// The unit a device's text names: `°C`, `C`, `°F`, `F`, `K` (also `℃`/`℉`). `None` for
    /// anything that isn't a temperature unit.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "°C" | "C" | "℃" | "celsius" | "Celsius" => Some(Self::Celsius),
            "°F" | "F" | "℉" | "fahrenheit" | "Fahrenheit" => Some(Self::Fahrenheit),
            "K" | "kelvin" | "Kelvin" => Some(Self::Kelvin),
            _ => None,
        }
    }

    /// As a device writes it: `°F`.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Celsius => "°C",
            Self::Fahrenheit => "°F",
            Self::Kelvin => "K",
        }
    }

    /// `value` in this unit, as °C.
    pub fn to_celsius(self, value: f64) -> f64 {
        match self {
            Self::Celsius => value,
            Self::Fahrenheit => (value - 32.0) * 5.0 / 9.0,
            Self::Kelvin => value - 273.15,
        }
    }

    /// `celsius` in this unit.
    pub fn from_celsius(self, celsius: f64) -> f64 {
        match self {
            Self::Celsius => celsius,
            Self::Fahrenheit => celsius * 9.0 / 5.0 + 32.0,
            Self::Kelvin => celsius + 273.15,
        }
    }

    /// A step or a difference in this unit, as °C: 1 °F is 5/9 °C, with no offset.
    pub fn step_to_celsius(self, step: f64) -> f64 {
        match self {
            Self::Fahrenheit => step * 5.0 / 9.0,
            Self::Celsius | Self::Kelvin => step,
        }
    }
}

/// How a temperature sensor's readings, or a temperature setting's value and range, become °C
/// on the way in and go back on the way out. Only for entities that aren't in °C already.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conversion {
    unit: TemperatureUnit,
    /// A difference between two temperatures (a calibration offset), which has no zero to move.
    difference: bool,
}

impl Conversion {
    /// What the model says once converted.
    pub const UNIT: &'static str = "°C";

    /// The attribute a protocol sets on a converted entity's reports, saying the unit its
    /// device speaks (`docs/specs/entities.md` §5.4).
    pub const NATIVE_UNIT: &'static str = "native_unit";

    /// The unit the device speaks, as it writes it.
    pub fn native_unit(self) -> &'static str {
        self.unit.symbol()
    }

    /// The attributes for a report from an entity with this conversion: its native unit, or
    /// none when nothing is converted.
    pub fn attributes(conversion: Option<Self>) -> BTreeMap<AttributeKey, serde_json::Value> {
        conversion
            .and_then(|conversion| {
                let key = AttributeKey::try_from(Self::NATIVE_UNIT).ok()?;
                Some((key, serde_json::json!(conversion.native_unit())))
            })
            .into_iter()
            .collect()
    }

    /// The conversion as `value * scale + offset`, for what was stored before it applied.
    pub fn linear(self) -> (f64, f64) {
        match (self.unit, self.difference) {
            (TemperatureUnit::Fahrenheit, false) => (5.0 / 9.0, -160.0 / 9.0),
            (TemperatureUnit::Fahrenheit, true) => (5.0 / 9.0, 0.0),
            (TemperatureUnit::Kelvin, false) => (1.0, -273.15),
            (TemperatureUnit::Kelvin, true) | (TemperatureUnit::Celsius, _) => (1.0, 0.0),
        }
    }

    /// The conversion for an entity of `class` that says `unit`. `None` when there's nothing
    /// to convert: it isn't a temperature, is in °C already, or names a unit that isn't one.
    pub fn of(class: Option<SensorClass>, unit: Option<&str>) -> Option<Self> {
        let difference = match class? {
            SensorClass::Temperature => false,
            SensorClass::TemperatureDelta => true,
            _ => return None,
        };
        let unit = TemperatureUnit::parse(unit?)?;
        (unit != TemperatureUnit::Celsius && !(difference && unit == TemperatureUnit::Kelvin))
            .then_some(Self { unit, difference })
    }

    /// The device's value, as the model keeps it: °C, to a hundredth.
    pub fn to_model(self, value: f64) -> f64 {
        round_to(
            if self.difference {
                self.unit.step_to_celsius(value)
            } else {
                self.unit.to_celsius(value)
            },
            2,
        )
    }

    /// The model's value, as the device takes it.
    pub fn from_model(self, celsius: f64) -> f64 {
        round_to(
            if self.difference {
                celsius / self.unit.step_to_celsius(1.0)
            } else {
                self.unit.from_celsius(celsius)
            },
            4,
        )
    }

    /// A step in the device's unit, as °C.
    pub fn step_to_model(self, step: f64) -> f64 {
        round_to(self.unit.step_to_celsius(step), 4)
    }
}

/// `value` rounded to `places` decimals, so a converted reading doesn't show 21.111111.
pub fn round_to(value: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (value * scale).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_temperatures_that_arent_celsius_are_converted() {
        let of = |class, unit| Conversion::of(Some(class), Some(unit));
        assert_eq!(of(SensorClass::Temperature, "°C"), None);
        assert_eq!(of(SensorClass::Humidity, "°F"), None);
        assert_eq!(of(SensorClass::Temperature, "lx"), None);
        assert_eq!(Conversion::of(None, Some("°F")), None);

        let outside = of(SensorClass::Temperature, "°F").expect("converted");
        assert_eq!(outside.to_model(70.0), 21.11);
        assert_eq!(outside.from_model(20.0), 68.0);
        assert_eq!(outside.step_to_model(1.0), 0.5556);
        let (scale, offset) = outside.linear();
        assert_eq!(round_to(70.0 * scale + offset, 2), outside.to_model(70.0));
        assert_eq!(
            Conversion::attributes(Some(outside))
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect::<Vec<_>>(),
            [("native_unit".to_owned(), serde_json::json!("°F"))]
        );
        assert!(Conversion::attributes(None).is_empty());

        // An offset of 2 °F is 1.11 °C, not −16.67: a difference has no zero to move.
        let offset = of(SensorClass::TemperatureDelta, "°F").expect("converted");
        assert_eq!(offset.to_model(2.0), 1.11);
        assert_eq!(offset.from_model(1.0), 1.8);
        // A kelvin of difference is a degree Celsius of difference.
        assert_eq!(of(SensorClass::TemperatureDelta, "K"), None);
    }

    #[test]
    fn temperatures_cross_to_celsius_and_back() {
        let f = TemperatureUnit::parse("°F").expect("a unit");
        assert_eq!(f.to_celsius(212.0), 100.0);
        assert_eq!(round_to(f.to_celsius(70.0), 2), 21.11);
        assert_eq!(round_to(f.from_celsius(f.to_celsius(68.5)), 6), 68.5);
        assert_eq!(TemperatureUnit::Kelvin.to_celsius(273.15), 0.0);
        assert_eq!(round_to(f.step_to_celsius(1.0), 3), 0.556);
        assert_eq!(TemperatureUnit::parse("lx"), None);
    }
}
