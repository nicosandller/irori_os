//! Somewhere a message can be sent: a box to write it in, sent when the box is left.

use irori_types::Entity;
use leptos::prelude::*;

use crate::devices::Controls;

pub(crate) fn notify_control(entity: &Entity, offline: bool, controls: Controls) -> AnyView {
    let entity_id = entity.id.clone();
    let disable = {
        let entity_id = entity_id.clone();
        move || offline || controls.busy.get().contains(&entity_id)
    };
    view! {
        <input
            class="text-control"
            type="text"
            aria-label=format!("Send a message to {}", entity.name)
            maxlength=irori_types::NOTIFY_MESSAGE_MOST.to_string()
            placeholder="Send a message"
            disabled=disable
            on:change:target=move |ev| {
                let input = ev.target();
                let message = input.value();
                if message.trim().is_empty() {
                    return;
                }
                input.set_value("");
                controls.act.run((
                    entity_id.clone(),
                    "send_message",
                    Some(serde_json::json!({ "message": message })),
                ));
            }
        />
    }
    .into_any()
}
