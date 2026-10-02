//! A lock: locked or not, with a confirmation before it unlocks or opens.

use irori_types::{Entity, LockCapabilities, LockStatus, State};
use leptos::prelude::*;

use super::UNKNOWN;
use crate::devices::Controls;

/// Where a lock is, in words.
pub(crate) fn lock_words(state: LockStatus) -> &'static str {
    match state {
        LockStatus::Locked => "Locked",
        LockStatus::Unlocked => "Unlocked",
        LockStatus::Locking => "Locking",
        LockStatus::Unlocking => "Unlocking",
        LockStatus::Jammed => "Jammed",
        LockStatus::Open => "Open",
        LockStatus::Opening => "Opening",
    }
}

/// A lock: where it is, and buttons to lock, unlock and (when it can) open it. Locking happens at
/// once; unlocking and opening let someone in, so they ask first, in a window that also takes the
/// code a lock may need. The code goes with the call and nowhere else.
pub(crate) fn lock_control(
    entity: &Entity,
    capabilities: &LockCapabilities,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let current = match value {
        Some(State::Lock(lock)) => Some(lock.state),
        _ => None,
    };
    let entity_id = entity.id.clone();
    let name = entity.name.to_string();
    let disable = {
        let entity_id = entity_id.clone();
        move || offline || controls.busy.get().contains(&entity_id)
    };
    // Which action is waiting to be confirmed, if any.
    let asking: RwSignal<Option<(&'static str, &'static str)>> = RwSignal::new(None);
    let code = RwSignal::new(String::new());
    let needs_code = capabilities.requires_code;
    let code_format = capabilities.code_format.clone();
    let send = {
        let entity_id = entity_id.clone();
        move |action: &'static str| {
            let typed = code.get_untracked();
            let data = (!typed.is_empty()).then(|| serde_json::json!({ "code": typed }));
            controls.act.run((entity_id.clone(), action, data));
            code.set(String::new());
            asking.set(None);
        }
    };
    let ask = |label: &'static str, action: &'static str| {
        let disable = disable.clone();
        view! {
            <button
                type="button"
                class="press"
                disabled=disable
                on:click=move |_| asking.set(Some((label, action)))
            >
                {label}
            </button>
        }
    };
    let lock = {
        let disable = disable.clone();
        let send = send.clone();
        view! {
            <button type="button" class="press" disabled=disable on:click=move |_| send("lock")>
                "Lock"
            </button>
        }
    };
    let confirm = move || {
        asking.get().map(|(label, action)| {
            let send = send.clone();
            let code_format = code_format.clone();
            view! {
                <crate::modal::Modal
                    title=format!("{label} {name}?")
                    on_close=Callback::new(move |()| {
                        code.set(String::new());
                        asking.set(None);
                    })
                >
                    <p>"This lets someone in. Irori sends it as soon as you confirm."</p>
                    {(needs_code || code_format.is_some()).then(|| view! {
                        <label class="lock-code">
                            "Code"
                            <input
                                type="password"
                                autocomplete="off"
                                pattern=code_format.clone()
                                required=needs_code
                                prop:value=move || code.get()
                                on:input:target=move |ev| code.set(ev.target().value())
                            />
                        </label>
                    })}
                    <div class="ext-actions">
                        <button
                            type="button"
                            class="danger"
                            on:click=move |_| {
                                code.set(String::new());
                                asking.set(None);
                            }
                        >
                            "Cancel"
                        </button>
                        <button
                            type="button"
                            class="add solid"
                            disabled=move || needs_code && code.get().is_empty()
                            on:click=move |_| send(action)
                        >
                            {label}
                        </button>
                    </div>
                </crate::modal::Modal>
            }
        })
    };
    view! {
        <>
            <span class="reading">{current.map_or(UNKNOWN, lock_words)}</span>
            <span class="cover-buttons">
                {lock}
                {ask("Unlock", "unlock")}
                {capabilities.open.then(|| ask("Open", "open"))}
            </span>
            {confirm}
        </>
    }
    .into_any()
}
