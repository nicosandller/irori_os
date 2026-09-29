//! The Automations page (`docs/specs/flows.md`): runs inside Irori's shell, in a sandboxed frame,
//! and talks to it — and through it to the engine — over the bridge (irori-ui-kit).
//!
//! Two views: the list of flows, and a flow's editor (canvas, palette, and a panel for the
//! selected node, runs, tests, "why didn't it fire?", and versions). Where you are lives in the
//! address after `#`, so the shell's back button and deep links work.

mod api;
mod canvas;
mod editor;
mod inspector;
mod list;
mod model;
mod panels;
mod time;
mod widgets;

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::time::Duration;

use irori_types::{Device, Entity, EntityId, EntityState};
use irori_ui_kit::page::{Bridge, Event, apply_theme};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;

thread_local! {
    static BRIDGE: OnceCell<Bridge> = const { OnceCell::new() };
}

/// The page's line to the shell.
pub fn bridge() -> Bridge {
    BRIDGE.with(|cell| {
        cell.get_or_init(|| {
            Bridge::connect(|event| match event {
                Event::Theme(theme) => apply_theme(&theme),
                Event::Path(path) => set_hash(&path),
            })
        })
        .clone()
    })
}

/// Where the page is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    List,
    Flow(String),
    New,
}

impl Route {
    fn parse(path: &str) -> Self {
        let path = path.trim_start_matches('#').trim_start_matches('/');
        match path.split('/').collect::<Vec<_>>().as_slice() {
            ["flows", id, ..] if !id.is_empty() => Self::Flow((*id).to_owned()),
            ["new"] => Self::New,
            _ => Self::List,
        }
    }

    fn path(&self) -> String {
        match self {
            Self::List => String::new(),
            Self::Flow(id) => format!("flows/{id}"),
            Self::New => "new".into(),
        }
    }
}

fn set_hash(path: &str) {
    let _ = window().location().set_hash(&format!("/{path}"));
}

/// Goes to `route`, and tells the shell so its address bar follows.
pub fn go(route: Route) {
    let path = route.path();
    set_hash(&path);
    spawn_local(async move { bridge().navigate(&path).await });
}

/// The home as the page knows it: what exists and what it's doing. Refreshed every couple of
/// seconds while the page is open.
#[derive(Debug, Clone, Copy)]
pub struct Home {
    pub entities: RwSignal<Vec<Entity>>,
    pub devices: RwSignal<Vec<Device>>,
    pub states: RwSignal<BTreeMap<EntityId, EntityState>>,
}

impl Home {
    /// A person's name for an entity: its own, or its id.
    pub fn name(&self, id: &EntityId) -> String {
        self.entities.with_untracked(|entities| {
            entities
                .iter()
                .find(|entity| &entity.id == id)
                .map(|entity| entity.name.to_string())
                .unwrap_or_else(|| id.to_string())
        })
    }
}

#[derive(Deserialize)]
struct Registry {
    entities: Vec<Entity>,
    #[serde(default)]
    devices: Vec<Device>,
}

fn main() {
    // A bug shouldn't leave a blank frame: say what broke, on the page as well as the console.
    std::panic::set_hook(Box::new(|info| {
        console_error_panic_hook::hook(info);
        // Tell the shell too: the frame's own console is out of sight.
        if let Ok(Some(parent)) = window().parent() {
            let message =
                serde_json::json!({ "irori": 1, "id": 0, "op": "log", "args": info.to_string() });
            let _ = parent.post_message(&message.to_string().into(), "*");
        }
        if let Some(body) = document().body() {
            let note = document().create_element("div").ok();
            if let Some(note) = note {
                let _ = note.set_attribute(
                    "style",
                    "position:fixed;left:1rem;right:1rem;top:1rem;z-index:10;padding:.8rem 1rem;\
                     border:1px solid var(--error);border-radius:.5rem;background:var(--card);\
                     color:var(--error);font-size:.85rem",
                );
                note.set_text_content(Some(&format!(
                    "Something went wrong on this page; reload it to carry on. ({info})"
                )));
                let _ = body.append_child(&note);
            }
        }
    }));
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    let route = RwSignal::new(Route::parse(
        &window().location().hash().unwrap_or_default(),
    ));
    let listening = window_event_listener(leptos::ev::hashchange, move |_| {
        route.set(Route::parse(
            &window().location().hash().unwrap_or_default(),
        ));
    });
    on_cleanup(move || listening.remove());

    let home = Home {
        entities: RwSignal::new(Vec::new()),
        devices: RwSignal::new(Vec::new()),
        states: RwSignal::new(BTreeMap::new()),
    };
    provide_context(home);
    let trouble = RwSignal::new(None::<String>);

    spawn_local(async move {
        match bridge().hello().await {
            Ok(hello) => {
                apply_theme(&hello.theme);
                if !hello.path.is_empty() && Route::parse(&hello.path) != route.get_untracked() {
                    set_hash(&hello.path);
                }
            }
            Err(error) => trouble.set(Some(error)),
        }
        loop {
            if let Ok(registry) = bridge().registry::<Registry>().await {
                if registry.entities != home.entities.get_untracked() {
                    home.entities.set(registry.entities);
                }
                if registry.devices != home.devices.get_untracked() {
                    home.devices.set(registry.devices);
                }
            }
            if let Ok(states) = bridge().states::<Vec<EntityState>>().await {
                let states: BTreeMap<EntityId, EntityState> = states
                    .into_iter()
                    .map(|state| (state.entity_id.clone(), state))
                    .collect();
                if states != home.states.get_untracked() {
                    home.states.set(states);
                }
            }
            gloo_timers::future::sleep(Duration::from_secs(2)).await;
        }
    });

    view! {
        {move || trouble.get().map(|why| view! {
            <div class="list"><div class="problems-box">
                "This page has to be opened from Irori's sidebar: " {why}
            </div></div>
        })}
        {move || match route.get() {
            Route::List => view! { <list::List /> }.into_any(),
            Route::Flow(id) => view! { <editor::Editor id=id is_new=false /> }.into_any(),
            Route::New => view! { <editor::Editor id=String::new() is_new=true /> }.into_any(),
        }}
    }
}
