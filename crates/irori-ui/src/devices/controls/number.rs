//! A number set within a range: a slider or a box.

use irori_types::{Capabilities, Entity, NumberCapabilities, NumberMode, State};
use leptos::prelude::*;

use super::{UNKNOWN, fill, number};
use crate::devices::Controls;

/// The unit a reading or a setting is in, for labels: `°C`, `s`. Empty when it has none.
pub(crate) fn unit_of(capabilities: &Capabilities) -> String {
    match capabilities {
        Capabilities::Sensor(sensor) => sensor.unit.clone(),
        Capabilities::Number(number) => number.unit.clone(),
        _ => None,
    }
    .unwrap_or_default()
}

/// A number: its value and unit, and a way to change it. A slider when its range is short
/// enough to drag through (Home Assistant's rule: at most 256 steps), or when the device asks
/// for one; a box to type into otherwise. Like a light's sliders, only letting go (or leaving
/// the box) sends anything.
pub(crate) fn number_control(
    entity: &Entity,
    capabilities: &NumberCapabilities,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let current = match value {
        Some(State::Number(number)) => Some(number.value),
        _ => None,
    };
    let (min, max, step) = (capabilities.min, capabilities.max, capabilities.step);
    let slider = match capabilities.mode {
        NumberMode::Slider => true,
        NumberMode::Box => false,
        NumberMode::Auto => (max - min) / step <= 256.0,
    };
    let unit = capabilities.unit.clone().unwrap_or_default();
    let label = format!(
        "{} ({})",
        entity.name,
        if unit.is_empty() { "value" } else { &unit }
    );
    let entity_id = entity.id.clone();
    let disable = {
        let entity_id = entity_id.clone();
        move || offline || controls.busy.get().contains(&entity_id)
    };
    // Anything outside the range, or not a number at all, isn't sent: the box goes back to what
    // the device last said when the page next draws it.
    let send = move |text: String| {
        if let Ok(value) = text.trim().parse::<f64>()
            && (min..=max).contains(&value)
        {
            controls.act.run((
                entity_id.clone(),
                "set_value",
                Some(serde_json::json!({ "value": value })),
            ));
        }
    };
    if slider {
        let dragged = RwSignal::new(current.unwrap_or(min));
        let title = label.clone();
        // The same row a light's sliders sit in, so the two look and behave alike.
        view! {
            <span class="light-controls">
            <label class="dim" title=title>
                <span class="lv">
                    {move || current.map_or_else(|| UNKNOWN.to_owned(), |_| number(dragged.get()))}
                    {(!unit.is_empty()).then(|| format!(" {unit}"))}
                </span>
                <input
                    type="range"
                    min=min.to_string()
                    max=max.to_string()
                    step=step.to_string()
                    aria-label=label
                    prop:value=current.unwrap_or(min).to_string()
                    style:--fill=move || format!("{}%", fill(dragged.get(), min, max))
                    disabled=disable
                    on:input:target=move |ev| {
                        if let Ok(value) = ev.target().value().parse::<f64>() {
                            dragged.set(value);
                        }
                    }
                    on:change:target=move |ev| send(ev.target().value())
                />
            </label>
            </span>
        }
        .into_any()
    } else {
        view! {
            <label class="number-box">
                <input
                    type="number"
                    inputmode="decimal"
                    min=min.to_string()
                    max=max.to_string()
                    step=step.to_string()
                    aria-label=label
                    prop:value=current.map(number).unwrap_or_default()
                    placeholder=UNKNOWN
                    disabled=disable
                    on:change:target=move |ev| send(ev.target().value())
                />
                {(!unit.is_empty()).then(|| view! { <span class="unit">{unit}</span> })}
            </label>
        }
        .into_any()
    }
}
