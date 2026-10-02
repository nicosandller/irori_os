//! A piece of text the device holds, set from here.

use irori_types::{Entity, State, TextCapabilities, TextMode};
use leptos::prelude::*;

use crate::devices::Controls;

/// A text: a box holding what the device last reported, sent when the box is left. A secret
/// (`password` mode) is never shown: the box starts empty, and typing replaces it.
pub(crate) fn text_control(
    entity: &Entity,
    capabilities: &TextCapabilities,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let secret = capabilities.mode == TextMode::Password;
    let current = match value {
        Some(State::Text(text)) if !secret => text.value.clone(),
        _ => String::new(),
    };
    let entity_id = entity.id.clone();
    let disable = {
        let entity_id = entity_id.clone();
        move || offline || controls.busy.get().contains(&entity_id)
    };
    let (min, max) = (capabilities.min_length, capabilities.max_length);
    view! {
        <input
            class="text-control"
            type=if secret { "password" } else { "text" }
            aria-label=entity.name.to_string()
            minlength=min.to_string()
            maxlength=max.to_string()
            pattern=capabilities.pattern.clone()
            placeholder=if secret { "Hidden" } else { "" }
            prop:value=current
            disabled=disable
            on:change:target=move |ev| {
                controls.act.run((
                    entity_id.clone(),
                    "set_value",
                    Some(serde_json::json!({ "value": ev.target().value() })),
                ));
            }
        />
    }
    .into_any()
}
