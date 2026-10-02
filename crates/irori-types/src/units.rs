//! Temperatures in the model are °C (`docs/specs/entities.md` §4.4): protocols convert what a
//! device says in °F or K on the way in, and back on the way out. Pages show °C for now.
//! Otherwise `num('sensor.outside') < num('climate.hall', 'target_temperature')` could compare
//! °F with °C without anyone noticing.

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

/// `value` rounded to `places` decimals, so a converted reading doesn't show 21.111111.
pub fn round_to(value: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (value * scale).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;

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
