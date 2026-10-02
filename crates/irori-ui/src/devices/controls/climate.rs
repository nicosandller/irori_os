//! A thermostat or air conditioner: its mode, its target or range, and its settings.

use irori_types::{ClimateCapabilities, ClimateState, Entity, HvacMode, State};
use leptos::prelude::*;

use super::degrees;
use crate::choices::choices;
use crate::devices::Controls;

/// A thermostat's mode, in words.
pub(crate) fn hvac_mode_words(mode: HvacMode) -> &'static str {
    match mode {
        HvacMode::Off => "Off",
        HvacMode::Heat => "Heat",
        HvacMode::Cool => "Cool",
        HvacMode::HeatCool => "Heat and cool",
        HvacMode::Auto => "Auto",
        HvacMode::Dry => "Dry",
        HvacMode::FanOnly => "Fan only",
    }
}

/// A thermostat in words: "Heat · 21 °C · now 19.5 °C", "Off · now 18 °C".
pub(crate) fn climate_words(climate: &ClimateState) -> String {
    let mut words = vec![hvac_mode_words(climate.hvac_mode).to_owned()];
    if climate.hvac_mode != HvacMode::Off {
        match (
            climate.target_temperature,
            climate.target_temp_low,
            climate.target_temp_high,
        ) {
            (Some(target), _, _) => words.push(degrees(target)),
            (None, Some(low), Some(high)) => {
                words.push(format!("{}–{}", degrees(low), degrees(high)));
            }
            _ => {}
        }
    }
    if let Some(now) = climate.current_temperature {
        words.push(format!("now {}", degrees(now)));
    }
    words.join(" · ")
}

/// A thermostat: its mode, the temperature it's aiming for (or the range it keeps to), and its
/// presets and fan modes when it has them. Each change is sent as soon as it's made; a target is
/// sent when the box is left or Enter is pressed.
pub(crate) fn climate_control(
    entity: &Entity,
    capabilities: &ClimateCapabilities,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let current = match value {
        Some(State::Climate(climate)) => Some(climate.clone()),
        _ => None,
    };
    let entity_id = entity.id.clone();
    let disable = {
        let entity_id = entity_id.clone();
        move || offline || controls.busy.get().contains(&entity_id)
    };
    let picker = |label: String,
                  action: &'static str,
                  field: &'static str,
                  options: Vec<(String, String)>,
                  chosen: Option<String>| {
        let entity_id = entity_id.clone();
        let disable = disable.clone();
        choices(
            label,
            options,
            move || chosen.clone(),
            disable,
            move |picked| {
                controls.act.run((
                    entity_id.clone(),
                    action,
                    Some(serde_json::json!({ field: picked })),
                ));
            },
        )
    };
    let mode = picker(
        format!("Mode for {}", entity.name),
        "set_hvac_mode",
        "hvac_mode",
        capabilities
            .hvac_modes
            .iter()
            .map(|mode| (mode.as_str().to_owned(), hvac_mode_words(*mode).to_owned()))
            .collect(),
        current.as_ref().map(|c| c.hvac_mode.as_str().to_owned()),
    );
    let running = current.as_ref().filter(|c| c.hvac_mode != HvacMode::Off);
    let target_box = |label: String, at: Option<f64>, field: &'static str| {
        let entity_id = entity_id.clone();
        let disable = disable.clone();
        // A range sends both ends; the other end goes as it is.
        let other = match field {
            "target_temp_low" => running.and_then(|c| c.target_temp_high),
            "target_temp_high" => running.and_then(|c| c.target_temp_low),
            _ => None,
        };
        view! {
            <label class="climate-target">
                <input
                    type="number"
                    min=capabilities.min_temp.to_string()
                    max=capabilities.max_temp.to_string()
                    step=capabilities.temp_step.to_string()
                    aria-label=label
                    prop:value=at.map(|t| t.to_string()).unwrap_or_default()
                    disabled=disable
                    on:change:target=move |ev| {
                        let Ok(value) = ev.target().value().parse::<f64>() else {
                            return;
                        };
                        let data = match (field, other) {
                            ("target_temp_low", Some(high)) => serde_json::json!({
                                "target_temp_low": value, "target_temp_high": high,
                            }),
                            ("target_temp_high", Some(low)) => serde_json::json!({
                                "target_temp_low": low, "target_temp_high": value,
                            }),
                            _ => serde_json::json!({ "temperature": value }),
                        };
                        controls.act.run((entity_id.clone(), "set_temperature", Some(data)));
                    }
                />
                "°C"
            </label>
        }
    };
    let targets = running.map(|climate| {
        if capabilities.target_temperature {
            view! {
                {target_box(
                    format!("Target temperature for {}", entity.name),
                    climate.target_temperature,
                    "temperature",
                )}
            }
            .into_any()
        } else if capabilities.target_temperature_range
            && climate.target_temp_low.is_some()
            && climate.target_temp_high.is_some()
        {
            view! {
                {target_box(
                    format!("Lowest temperature for {}", entity.name),
                    climate.target_temp_low,
                    "target_temp_low",
                )}
                {target_box(
                    format!("Highest temperature for {}", entity.name),
                    climate.target_temp_high,
                    "target_temp_high",
                )}
            }
            .into_any()
        } else {
            ().into_any()
        }
    });
    let words = |list: &[String]| -> Vec<(String, String)> {
        list.iter().map(|m| (m.clone(), m.clone())).collect()
    };
    let presets = (running.is_some() && !capabilities.preset_modes.is_empty()).then(|| {
        picker(
            format!("Preset for {}", entity.name),
            "set_preset_mode",
            "preset_mode",
            words(&capabilities.preset_modes),
            running.and_then(|c| c.preset_mode.clone()),
        )
    });
    let fan = (running.is_some() && !capabilities.fan_modes.is_empty()).then(|| {
        picker(
            format!("Fan mode for {}", entity.name),
            "set_fan_mode",
            "fan_mode",
            words(&capabilities.fan_modes),
            running.and_then(|c| c.fan_mode.clone()),
        )
    });
    let now = current
        .as_ref()
        .and_then(|c| c.current_temperature)
        .map(|now| view! { <span class="climate-now">{format!("now {}", degrees(now))}</span> });
    view! {
        <span class="light-controls">{mode}{targets}{presets}{fan}{now}</span>
    }
    .into_any()
}
