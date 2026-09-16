//! The Irori web UI.
//!
//! Compiled to wasm and embedded in the binary (`cargo xtask ui`). See `README.md` for how to
//! run it against a live core while working on it.
//!
//! One place fetches what the home looks like and hands it to whichever page is showing, so
//! moving between pages doesn't refetch and the two can't disagree.

mod api;
mod device;
mod devices;
mod home;
mod rooms;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use irori_types::EntityId;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::{A, Route, Router, Routes};
use leptos_router::path;

use crate::api::{Health, Home};
use crate::devices::Controls;

/// How often the page asks the core what changed. Polling is temporary: the WebSocket API
/// (M1.5) pushes changes instead, and then this disappears.
const REFRESH: Duration = Duration::from_secs(2);

/// How many refreshes between asking Irori about itself. Its version and database don't change
/// while it runs, and its uptime only needs to be roughly right.
const HEALTH_EVERY: u32 = 15;

/// What every page is given: the home as it currently stands, and whether the core is answering.
#[derive(Debug, Clone, Copy)]
pub struct Live {
    pub home: RwSignal<Home>,
    pub health: RwSignal<Option<Health>>,
    /// Why the last refresh failed, if it did.
    pub trouble: RwSignal<Option<String>>,
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    let live = Live {
        home: RwSignal::new(Home::default()),
        health: RwSignal::new(None),
        trouble: RwSignal::new(None),
    };
    provide_context(live);

    let busy = RwSignal::new(BTreeSet::new());
    let failures = RwSignal::new(BTreeMap::new());
    let controls = Controls {
        busy,
        failures,
        set_on: Callback::new(move |(entity_id, on)| {
            set_on(entity_id, on, live.home, busy, failures)
        }),
    };
    provide_context(controls);

    spawn_local(async move {
        let mut ticks: u32 = 0;
        loop {
            match api::fetch_home().await {
                Ok(mut fetched) => {
                    let shown = live.home.get_untracked();
                    // This snapshot can be older than a change the page already has from a
                    // command it sent, so the fresher of the two wins per entity.
                    for state in &shown.states {
                        fetched.accept(state.clone());
                    }
                    // Set it only when something actually changed: an unchanged home would
                    // rebuild the list under the pointer twice a second for nothing.
                    if fetched != shown {
                        live.home.set(fetched);
                    }
                    live.trouble.set(None);
                }
                Err(why) => live.trouble.set(Some(why)),
            }
            // What Irori itself is doing changes far less often than what the devices are, so
            // it's asked for less often — but it is asked again: the uptime moves, and a
            // restart onto a different build should show, not sit there as the version the
            // page happened to load with.
            // A failure here changes nothing on purpose: the banner already says the core
            // isn't answering, and the last known facts are better than a blank card.
            if ticks.is_multiple_of(HEALTH_EVERY)
                && let Ok(health) = api::fetch_health().await
            {
                live.health.set(Some(health));
            }
            ticks = ticks.wrapping_add(1);
            gloo_timers::future::sleep(REFRESH).await;
        }
    });

    view! {
        <Router>
            <nav>
                <span class="mark">
                    // Mark A (assets/irori-mark-a-mono.svg): frame follows the text, ember stays.
                    <svg viewBox="0 0 48 48" role="img" aria-label="IroriOS">
                        <rect x="2" y="2" width="44" height="44" rx="2.5" fill="none"
                            stroke="currentColor" stroke-width="4" />
                        <rect x="17" y="17" width="14" height="14" rx="1" fill="#c4552b" />
                    </svg>
                    "Irori"
                </span>
                <A href="/">"Home"</A>
                <A href="/devices">"Devices"</A>
                <A href="/rooms">"Rooms"</A>
                <span class="live">
                    <span class="dot" class:ok=move || live.trouble.get().is_none()></span>
                    {move || if live.trouble.get().is_none() { "Live" } else { "No answer" }}
                </span>
            </nav>

            <main>
                {move || live.trouble.get().map(|why| view! { <p class="banner">{why}</p> })}
                <Routes fallback=NotFound>
                    <Route path=path!("/") view=home::Home />
                    <Route path=path!("/devices") view=devices::Devices />
                    <Route path=path!("/devices/:id") view=device::DevicePage />
                    <Route path=path!("/rooms") view=rooms::Rooms />
                </Routes>
            </main>
        </Router>
    }
}

#[component]
fn NotFound() -> impl IntoView {
    view! {
        <section class="card">
            <h1>"There's no page here"</h1>
            <p class="muted">
                "Irori has a Home page, a Devices page and a Rooms page. The rest is still to "
                "come."
            </p>
            <p><A href="/">"Back to the start"</A></p>
        </section>
    }
}

/// Asks the core for the home again, now, rather than waiting for the next poll.
///
/// Renaming and moving things change more than the thing that was changed — an entity with no
/// name of its own follows its device, and a room that goes away unplaces everything in it — so
/// after one of those the whole picture is refetched rather than patched.
pub fn refresh(live: Live) {
    spawn_local(async move {
        match api::fetch_home().await {
            Ok(fetched) => {
                if fetched != live.home.get_untracked() {
                    live.home.set(fetched);
                }
                live.trouble.set(None);
            }
            Err(why) => live.trouble.set(Some(why)),
        }
    });
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
