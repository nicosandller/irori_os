//! A button: something to press.

use irori_types::{ButtonClass, Entity};
use leptos::prelude::*;

use crate::devices::Controls;

/// A button: one press, named for what it does when the device says.
pub(crate) fn button_control(
    entity: &Entity,
    class: Option<ButtonClass>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let label = match class {
        Some(ButtonClass::Restart) => "Restart",
        Some(ButtonClass::Identify) => "Identify",
        Some(ButtonClass::Update) => "Update",
        None => "Press",
    };
    let entity_id = entity.id.clone();
    let disable = {
        let entity_id = entity_id.clone();
        move || offline || controls.busy.get().contains(&entity_id)
    };
    view! {
        <button
            type="button"
            class="press"
            aria-label=format!("{label}: {}", entity.name)
            disabled=disable
            on:click=move |_| controls.act.run((entity_id.clone(), "press", None))
        >
            {label}
        </button>
    }
    .into_any()
}
