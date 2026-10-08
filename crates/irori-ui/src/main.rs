//! The Irori web UI.
//!
//! Compiled to wasm and embedded in the binary (`cargo xtask ui`). See `README.md` for how to
//! run it against a live core while working on it.
//!
//! One place fetches what the home looks like and hands it to whichever page is showing, so
//! moving between pages doesn't refetch and the two can't disagree.

mod api;
mod app_frame;
mod assistant;
mod chart;
mod choices;
mod count;
mod device;
mod devices;
mod extensions;
mod floorplan;
mod fold;
mod gesture;
mod glide;
mod history;
mod icons;
mod inline;
mod log_window;
mod machine;
mod map;
mod modal;
mod place;
mod places;
mod removal;
mod rich;
mod segmented;
mod settings;
mod settings_form;
mod start;
mod timeline;
mod transition;
mod users;
mod waiting;
mod welcome;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use irori_types::{EntityId, EntityState, LightTurnOn};
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::{A, Redirect, Route, Router, Routes};
use leptos_router::hooks::use_location;
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

/// Whether a model can answer. `None` until the first look, which hides the sidebar entry.
#[derive(Debug, Clone, Copy)]
pub struct Assistant(pub RwSignal<Option<api::AssistantStatus>>);

/// Who this browser is, and how far the home has been set up. `None` until the first answer.
#[derive(Debug, Clone, Copy)]
pub struct Session(pub RwSignal<Option<api::Session>>);

/// Whether the page animates: switches that spring, sliders that swell, the Live dot breathing.
/// On unless Settings turned it off. The system's own "reduce motion" wins over this either way —
/// that's CSS, and needs nothing from here.
#[derive(Debug, Clone, Copy)]
pub struct Motion(pub RwSignal<bool>);

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
    // Who is there. Asked before anything else: a home that asks gets the sign-in page and
    // nothing behind it, and a home nobody has set up gets the welcome.
    let session = RwSignal::new(None::<api::Session>);
    provide_context(Session(session));
    let place = RwSignal::new(None);
    provide_context(place::Place(place));
    let people = RwSignal::new(Vec::new());
    provide_context(users::People(people));
    let welcoming = RwSignal::new(false);
    provide_context(welcome::Showing(welcoming));
    let must_sign_in =
        Memo::new(move |_| session.with(|s| s.as_ref().is_some_and(api::Session::must_sign_in)));
    spawn_local(async move {
        if let Ok(now) = api::fetch_session().await {
            welcoming.set(now.wants_welcome() && now.owner);
            session.set(Some(now));
        }
    });
    // Where the home is and who is in it, once this browser may ask: as the page opens, and
    // again when somebody signs in.
    Effect::new(move |_| {
        if session.with(Option::is_none) || must_sign_in.get() {
            return;
        }
        spawn_local(async move {
            if let Ok(home) = api::fetch_place().await {
                place.set(Some(home));
            }
            if let Ok(users) = api::fetch_users().await {
                people.set(users);
            }
        });
    });
    let assistant = RwSignal::new(None);
    provide_context(Assistant(assistant));
    provide_context(assistant::Asking(RwSignal::new(None)));
    provide_context(assistant::Chats::new());
    provide_context(assistant::ModelSide(RwSignal::new(None)));
    let model_log = RwSignal::new(false);
    provide_context(assistant::ModelLog(model_log));

    let busy = RwSignal::new(BTreeSet::new());
    let failures = RwSignal::new(BTreeMap::new());
    let controls = Controls {
        busy,
        failures,
        set_on: Callback::new(move |(entity_id, on): (EntityId, bool)| {
            let (home, busy, failures) = (live.home, busy, failures);
            send_command(entity_id.clone(), home, busy, failures, async move {
                api::set_on(&entity_id, on).await
            })
        }),
        set_light: Callback::new(move |(entity_id, data): (EntityId, LightTurnOn)| {
            let (home, busy, failures) = (live.home, busy, failures);
            send_command(entity_id.clone(), home, busy, failures, async move {
                api::set_light(&entity_id, &data).await
            })
        }),
        act: Callback::new(
            move |(entity_id, action, data): (
                EntityId,
                &'static str,
                Option<serde_json::Value>,
            )| {
                let (home, busy, failures) = (live.home, busy, failures);
                send_command(entity_id.clone(), home, busy, failures, async move {
                    api::act(&entity_id, action, data).await
                })
            },
        ),
    };
    provide_context(controls);

    spawn_local(async move {
        let mut ticks: u32 = 0;
        loop {
            // Nobody is signed in: there is nothing this browser may ask for yet.
            if must_sign_in.get_untracked() {
                gloo_timers::future::sleep(REFRESH).await;
                continue;
            }
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
                Err(why) => {
                    live.trouble.set(Some(why));
                    // It may be that the home started asking who is there (a password set
                    // from another screen), or that this browser was signed out.
                    if let Ok(now) = api::fetch_session().await
                        && session.get_untracked().as_ref() != Some(&now)
                    {
                        session.set(Some(now));
                    }
                }
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
            // Same slow cadence: a model being ready changes rarely, and a chat must not
            // wait on the two-second home poll.
            if ticks.is_multiple_of(HEALTH_EVERY)
                && let Ok(status) = api::fetch_assistant().await
            {
                assistant.set(Some(status));
            }
            ticks = ticks.wrapping_add(1);
            gloo_timers::future::sleep(REFRESH).await;
        }
    });

    // Folded down to icons, or open with labels. A preference about this screen, so it's
    // remembered by the browser rather than by Irori.
    let folded = RwSignal::new(devices::stored(SIDEBAR_KEY).as_deref() == Some("folded"));
    Effect::new(move |_| {
        devices::remember(SIDEBAR_KEY, if folded.get() { "folded" } else { "open" })
    });

    // Remembered the same way, and for the same reason: it's about this screen, not the home.
    let motion = RwSignal::new(devices::stored(MOTION_KEY).as_deref() != Some("off"));
    Effect::new(move |_| {
        let on = if motion.get() { "on" } else { "off" };
        devices::remember(MOTION_KEY, on);
        // Page changes are drawn by the browser over the whole document, outside `.shell`, so
        // the switch has to reach `<html>` as well.
        if let Some(root) = document().document_element() {
            let _ = root.set_attribute("data-motion", on);
        }
    });
    provide_context(Motion(motion));

    provide_context(transition::Travelling(RwSignal::new(None)));
    // Every new reading: numbers on the page count to where they're going.
    count::watch(move || live.home.track());

    let sidebar = NodeRef::<leptos::html::Aside>::new();

    view! {
        {move || must_sign_in.get().then(|| view! { <welcome::SignIn /> })}
        {move || (welcoming.get() && !must_sign_in.get()).then(|| view! { <welcome::Welcome /> })}
        <Router>
            <div
                class="shell"
                class:folded=move || folded.get()
                // Kept, and put out of sight, while the home is asking who is there.
                class:locked=move || must_sign_in.get()
                data-motion=move || if motion.get() { "on" } else { "off" }
            >
                <aside class="sidebar" node_ref=sidebar>
                    <SidebarGlide sidebar=sidebar />
                    <div class="sidebar-top">
                        <Mark />
                        <button
                            type="button"
                            class="fold"
                            aria-label=move || if folded.get() { "Open the sidebar" } else { "Fold the sidebar" }
                            aria-expanded=move || (!folded.get()).to_string()
                            on:click=move |_| folded.update(|folded| *folded = !*folded)
                        >
                            <svg viewBox="0 0 24 24" aria-hidden="true">
                                <path d="M15 5 8 12l7 7" fill="none" stroke="currentColor"
                                    stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
                            </svg>
                        </button>
                    </div>
                    <nav aria-label="Sections">
                        {SECTIONS
                            .iter()
                            .map(|(href, label, icon)| view! {
                                <A href=*href attr:title=*label>
                                    <svg viewBox="0 0 24 24" aria-hidden="true" inner_html=*icon></svg>
                                    <span class="label">{*label}</span>
                                </A>
                            })
                            .collect_view()}
                        <AppLinks />
                    </nav>
                    <div class="sidebar-bottom">
                        <A href="/settings" attr:class="settings-link" attr:title="Settings">
                            // A cog: the thing that isn't a device, but that the home runs on.
                            <svg viewBox="0 0 24 24" aria-hidden="true">
                                <circle cx="12" cy="12" r="3.2" fill="none" stroke="currentColor"
                                    stroke-width="1.8" />
                                <path d="M12 2.5v3M12 18.5v3M2.5 12h3M18.5 12h3M5.3 5.3l2.1 2.1M16.6 16.6l2.1 2.1M5.3 18.7l2.1-2.1M16.6 7.4l2.1-2.1"
                                    stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
                            </svg>
                            <span class="label">"Settings"</span>
                        </A>
                        <span class="live" title=move || if live.trouble.get().is_none() { "Live" } else { "No answer" }>
                            <span class="dot" class:ok=move || live.trouble.get().is_none()></span>
                            <span class="label">
                                {move || if live.trouble.get().is_none() { "Live" } else { "No answer" }}
                            </span>
                        </span>
                    </div>
                </aside>

                <Page live=live />
                <assistant::Popover />
                // Over everything, the chat window included: it is opened from inside one.
                {move || model_log.get().then(|| view! {
                    <log_window::LogWindow
                        source=log_window::Source::Model
                        on_close=move || model_log.set(false)
                    />
                })}
            </div>
        </Router>
    }
}

/// The page beside the sidebar.
///
/// Its own component so that it can ask the router where we are, which only works inside
/// `<Router>`. What it asks for: the Floorplan is a canvas rather than a column of cards, so it
/// takes the whole width the sidebar leaves and sets its own margins.
#[component]
fn Page(live: Live) -> impl IntoView {
    let location = use_location();
    transition::watch(location.pathname);
    view! {
        <main class:full=move || {
            let path = location.pathname.get();
            path == "/floorplan" || path.starts_with("/apps/")
        }>
            {move || live.trouble.get().map(|why| view! { <p class="banner">{why}</p> })}
            <Routes fallback=NotFound transition=true>
                // The start screen moved to the head of Settings, so that is where a bare
                // address lands.
                <Route path=path!("/") view=|| view! { <Redirect path="/settings" /> } />
                <Route path=path!("/floorplan") view=floorplan::Floorplan />
                <Route path=path!("/devices") view=devices::Devices />
                <Route path=path!("/devices/:id") view=device::DevicePage />
                <Route path=path!("/extensions") view=extensions::Extensions />
                <Route path=path!("/assistant") view=assistant::Page />
                <Route path=path!("/settings") view=settings::Settings />
                <Route path=path!("/apps/:id") view=app_frame::AppPage />
                <Route path=path!("/apps/:id/*rest") view=app_frame::AppPage />
            </Routes>
        </main>
    }
}

/// How long the mark takes to catch light when a model becomes ready: the last letter's
/// delay and its burn, as `index.html` times them (`letter-catch`).
const IGNITE: Duration = Duration::from_millis(1700);

/// The top of the sidebar: IroriOS's mark and name, which is also the way to its assistant.
///
/// Until a model is ready it is only the name, and nothing to click. The moment one becomes
/// ready the name catches, letter by letter from the left, while the ember in the mark flares:
/// something here just came alive. Once that has burned down it is a link to the chat about
/// the whole home, wearing the same spark every Ask does: after the name, or on the mark's
/// corner when the sidebar is folded.
#[component]
fn Mark() -> impl IntoView {
    let Assistant(assistant) = expect_context::<Assistant>();
    let ready = Memo::new(move |_| assistant.get().map(|status| status.ready));
    let igniting = RwSignal::new(false);
    // Only a change seen happening catches light: a page that opens with a model already
    // set has nothing new to announce.
    Effect::new(move |before: Option<Option<bool>>| {
        let now = ready.get();
        if before == Some(Some(false)) && now == Some(true) && !count::still() {
            igniting.set(true);
            // A timer and not the animation's own end: with motion off there is no animation
            // to end, and the name must never be left unclickable.
            spawn_local(async move {
                gloo_timers::future::sleep(IGNITE).await;
                igniting.set(false);
            });
        }
        now
    });
    let inside = move || {
        view! {
            // Mark A (assets/irori-mark-a-mono.svg): frame follows the text, ember stays.
            <svg class="mark-logo" viewBox="0 0 48 48" role="img" aria-label="IroriOS">
                <rect x="2" y="2" width="44" height="44" rx="2.5" fill="none"
                    stroke="currentColor" stroke-width="4" />
                <rect class="mark-ember" x="17" y="17" width="14" height="14" rx="1"
                    fill="#c4552b" />
            </svg>
            <span class="label mark-letters">
                {"IroriOS"
                    .chars()
                    .enumerate()
                    .map(|(i, letter)| view! { <span style=format!("--i: {i}")>{letter}</span> })
                    .collect_view()}
            </span>
        }
    };
    move || {
        if ready.get() == Some(true) && !igniting.get() {
            view! {
                <A href="/assistant" attr:class="mark" attr:title="Ask IroriOS">
                    {inside()}
                    <svg class="mark-spark" viewBox="0 0 24 24" aria-hidden="true">
                        <path d="M12 3l1.9 5.6L19.5 10l-5.6 1.9L12 17.5l-1.9-5.6L4.5 10l5.6-1.4z"
                            fill="currentColor" />
                    </svg>
                    <span class="visually-hidden">"Assistant"</span>
                </A>
            }
            .into_any()
        } else {
            view! {
                <span
                    class="mark"
                    class:igniting=move || igniting.get()
                    title=move || {
                        if igniting.get() {
                            "IroriOS"
                        } else {
                            "IroriOS. Its assistant is set up in Settings."
                        }
                    }
                >
                    {inside()}
                </span>
            }
            .into_any()
        }
    }
}

/// Where the sidebar's folded-or-open state is remembered.
const SIDEBAR_KEY: &str = "irori.sidebar";

/// Where turning motion off in Settings is remembered.
const MOTION_KEY: &str = "irori.motion";

/// The pages the sidebar links to, besides Settings: address, name, and an icon drawn in 24×24
/// strokes. Written here, not taken from any extension, so `inner_html` only ever holds these
/// literals.
const SECTIONS: [(&str, &str, &str); 3] = [
    (
        "/floorplan",
        "Floorplan",
        r#"<path d="M3 4.5h18v15H3z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/><path d="M10 4.5v7M10 11.5h11M15 11.5v8M3 15.5h4" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>"#,
    ),
    (
        "/devices",
        "Devices",
        r#"<rect x="6" y="6" width="12" height="12" rx="2" fill="none" stroke="currentColor" stroke-width="1.8"/><path d="M9 3v3M15 3v3M9 18v3M15 18v3M3 9h3M3 15h3M18 9h3M18 15h3" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>"#,
    ),
    (
        "/extensions",
        "Extensions",
        r#"<rect x="4" y="4" width="7" height="7" rx="1.2" fill="none" stroke="currentColor" stroke-width="1.8"/><rect x="13" y="4" width="7" height="7" rx="1.2" fill="none" stroke="currentColor" stroke-width="1.8"/><rect x="4" y="13" width="7" height="7" rx="1.2" fill="none" stroke="currentColor" stroke-width="1.8"/><rect x="13" y="13" width="7" height="7" rx="1.2" fill="none" stroke="currentColor" stroke-width="1.8"/>"#,
    ),
];

/// Extensions' own pages (`docs/specs/automations.md` §B3), after Irori's: each running
/// extension with an app gets an entry, with its own icon as an image — never inlined, so an
/// extension's SVG can't run script in the shell.
#[component]
fn AppLinks() -> impl IntoView {
    let live = expect_context::<Live>();
    let Session(session) = expect_context::<Session>();
    let apps = Memo::new(move |_| {
        // An extension's page talks to its engine, which is running the home: an owner's.
        if !session.with(|s| s.as_ref().is_none_or(|s| s.owner)) {
            return Vec::new();
        }
        live.home.with(|home| {
            home.extensions
                .iter()
                .filter(|(_, extension)| matches!(extension.state.as_str(), "running" | "degraded"))
                .filter_map(|(id, extension)| {
                    extension
                        .app
                        .as_ref()
                        .map(|app| (id.to_string(), app.label.clone(), extension.has_icon))
                })
                .collect::<Vec<_>>()
        })
    });
    view! {
        <For each=move || apps.get() key=|app| app.clone() let:app>
            {
                let (id, label, has_icon) = app;
                let title = label.clone();
                view! {
                    <A href=format!("/apps/{id}/") attr:title=title>
                        {if has_icon {
                            view! {
                                <img src=format!("/api/dev/extensions/{id}/icon.svg") alt="" />
                            }
                            .into_any()
                        } else {
                            view! {
                                <svg viewBox="0 0 24 24" aria-hidden="true">
                                    <rect x="4" y="4" width="16" height="16" rx="2" fill="none"
                                        stroke="currentColor" stroke-width="1.8" />
                                </svg>
                            }
                            .into_any()
                        }}
                        <span class="label">{label}</span>
                    </A>
                }
            }
        </For>
    }
}

/// The sidebar's highlight, gliding to the page being shown. Its own component because it asks
/// the router where the page is, which only works inside `<Router>`.
#[component]
fn SidebarGlide(sidebar: NodeRef<leptos::html::Aside>) -> impl IntoView {
    let location = use_location();
    // Not the mark, though it is a link too, to the assistant: it sits in a row with the fold
    // button, and shows that it is the page being read in its own way.
    glide::glide(
        sidebar,
        r#"nav a[aria-current="page"], .settings-link[aria-current="page"]"#,
        move || location.pathname.track(),
    );
    view! { <span class="glide" aria-hidden="true"></span> }
}

#[component]
fn NotFound() -> impl IntoView {
    view! {
        <section class="card">
            <h1>"There's no page here"</h1>
            <p class="muted">
                "IroriOS has a Floorplan, a Devices page, an Extensions page, an assistant "
                "and a Settings page. The rest is still to come."
            </p>
            <p><A href="/settings" attr:class="quiet-button">"Back to Settings"</A></p>
        </section>
    }
}

/// Asks the core for the home again, now, rather than waiting for the next poll.
///
/// Renaming and moving things change more than the thing that was changed — an entity with no
/// name of its own follows its device, and an area that goes away unplaces everything in it — so
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

/// Sends a command, then puts the entity's new state on the page without waiting for the next
/// refresh: the core answers once the protocol has confirmed.
fn send_command(
    entity_id: EntityId,
    home: RwSignal<Home>,
    busy: RwSignal<BTreeSet<EntityId>>,
    failures: RwSignal<BTreeMap<EntityId, String>>,
    run: impl Future<Output = Result<Option<EntityState>, String>> + 'static,
) {
    busy.update(|busy| {
        busy.insert(entity_id.clone());
    });
    spawn_local(async move {
        match run.await {
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
