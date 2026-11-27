//! A to-do list: how many items are left, and a box to add one where it can be added to.

use irori_types::{Entity, State, TodoCapabilities};
use leptos::prelude::*;

use super::UNKNOWN;
use crate::devices::Controls;

/// `3 to do`, or `Done` when nothing is left.
pub(crate) fn todo_words(open: u32) -> String {
    if open == 0 {
        "Done".to_owned()
    } else {
        format!("{open} to do")
    }
}

pub(crate) fn todo_control(
    entity: &Entity,
    capabilities: TodoCapabilities,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let reading = match value {
        Some(State::Todo(todo)) => todo_words(todo.open),
        _ => UNKNOWN.to_owned(),
    };
    let add = capabilities.add.then(|| {
        let entity_id = entity.id.clone();
        let disable = {
            let entity_id = entity_id.clone();
            move || offline || controls.busy.get().contains(&entity_id)
        };
        view! {
            <input
                class="text-control"
                type="text"
                aria-label=format!("Add to {}", entity.name)
                maxlength=irori_types::TODO_ITEM_MOST.to_string()
                placeholder="Add an item"
                disabled=disable
                on:change:target=move |ev| {
                    let input = ev.target();
                    let item = input.value();
                    if item.trim().is_empty() {
                        return;
                    }
                    input.set_value("");
                    controls.act.run((
                        entity_id.clone(),
                        "add_item",
                        Some(serde_json::json!({ "item": item })),
                    ));
                }
            />
        }
    });
    view! {
        <span class="reading">{reading}</span>
        {add}
    }
    .into_any()
}
