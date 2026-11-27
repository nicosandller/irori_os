//! The weather: its condition in words, and the temperature when it says one.

use irori_types::{State, WeatherCondition, WeatherState};
use leptos::prelude::*;

use super::{UNKNOWN, number};

/// A condition the way a person says it.
pub(crate) fn condition_words(condition: WeatherCondition) -> &'static str {
    match condition {
        WeatherCondition::ClearNight => "Clear",
        WeatherCondition::Cloudy => "Cloudy",
        WeatherCondition::Exceptional => "Exceptional",
        WeatherCondition::Fog => "Fog",
        WeatherCondition::Hail => "Hail",
        WeatherCondition::Lightning => "Lightning",
        WeatherCondition::LightningRainy => "Thunderstorms",
        WeatherCondition::PartlyCloudy => "Partly cloudy",
        WeatherCondition::Pouring => "Pouring",
        WeatherCondition::Rainy => "Rain",
        WeatherCondition::Snowy => "Snow",
        WeatherCondition::SnowyRainy => "Sleet",
        WeatherCondition::Sunny => "Sunny",
        WeatherCondition::Windy | WeatherCondition::WindyVariant => "Windy",
    }
}

/// `Partly cloudy · 14.5 °C`.
pub(crate) fn weather_words(weather: &WeatherState) -> String {
    let condition = condition_words(weather.condition);
    match weather.temperature {
        Some(temperature) => format!("{condition} · {} °C", number(temperature)),
        None => condition.to_owned(),
    }
}

pub(crate) fn weather_reading(value: Option<&State>) -> AnyView {
    let reading = match value {
        Some(State::Weather(weather)) => weather_words(weather),
        _ => UNKNOWN.to_owned(),
    };
    view! { <span class="reading">{reading}</span> }.into_any()
}
