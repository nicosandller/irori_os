//! A siren: sound it, with a tone and a volume when it has them, or quiet it.

use irori_types::{Entity, SirenCapabilities, State};
use leptos::prelude::*;

use super::knob;
use crate::devices::Controls;

/// A siren: on and off like a switch, and a choice of tone when it has one, which sounds it.
pub(crate) fn siren_control(
    entity: &Entity,
    capabilities: &SirenCapabilities,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let on = match value {
        Some(State::Siren(siren)) => Some(siren.on),
        _ => None,
    };
    let entity_id = entity.id.clone();
    let tones = (!capabilities.tones.is_empty()).then(|| {
        let options = capabilities
            .tones
            .iter()
            .map(|tone| view! { <option value=tone.clone()>{tone.clone()}</option> })
            .collect_view();
        let busy_id = entity_id.clone();
        view! {
            <select
                class="select-control"
                aria-label=format!("Sound {} with a tone", entity.name)
                disabled=move || offline || controls.busy.get().contains(&busy_id)
                on:change:target=move |ev| {
                    controls.act.run((
                        entity_id.clone(),
                        "turn_on",
                        Some(serde_json::json!({ "tone": ev.target().value() })),
                    ));
                }
            >
                <option value="" selected=true disabled=true>"Tone"</option>
                {options}
            </select>
        }
    });
    view! {
        <>
            {tones}
            {knob(entity, on, offline, controls)}
        </>
    }
    .into_any()
}
