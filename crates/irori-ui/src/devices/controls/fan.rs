//! A fan: on and off, its speed, and its other settings.

use irori_types::{Entity, FanCapabilities, FanState, State};
use leptos::prelude::*;

use super::{fill, knob};
use crate::choices::choices;
use crate::devices::Controls;

/// A fan in words: "On · 66%", "On · sleep", "Off".
pub(crate) fn fan_words(fan: &FanState) -> String {
    let on = if fan.on { "On" } else { "Off" };
    match (&fan.preset_mode, fan.percentage) {
        (_, _) if !fan.on => on.to_owned(),
        (Some(mode), _) => format!("{on} · {mode}"),
        (None, Some(percentage)) => format!("{on} · {percentage}%"),
        (None, None) => on.to_owned(),
    }
}

/// A fan: on and off like a switch, and while it's on its speed, its preset modes and whether it
/// swings. Like a light's sliders, only letting go of the speed sends it.
pub(crate) fn fan_control(
    entity: &Entity,
    capabilities: &FanCapabilities,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let current = match value {
        Some(State::Fan(fan)) => Some(fan.clone()),
        _ => None,
    };
    let on = current.as_ref().map(|fan| fan.on);
    let entity_id = entity.id.clone();
    let disable = {
        let entity_id = entity_id.clone();
        move || offline || controls.busy.get().contains(&entity_id)
    };
    let running = current.clone().filter(|fan| fan.on);
    let speed = (capabilities.speed_count > 0).then(|| {
        let at = running.as_ref().and_then(|fan| fan.percentage).unwrap_or(0);
        let dragged = RwSignal::new(at);
        let entity_id = entity_id.clone();
        let disable = disable.clone();
        // One stop per real speed, so the slider never offers a speed the fan doesn't have.
        let step = (100 / capabilities.speed_count.max(1)).max(1);
        view! {
            <label class="dim" title="Speed">
                <span class="lv">{move || format!("{}%", dragged.get())}</span>
                <input
                    type="range"
                    min="0"
                    max="100"
                    step=step.to_string()
                    aria-label=format!("Speed for {}", entity.name)
                    prop:value=at.to_string()
                    style:--fill=move || format!("{}%", fill(dragged.get(), 0, 100))
                    disabled=disable
                    on:input:target=move |ev| {
                        if let Ok(value) = ev.target().value().parse::<u8>() {
                            dragged.set(value);
                        }
                    }
                    on:change:target=move |ev| {
                        if let Ok(percentage) = ev.target().value().parse::<u8>() {
                            controls.act.run((
                                entity_id.clone(),
                                "set_percentage",
                                Some(serde_json::json!({ "percentage": percentage })),
                            ));
                        }
                    }
                />
            </label>
        }
    });
    let presets = (!capabilities.preset_modes.is_empty()).then(|| {
        let current_mode = running.as_ref().and_then(|fan| fan.preset_mode.clone());
        let entity_id = entity_id.clone();
        let disable = disable.clone();
        let options = capabilities
            .preset_modes
            .iter()
            .map(|mode| (mode.clone(), mode.clone()))
            .collect();
        let chosen = current_mode.clone();
        choices(
            format!("Mode for {}", entity.name),
            options,
            move || chosen.clone(),
            disable,
            move |mode| {
                controls.act.run((
                    entity_id.clone(),
                    "set_preset_mode",
                    Some(serde_json::json!({ "preset_mode": mode })),
                ));
            },
        )
    });
    let swing = capabilities.oscillate.then(|| {
        let swinging = running
            .as_ref()
            .and_then(|fan| fan.oscillating)
            .unwrap_or(false);
        let entity_id = entity_id.clone();
        view! {
            <label class="fan-swing">
                <input
                    type="checkbox"
                    prop:checked=swinging
                    disabled=disable.clone()
                    on:change:target=move |ev| {
                        controls.act.run((
                            entity_id.clone(),
                            "oscillate",
                            Some(serde_json::json!({ "oscillating": ev.target().checked() })),
                        ));
                    }
                />
                "Swing"
            </label>
        }
    });
    let extras = running.is_some() && (speed.is_some() || presets.is_some() || swing.is_some());
    view! {
        <>
            {knob(entity, on, offline, controls)}
            {extras.then(|| view! { <span class="light-controls">{speed}{presets}{swing}</span> })}
        </>
    }
    .into_any()
}
