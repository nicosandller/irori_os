//! A water heater: its mode, and the temperature it heats the water to.

use irori_types::{Entity, State};
use irori_types::{WaterHeaterCapabilities, WaterHeaterMode, WaterHeaterState};
use leptos::prelude::*;

use super::degrees;
use crate::devices::Controls;

/// A water heater's mode, in words.
pub(crate) fn operation_mode_words(mode: WaterHeaterMode) -> &'static str {
    match mode {
        WaterHeaterMode::Off => "Off",
        WaterHeaterMode::Eco => "Eco",
        WaterHeaterMode::Electric => "Electric",
        WaterHeaterMode::Gas => "Gas",
        WaterHeaterMode::HeatPump => "Heat pump",
        WaterHeaterMode::HighDemand => "High demand",
        WaterHeaterMode::Performance => "Performance",
    }
}

/// A water heater in words: "Eco · 55 °C · water 52 °C".
pub(crate) fn water_heater_words(heater: &WaterHeaterState) -> String {
    let mut words = vec![operation_mode_words(heater.operation_mode).to_owned()];
    if heater.operation_mode != WaterHeaterMode::Off
        && let Some(target) = heater.target_temperature
    {
        words.push(degrees(target));
    }
    if let Some(water) = heater.current_temperature {
        words.push(format!("water {}", degrees(water)));
    }
    words.join(" · ")
}

/// A water heater: its mode, with off when it has its own switch, and its target.
pub(crate) fn water_heater_control(
    entity: &Entity,
    capabilities: &WaterHeaterCapabilities,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let current = match value {
        Some(State::WaterHeater(heater)) => Some(heater.clone()),
        _ => None,
    };
    let entity_id = entity.id.clone();
    let disable = {
        let entity_id = entity_id.clone();
        move || offline || controls.busy.get().contains(&entity_id)
    };
    let mut modes = capabilities.operation_modes.clone();
    if capabilities.on_off && !modes.contains(&WaterHeaterMode::Off) {
        modes.insert(0, WaterHeaterMode::Off);
    }
    let chosen = current.as_ref().map(|heater| heater.operation_mode);
    let options = modes
        .iter()
        .map(|mode| {
            view! {
                <option value=mode.as_str() selected=chosen == Some(*mode)>
                    {operation_mode_words(*mode)}
                </option>
            }
        })
        .collect_view();
    let switch = capabilities.on_off;
    let mode = {
        let entity_id = entity_id.clone();
        let disable = disable.clone();
        view! {
            <select
                class="select-control"
                aria-label=format!("Mode for {}", entity.name)
                disabled=disable
                on:change:target=move |ev| {
                    let picked = ev.target().value();
                    // Off by its own switch, and on again with it before a mode is chosen.
                    let (action, data) = match (picked.as_str(), switch, chosen) {
                        ("off", true, _) => ("turn_off", None),
                        (_, true, Some(WaterHeaterMode::Off)) => ("turn_on", None),
                        _ => (
                            "set_operation_mode",
                            Some(serde_json::json!({ "operation_mode": picked })),
                        ),
                    };
                    controls.act.run((entity_id.clone(), action, data));
                }
            >
                <option value="" selected=chosen.is_none() disabled=true>"—"</option>
                {options}
            </select>
        }
    };
    let running = current
        .as_ref()
        .filter(|heater| heater.operation_mode != WaterHeaterMode::Off);
    let target = (capabilities.target_temperature && running.is_some()).then(|| {
        let at = running.and_then(|heater| heater.target_temperature);
        let entity_id = entity_id.clone();
        view! {
            <label class="climate-target">
                <input
                    type="number"
                    min=capabilities.min_temp.to_string()
                    max=capabilities.max_temp.to_string()
                    step=capabilities.temp_step.to_string()
                    aria-label=format!("Target temperature for {}", entity.name)
                    prop:value=at.map(|t| t.to_string()).unwrap_or_default()
                    disabled=disable.clone()
                    on:change:target=move |ev| {
                        if let Ok(temperature) = ev.target().value().parse::<f64>() {
                            controls.act.run((
                                entity_id.clone(),
                                "set_temperature",
                                Some(serde_json::json!({ "temperature": temperature })),
                            ));
                        }
                    }
                />
                "°C"
            </label>
        }
    });
    let water = current
        .as_ref()
        .and_then(|heater| heater.current_temperature)
        .map(|water| view! { <span class="climate-now">{format!("water {}", degrees(water))}</span> });
    view! { <span class="light-controls">{mode}{target}{water}</span> }.into_any()
}
