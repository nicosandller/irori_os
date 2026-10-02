//! One choice out of a list.

use irori_types::{Entity, SelectCapabilities, State};
use leptos::prelude::*;

use super::UNKNOWN;
use crate::devices::Controls;

/// A select: its choices in a dropdown, on the one the device last reported. Choosing sends it.
pub(crate) fn select_control(
    entity: &Entity,
    capabilities: &SelectCapabilities,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let current = match value {
        Some(State::Select(select)) => Some(select.option.clone()),
        _ => None,
    };
    let entity_id = entity.id.clone();
    let disable = {
        let entity_id = entity_id.clone();
        move || offline || controls.busy.get().contains(&entity_id)
    };
    let options = capabilities
        .options
        .iter()
        .map(|option| {
            let chosen = current.as_deref() == Some(option.as_str());
            view! { <option value=option.clone() selected=chosen>{option.clone()}</option> }
        })
        .collect_view();
    view! {
        <select
            class="select-control"
            aria-label=entity.name.to_string()
            disabled=disable
            on:change:target=move |ev| {
                controls.act.run((
                    entity_id.clone(),
                    "select_option",
                    Some(serde_json::json!({ "option": ev.target().value() })),
                ));
            }
        >
            // Until it says, nothing is chosen rather than its first option.
            {current.is_none().then(|| view! { <option value="" selected=true disabled=true>{UNKNOWN}</option> })}
            {options}
        </select>
    }
    .into_any()
}
