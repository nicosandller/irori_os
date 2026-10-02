//! One choice out of a list.

use irori_types::{Entity, SelectCapabilities, State};
use leptos::prelude::*;

use crate::choices::choices;
use crate::devices::Controls;

/// A select: its choices as buttons, on the one the device last reported. Choosing sends it.
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
        .map(|option| (option.clone(), option.clone()))
        .collect();
    // Until it says, nothing is pressed rather than its first option.
    let chosen = current.clone();
    choices(
        entity.name.to_string(),
        options,
        move || chosen.clone(),
        disable,
        move |option| {
            controls.act.run((
                entity_id.clone(),
                "select_option",
                Some(serde_json::json!({ "option": option })),
            ));
        },
    )
}
