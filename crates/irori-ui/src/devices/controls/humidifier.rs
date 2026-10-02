//! A humidifier or dehumidifier: on and off, the humidity it aims for, and its modes.

use irori_types::{Entity, State};
use irori_types::{HumidifierCapabilities, HumidifierState};
use leptos::prelude::*;

use super::knob;
use crate::devices::Controls;

/// A humidifier in words: "On · 50% · now 62%".
pub(crate) fn humidifier_words(humidifier: &HumidifierState) -> String {
    let mut words = vec![if humidifier.on { "On" } else { "Off" }.to_owned()];
    if humidifier.on {
        if let Some(mode) = &humidifier.mode {
            words.push(mode.clone());
        }
        if let Some(target) = humidifier.target_humidity {
            words.push(format!("{target}%"));
        }
    }
    if let Some(now) = humidifier.current_humidity {
        words.push(format!("now {now}%"));
    }
    words.join(" · ")
}

/// A humidifier or dehumidifier: on and off like a switch, and while it's on the humidity it aims
/// for and its modes.
pub(crate) fn humidifier_control(
    entity: &Entity,
    capabilities: &HumidifierCapabilities,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let current = match value {
        Some(State::Humidifier(humidifier)) => Some(humidifier.clone()),
        _ => None,
    };
    let entity_id = entity.id.clone();
    let disable = {
        let entity_id = entity_id.clone();
        move || offline || controls.busy.get().contains(&entity_id)
    };
    let running = current.clone().filter(|humidifier| humidifier.on);
    let target = running.as_ref().map(|humidifier| {
        let entity_id = entity_id.clone();
        view! {
            <label class="climate-target">
                <input
                    type="number"
                    min=capabilities.humidity.min.to_string()
                    max=capabilities.humidity.max.to_string()
                    step="1"
                    aria-label=format!("Target humidity for {}", entity.name)
                    prop:value=humidifier
                        .target_humidity
                        .map(|t| t.to_string())
                        .unwrap_or_default()
                    disabled=disable.clone()
                    on:change:target=move |ev| {
                        if let Ok(humidity) = ev.target().value().parse::<f64>() {
                            controls.act.run((
                                entity_id.clone(),
                                "set_humidity",
                                Some(serde_json::json!({ "humidity": humidity })),
                            ));
                        }
                    }
                />
                "%"
            </label>
        }
    });
    let modes = (running.is_some() && !capabilities.modes.is_empty()).then(|| {
        let chosen = running
            .as_ref()
            .and_then(|humidifier| humidifier.mode.clone());
        let entity_id = entity_id.clone();
        let options = capabilities
            .modes
            .iter()
            .map(|mode| {
                let selected = chosen.as_deref() == Some(mode.as_str());
                view! { <option value=mode.clone() selected=selected>{mode.clone()}</option> }
            })
            .collect_view();
        view! {
            <select
                class="select-control"
                aria-label=format!("Mode for {}", entity.name)
                disabled=disable.clone()
                on:change:target=move |ev| {
                    controls.act.run((
                        entity_id.clone(),
                        "set_mode",
                        Some(serde_json::json!({ "mode": ev.target().value() })),
                    ));
                }
            >
                <option value="" selected=chosen.is_none() disabled=true>"Mode"</option>
                {options}
            </select>
        }
    });
    let now = current
        .as_ref()
        .and_then(|humidifier| humidifier.current_humidity)
        .map(|now| view! { <span class="climate-now">{format!("now {now}%")}</span> });
    let on = current.as_ref().map(|humidifier| humidifier.on);
    view! {
        <>
            {knob(entity, on, offline, controls)}
            <span class="light-controls">{target}{modes}{now}</span>
        </>
    }
    .into_any()
}
