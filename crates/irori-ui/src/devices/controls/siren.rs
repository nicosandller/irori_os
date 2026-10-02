//! A siren: sound it, with a tone and a volume when it has them, or quiet it.

use irori_types::{Entity, SirenCapabilities, State};
use leptos::prelude::*;

use super::knob;
use crate::choices::choices;
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
            .map(|tone| (tone.clone(), tone.clone()))
            .collect();
        let busy_id = entity_id.clone();
        // A siren doesn't report which tone is sounding, so none of these stays pressed:
        // picking one sounds it.
        choices(
            format!("Sound {} with a tone", entity.name),
            options,
            || None,
            move || offline || controls.busy.get().contains(&busy_id),
            move |tone| {
                controls.act.run((
                    entity_id.clone(),
                    "turn_on",
                    Some(serde_json::json!({ "tone": tone })),
                ));
            },
        )
    });
    view! {
        <>
            {tones}
            {knob(entity, on, offline, controls)}
        </>
    }
    .into_any()
}
