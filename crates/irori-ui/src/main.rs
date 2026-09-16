//! The Irori web UI: one page listing every device in the home, with a switch for the things
//! that can be switched.
//!
//! Compiled to wasm and embedded in the binary (`cargo xtask ui`). See `README.md` for how to
//! run it against a live core while working on it.

mod api;
mod devices;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use irori_types::EntityId;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::Home;
use crate::devices::Controls;

/// How often the page asks the core what changed. Polling is temporary: the WebSocket API
/// (M1.5) pushes changes instead, and then this disappears.
const REFRESH: Duration = Duration::from_secs(2);

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    let home = RwSignal::new(Home::default());
    let trouble = RwSignal::new(Option::<String>::None);
    let filter = RwSignal::new(String::new());
    let busy = RwSignal::new(BTreeSet::new());
    let failures = RwSignal::new(BTreeMap::new());
    let controls = Controls {
        busy,
        failures,
        set_on: Callback::new(move |(entity_id, on)| set_on(entity_id, on, home, busy, failures)),
    };

    spawn_local(async move {
        loop {
            match api::fetch_home().await {
                Ok(mut fetched) => {
                    let shown = home.get_untracked();
                    // This snapshot can be older than a change the page already has from a
                    // command it sent, so the fresher of the two wins per entity.
                    for state in &shown.states {
                        fetched.accept(state.clone());
                    }
                    // Set it only when something actually changed: an unchanged home would
                    // rebuild the list under the pointer twice a second for nothing.
                    if fetched != shown {
                        home.set(fetched);
                    }
                    trouble.set(None);
                }
                Err(why) => trouble.set(Some(why)),
            }
            gloo_timers::future::sleep(REFRESH).await;
        }
    });

    view! {
        <main>
            <header>
                // Mark A (assets/irori-mark-a-mono.svg): frame follows the text, ember stays ember.
                <svg viewBox="0 0 48 48" role="img" aria-label="IroriOS mark">
                    <rect x="2" y="2" width="44" height="44" rx="2.5" fill="none"
                        stroke="currentColor" stroke-width="4" />
                    <rect x="17" y="17" width="14" height="14" rx="1" fill="#c4552b" />
                </svg>
                <h1>"Devices"</h1>
                <span class="live">
                    <span class="dot" class:ok=move || trouble.get().is_none()></span>
                    {move || if trouble.get().is_none() { "Live" } else { "No answer" }}
                </span>
            </header>

            {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}

            <input
                class="filter"
                type="search"
                placeholder="Filter by name or id"
                aria-label="Filter devices"
                prop:value=filter
                on:input:target=move |ev| filter.set(ev.target().value())
            />

            {move || devices::view(&home.get(), &filter.get(), controls)}
            <Extensions home=home />
        </main>
    }
}

/// Sends the command, then puts the entity's new state on the page without waiting for the next
/// refresh: the core answers once the integration has confirmed.
fn set_on(
    entity_id: EntityId,
    on: bool,
    home: RwSignal<Home>,
    busy: RwSignal<BTreeSet<EntityId>>,
    failures: RwSignal<BTreeMap<EntityId, String>>,
) {
    busy.update(|busy| {
        busy.insert(entity_id.clone());
    });
    spawn_local(async move {
        match api::set_on(&entity_id, on).await {
            Ok(state) => {
                failures.update(|failures| {
                    failures.remove(&entity_id);
                });
                if let Some(state) = state {
                    // Unless a poll has already brought something newer back.
                    let mut next = home.get_untracked();
                    if next.accept(state) {
                        home.set(next);
                    }
                }
            }
            Err(why) => failures.update(|failures| {
                failures.insert(entity_id.clone(), why);
            }),
        }
        busy.update(|busy| {
            busy.remove(&entity_id);
        });
    });
}

/// What's providing all this, and whether it's healthy. A footnote until the Extensions page
/// exists (M1.6).
#[component]
fn Extensions(home: RwSignal<Home>) -> impl IntoView {
    move || {
        let extensions = home.get().extensions;
        (!extensions.is_empty()).then(|| {
            view! {
                <section class="extensions">
                    <h2>"Extensions"</h2>
                    <ul>
                        {extensions
                            .into_iter()
                            .map(|(id, extension)| {
                                let lost = extension.rejected_reports + extension.dropped_reports;
                                view! {
                                    <li>
                                        <span class="ext-id">{id.to_string()}</span>
                                        " · " {extension.state}
                                        {extension.reason.map(|why| format!(" · {why}"))}
                                        {(lost > 0).then(|| format!(" · {lost} reports lost"))}
                                    </li>
                                }
                            })
                            .collect_view()}
                    </ul>
                </section>
            }
        })
    }
}
