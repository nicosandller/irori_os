//! What extensions have found but can't use until someone helps: a device that needs its
//! encryption key, say (`docs/specs/protocols.md` §6.6).
//!
//! Generic on purpose. An extension says what it's waiting for and where a secret goes; this
//! page doesn't know what ESPHome is, and a future extension that needs a pairing code gets the
//! same form.

use std::collections::BTreeMap;

use irori_types::{ExtensionId, Waiting};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;

/// Everything waiting, by extension. A memo over the extensions alone, not the readings: the key
/// field below has to survive a sensor reporting every second (ROADMAP D33).
pub fn everything_waiting(
    live: crate::Live,
) -> Memo<BTreeMap<ExtensionId, (String, Vec<Waiting>)>> {
    Memo::new(move |_| {
        live.home
            .get()
            .extensions
            .into_iter()
            .filter(|(_, extension)| !extension.waiting.is_empty())
            .map(|(id, extension)| (id, (extension.name, extension.waiting)))
            .collect()
    })
}

/// How many things are waiting, across every extension.
pub fn count(waiting: &BTreeMap<ExtensionId, (String, Vec<Waiting>)>) -> usize {
    waiting.values().map(|(_, items)| items.len()).sum()
}

/// The list, with a form for each thing that needs a secret. Shows nothing when nothing waits.
#[component]
pub fn Waiting() -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let waiting = everything_waiting(live);

    move || {
        let all = waiting.get();
        if all.is_empty() {
            return ().into_any();
        }
        let total = count(&all);
        let items = all
            .into_iter()
            .flat_map(|(extension, (name, items))| {
                items
                    .into_iter()
                    .map(move |item| (extension.clone(), name.clone(), item))
            })
            .map(|(extension, through, item)| view! { <Item extension through item /> })
            .collect_view();
        view! {
            <section class="card waiting">
                <h2>
                    {format!(
                        "Found, and waiting for you ({total})",
                    )}
                </h2>
                <p class="muted small">
                    "These are on your network and Irori can see them, but can't use them yet."
                </p>
                <ul class="waiting-list">{items}</ul>
            </section>
        }
        .into_any()
    }
}

#[component]
fn Item(extension: ExtensionId, through: String, item: Waiting) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let value = RwSignal::new(String::new());
    let sending = RwSignal::new(false);
    let trouble = RwSignal::new(None::<String>);
    let secret = item.secret.clone();

    let send = {
        let secret = secret.clone();
        move || {
            let Some(secret) = secret.clone() else { return };
            let typed = value.get_untracked();
            if typed.trim().is_empty() {
                return;
            }
            let extension = extension.clone();
            sending.set(true);
            spawn_local(async move {
                match api::give_secret(&extension, &secret.path, typed.trim()).await {
                    Ok(()) => {
                        // Cleared straight away: the page shouldn't keep a key around any longer
                        // than it takes to send it.
                        value.set(String::new());
                        trouble.set(None);
                        crate::refresh(live);
                    }
                    Err(why) => trouble.set(Some(why)),
                }
                sending.set(false);
            });
        }
    };

    view! {
        <li>
            <div class="protocol-head">
                <span class="name">{item.name.to_string()}</span>
                <span class="badge">{through}</span>
                <span class="muted small id">{item.unique_id.to_string()}</span>
            </div>
            <p class="why-waiting">{item.reason.clone()}</p>
            {secret.map(|secret| {
                let send = send.clone();
                view! {
                    <form
                        class="inline-form"
                        on:submit=move |ev| {
                            ev.prevent_default();
                            send();
                        }
                    >
                        <input
                            type="password"
                            autocomplete="off"
                            spellcheck="false"
                            aria-label=secret.label.clone()
                            placeholder=secret.label.clone()
                            prop:value=value
                            on:input:target=move |ev| value.set(ev.target().value())
                        />
                        <button
                            type="submit"
                            class="add"
                            disabled=move || sending.get() || value.get().trim().is_empty()
                        >
                            {move || if sending.get() { "Saving…" } else { "Use this" }}
                        </button>
                    </form>
                    {secret.hint.clone().map(|hint| view! {
                        <p class="muted small">{format!("Where to find it: {hint}.")}</p>
                    })}
                    <p class="muted small">
                        "Kept in secrets.toml in Irori's config folder, readable only by Irori. "
                        "The page never shows it again."
                    </p>
                }
            })}
            {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
        </li>
    }
}
