//! `weather`: the weather where the home is, as a weather service says it is now. Home
//! Assistant's `weather`.
//!
//! Its value is the condition (`sunny`, `rainy`), in Home Assistant's words. The rest are
//! fields, each when the service says it, in Irori's units: °C, %, hPa, km/h, km and degrees.
//! Forecasts are not here yet (#142).

use std::sync::LazyLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Field, Typed};
use crate::InvariantError;

/// What a weather entity can do: say the weather. Nothing to say yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WeatherCapabilities {}

/// The weather's condition. Home Assistant's, in its spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum WeatherCondition {
    #[serde(rename = "clear-night")]
    ClearNight,
    #[serde(rename = "cloudy")]
    Cloudy,
    /// Something out of the ordinary: a storm warning, extreme heat.
    #[serde(rename = "exceptional")]
    Exceptional,
    #[serde(rename = "fog")]
    Fog,
    #[serde(rename = "hail")]
    Hail,
    #[serde(rename = "lightning")]
    Lightning,
    #[serde(rename = "lightning-rainy")]
    LightningRainy,
    #[serde(rename = "partlycloudy")]
    PartlyCloudy,
    #[serde(rename = "pouring")]
    Pouring,
    #[serde(rename = "rainy")]
    Rainy,
    #[serde(rename = "snowy")]
    Snowy,
    #[serde(rename = "snowy-rainy")]
    SnowyRainy,
    #[serde(rename = "sunny")]
    Sunny,
    #[serde(rename = "windy")]
    Windy,
    /// Windy and cloudy.
    #[serde(rename = "windy-variant")]
    WindyVariant,
}

impl WeatherCondition {
    pub const ALL: [Self; 15] = [
        Self::ClearNight,
        Self::Cloudy,
        Self::Exceptional,
        Self::Fog,
        Self::Hail,
        Self::Lightning,
        Self::LightningRainy,
        Self::PartlyCloudy,
        Self::Pouring,
        Self::Rainy,
        Self::Snowy,
        Self::SnowyRainy,
        Self::Sunny,
        Self::Windy,
        Self::WindyVariant,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClearNight => "clear-night",
            Self::Cloudy => "cloudy",
            Self::Exceptional => "exceptional",
            Self::Fog => "fog",
            Self::Hail => "hail",
            Self::Lightning => "lightning",
            Self::LightningRainy => "lightning-rainy",
            Self::PartlyCloudy => "partlycloudy",
            Self::Pouring => "pouring",
            Self::Rainy => "rainy",
            Self::Snowy => "snowy",
            Self::SnowyRainy => "snowy-rainy",
            Self::Sunny => "sunny",
            Self::Windy => "windy",
            Self::WindyVariant => "windy-variant",
        }
    }

    /// The condition Home Assistant calls `name`.
    pub fn parse(name: &str) -> Option<Self> {
        super::from_ha(name)
    }
}

/// Every text a weather entity's value can be, for rules to check against.
pub(crate) static CONDITIONS: LazyLock<Vec<String>> = LazyLock::new(|| {
    WeatherCondition::ALL
        .iter()
        .map(|condition| condition.as_str().to_owned())
        .collect()
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WeatherState {
    pub condition: WeatherCondition,
    /// °C.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// What it feels like, °C.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apparent_temperature: Option<f64>,
    /// °C.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dew_point: Option<f64>,
    /// Relative humidity, %.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0, max = 100.0))]
    pub humidity: Option<f64>,
    /// Air pressure, hPa.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0))]
    pub pressure: Option<f64>,
    /// km/h.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0))]
    pub wind_speed: Option<f64>,
    /// km/h.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0))]
    pub wind_gust_speed: Option<f64>,
    /// Where the wind comes from, degrees from north.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0, max = 360.0))]
    pub wind_bearing: Option<f64>,
    /// How much of the sky is cloud, %.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0, max = 100.0))]
    pub cloud_coverage: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0))]
    pub uv_index: Option<f64>,
    /// How far one can see, km.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0))]
    pub visibility: Option<f64>,
}

/// The fields, by name, with the least and most each can be.
const NUMBERS: [(&str, f64, f64); 11] = [
    ("temperature", -100.0, 100.0),
    ("apparent_temperature", -100.0, 100.0),
    ("dew_point", -100.0, 100.0),
    ("humidity", 0.0, 100.0),
    ("pressure", 0.0, 2000.0),
    ("wind_speed", 0.0, 1000.0),
    ("wind_gust_speed", 0.0, 1000.0),
    ("wind_bearing", 0.0, 360.0),
    ("cloud_coverage", 0.0, 100.0),
    ("uv_index", 0.0, 100.0),
    ("visibility", 0.0, 1000.0),
];

impl WeatherState {
    /// Only a condition, nothing else said.
    pub fn of(condition: WeatherCondition) -> Self {
        Self {
            condition,
            temperature: None,
            apparent_temperature: None,
            dew_point: None,
            humidity: None,
            pressure: None,
            wind_speed: None,
            wind_gust_speed: None,
            wind_bearing: None,
            cloud_coverage: None,
            uv_index: None,
            visibility: None,
        }
    }

    fn number(&self, name: &str) -> Option<f64> {
        match name {
            "temperature" => self.temperature,
            "apparent_temperature" => self.apparent_temperature,
            "dew_point" => self.dew_point,
            "humidity" => self.humidity,
            "pressure" => self.pressure,
            "wind_speed" => self.wind_speed,
            "wind_gust_speed" => self.wind_gust_speed,
            "wind_bearing" => self.wind_bearing,
            "cloud_coverage" => self.cloud_coverage,
            "uv_index" => self.uv_index,
            "visibility" => self.visibility,
            _ => None,
        }
    }

    /// Deserialization runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        for (name, least, most) in NUMBERS {
            if let Some(value) = self.number(name)
                && !(value.is_finite() && (least..=most).contains(&value))
            {
                return Err(InvariantError(format!(
                    "the weather's {name} is {least} to {most}, not {value}"
                )));
            }
        }
        Ok(())
    }
}

pub(crate) fn primary(state: &WeatherState) -> Typed {
    Typed::Text(state.condition.as_str().to_owned())
}

/// Every number the weather can have; which ones it has said is the state's to answer.
pub(crate) fn fields() -> Vec<Field> {
    NUMBERS
        .iter()
        .map(|(name, ..)| Field::number(name))
        .collect()
}

pub(crate) fn field(state: &WeatherState, name: &str) -> Option<Typed> {
    state.number(name).map(Typed::Number)
}

/// Keeps the numbers it said last.
pub(crate) fn with_primary(previous: Option<&WeatherState>, value: &Typed) -> Option<WeatherState> {
    let Typed::Text(text) = value else {
        return None;
    };
    let condition = WeatherCondition::parse(text)?;
    Some(WeatherState {
        condition,
        ..previous
            .cloned()
            .unwrap_or_else(|| WeatherState::of(condition))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_condition_is_its_value_in_home_assistants_words_and_numbers_are_fields() {
        let mut now = WeatherState::of(WeatherCondition::PartlyCloudy);
        now.temperature = Some(14.5);
        assert_eq!(primary(&now), Typed::Text("partlycloudy".into()));
        assert_eq!(field(&now, "temperature"), Some(Typed::Number(14.5)));
        assert_eq!(field(&now, "humidity"), None);
        assert_eq!(
            WeatherCondition::parse("lightning-rainy"),
            Some(WeatherCondition::LightningRainy)
        );
        assert_eq!(fields().len(), NUMBERS.len());

        now.humidity = Some(120.0);
        assert!(now.validate().is_err());
        now.humidity = Some(f64::NAN);
        assert!(now.validate().is_err());
    }
}
