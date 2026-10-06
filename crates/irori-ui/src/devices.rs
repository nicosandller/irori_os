//! The Devices page: everything in the home, as devices or as the entities they provide, in
//! groups that fold, with a switch for the things that can be switched.

use std::collections::{BTreeMap, BTreeSet};

use irori_types::{
    Availability, BinarySensorCapabilities, BinarySensorClass, Capabilities, Device, DeviceId,
    Entity, EntityId, EntityState, ExtensionId, LightTurnOn, SensorCapabilities, SensorClass,
    SensorValue, State,
};
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;

use crate::api::Home;

mod controls;
mod grouping;
mod tables;

pub(crate) use self::controls::{
    climate_words, fan_words, fill, humidifier_words, lock_words, media_player_words, number,
    opening_words, unit_of, water_heater_words, wording,
};
pub(crate) use self::controls::{control, kept};

/// What a row needs to show a command on its way and what came back from it.
#[derive(Debug, Clone, Copy)]
pub struct Controls {
    /// Entities with a command in flight.
    pub busy: RwSignal<BTreeSet<EntityId>>,
    /// Why the last command on an entity was refused, until the next one succeeds.
    pub failures: RwSignal<BTreeMap<EntityId, String>>,
    /// Ask an entity to turn on (`true`) or off (`false`).
    pub set_on: Callback<(EntityId, bool)>,
    /// Ask a light to come on with this brightness, color temperature or color.
    pub set_light: Callback<(EntityId, LightTurnOn)>,
    /// Ask an entity for one of its kind's actions, with that action's data: a button's
    /// `press`, a number's `set_value`, a cover's `set_position`.
    pub act: Callback<(EntityId, &'static str, Option<serde_json::Value>)>,
}

/// Whether a device matches the shared Devices/Entities search: name, id, area, make, model.
fn matches_device(home: &Home, device: &Device, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let haystack = [
        device.name.to_string(),
        device.id.to_string(),
        device.protocol.to_string(),
        device
            .description
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        device.manufacturer.clone().unwrap_or_default(),
        device.model.clone().unwrap_or_default(),
        home.area_of(device).unwrap_or_default(),
    ];
    haystack
        .iter()
        .any(|field| field.to_lowercase().contains(needle))
}

/// Which view the page shows. Remembered per browser, because it's a preference about reading
/// rather than anything Irori needs to know.
const VIEW_KEY: &str = "irori.devices.view";

/// The three ways to look at what's in the home.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Showing {
    /// One row per device. The default: a device is the thing a person bought and put somewhere.
    Devices,
    /// Every entity with its reading and its switch.
    Entities,
    /// Values Irori keeps itself, rather than a device reporting them.
    Helpers,
}

impl Showing {
    const ALL: [Showing; 3] = [Showing::Devices, Showing::Entities, Showing::Helpers];

    fn key(self) -> &'static str {
        match self {
            Showing::Devices => "devices",
            Showing::Entities => "entities",
            Showing::Helpers => "helpers",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Showing::Devices => "Devices",
            Showing::Entities => "Entities",
            Showing::Helpers => "Helpers",
        }
    }
}

#[component]
pub fn Devices() -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let controls = expect_context::<Controls>();
    let filter = RwSignal::new(String::new());
    let adding = RwSignal::new(false);
    let adding_helper = RwSignal::new(false);
    let showing = RwSignal::new(remembered_view());
    provide_context(HelperTrouble(RwSignal::new(None)));
    let waiting = crate::waiting::everything_waiting(live);
    // How many devices extensions have found that aren't in the home, key-waiting ones included.
    let found = Memo::new(move |_| {
        live.home.with(|home| {
            home.held
                .iter()
                .filter(|device| device.protocol != HELPERS)
                .count()
        }) + crate::waiting::count(&waiting.get())
    });

    Effect::new(move |_| remember(VIEW_KEY, showing.get().key()));

    view! {
        <div class="page-head">
            <h1>"Devices"</h1>
            {crate::segmented::segmented(
                "What to show",
                Showing::ALL.into_iter().map(|view| (view, view.label())).collect(),
                showing.into(),
                move |view| showing.set(view),
            )}
            // Helpers aren't devices and don't arrive through an extension — making one is a
            // name and nothing else — so the Helpers tab gets its own button, not a card in the
            // add-a-device flow pretending a helper is something an extension found.
            {move || if showing.get() == Showing::Helpers {
                view! {
                    <button
                        type="button"
                        class="add"
                        on:click=move |_| adding_helper.set(true)
                    >
                        "+ Add helper"
                    </button>
                }
                    .into_any()
            } else {
                // What extensions have found is only ever listed under "+ Add device": nothing
                // joins the home on its own, so the page shows the home and the button says how
                // much is waiting to be looked at.
                view! {
                    <button type="button" class="add" on:click=move |_| adding.set(true)>
                        "+ Add device"
                        {move || {
                            let found = found.get();
                            (found > 0).then(|| view! {
                                <span
                                    class="count-badge"
                                    aria-label=format!("{found} found")
                                >
                                    {found}
                                </span>
                            })
                        }}
                    </button>
                }
                    .into_any()
            }}
        </div>

        // Something found and waiting is worth saying even with the panel closed: it's the one
        // thing on this page only a person can fix.
        {move || {
            let waiting = crate::waiting::count(&waiting.get());
            (waiting > 0 && !adding.get()).then(|| view! {
                <p class="nudge">
                    {format!(
                        "{waiting} device{} found that Irori can't use yet. ",
                        if waiting == 1 { "" } else { "s" },
                    )}
                    <button type="button" class="quiet-button" on:click=move |_| adding.set(true)>
                        "See what they need"
                    </button>
                </p>
            })
        }}
        {move || adding.get().then(|| view! {
            <crate::modal::Modal title="Add a device".to_owned() on_close=move || adding.set(false)>
                <AddDevice />
            </crate::modal::Modal>
        })}

        {move || (showing.get() != Showing::Helpers).then(|| view! {
            <input
                class="filter"
                type="search"
                placeholder="Filter by name, id, area or make"
                aria-label="Filter"
                prop:value=filter
                on:input:target=move |ev| filter.set(ev.target().value())
            />
        })}

        // The two lists watch the home themselves, and only what they're made of (`tables.rs`).
        // Helpers are few and hold nothing a person is typing, so they're drawn with each reading.
        {move || match showing.get() {
            Showing::Devices => view! { <tables::DeviceTable filter=filter /> }.into_any(),
            Showing::Entities => view! { <tables::EntityTable filter=filter /> }.into_any(),
            Showing::Helpers => helpers(&live.home.get(), controls),
        }}
        {move || adding_helper.get().then(|| view! {
            <crate::modal::Modal
                title="Add a helper".to_owned()
                on_close=move || adding_helper.set(false)
            >
                <AddToggle on_added=move || adding_helper.set(false) />
            </crate::modal::Modal>
        })}
    }
}

/// A protocol's icon, or its initial when it has none. Always an `<img>`: an extension's SVG
/// is loaded as an image, where it can't run script (`docs/specs/extensions.md`).
pub fn icon(protocol: &str, has_icon: bool) -> AnyView {
    if has_icon {
        view! {
            <img
                class="protocol-icon"
                src=format!("/api/dev/extensions/{protocol}/icon.svg")
                alt=""
                width="20"
                height="20"
            />
        }
        .into_any()
    } else {
        let initial = protocol
            .chars()
            .next()
            .map(|c| c.to_ascii_uppercase().to_string())
            .unwrap_or_default();
        view! { <span class="protocol-icon letter" aria-hidden="true">{initial}</span> }.into_any()
    }
}

/// The helpers in the home: the switches Irori keeps itself, each with its switch and a way to
/// remove it. Redrawn with every reading, so it holds nothing a person is typing.
fn helpers(home: &Home, controls: Controls) -> AnyView {
    let states: BTreeMap<_, _> = home.states.iter().map(|s| (&s.entity_id, s)).collect();
    let mut toggles: Vec<_> = home
        .entities
        .iter()
        .filter(|entity| entity.protocol.as_str() == HELPERS)
        .filter_map(|entity| {
            let id = entity
                .unique_id
                .as_str()
                .strip_prefix("toggle-")?
                .to_owned();
            Some((id, entity.clone(), states.get(&entity.id).copied().cloned()))
        })
        .collect();
    toggles.sort_by(|a, b| a.1.name.as_str().cmp(b.1.name.as_str()));
    if toggles.is_empty() {
        return view! {
            <section class="card">
                <h2>"No helpers yet"</h2>
                <p class="muted">
                    "A helper is a value Irori keeps itself rather than a device reporting it: a "
                    "switch for \"guests are over\" or \"holiday mode\", say. It stays as it was "
                    "left through restarts, and rules (M1.4) will be able to read and flip it. "
                    "Make one with \"+ Add helper\" above."
                </p>
            </section>
        }
        .into_any();
    }
    view! {
        <section class="card">
            <ul class="room-devices">
                {toggles
                    .into_iter()
                    .map(|(id, entity, state)| {
                        let name = entity.name.to_string();
                        let shown = entity.id.to_string();
                        let offline = state
                            .as_ref()
                            .is_some_and(|s| s.availability == Availability::Unavailable);
                        let knob = control(&entity, state.as_ref(), offline, controls);
                        view! {
                            <li>
                                <span class="name">{name.clone()}</span>
                                <code class="muted small">{shown}</code>
                                <span class="room-actions">
                                    {knob}
                                    <ToggleActions id=id entity_id=entity.id.clone() name=name />
                                </span>
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        </section>
    }
    .into_any()
}

/// The protocol that keeps helpers.
const HELPERS: &str = "helpers";

/// Renaming and removing a toggle. A toggle has one name, kept with its definition in
/// `extensions/helpers.toml`; renaming it here changes that one (ROADMAP D36).
#[component]
fn ToggleActions(id: String, entity_id: EntityId, name: String) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let trouble = expect_context::<HelperTrouble>().0;
    let rename = {
        let name = name.clone();
        move |_| {
            let Ok(Some(typed)) =
                window().prompt_with_message_and_default("Rename this toggle", &name)
            else {
                return;
            };
            if typed.trim() == name {
                return;
            }
            let Some(named) = crate::settings::named(typed, trouble) else {
                return;
            };
            let entity_id = entity_id.clone();
            spawn_local(async move {
                match crate::api::rename_entity(&entity_id, Some(named)).await {
                    Ok(()) => {
                        trouble.set(None);
                        crate::refresh(live);
                    }
                    Err(why) => trouble.set(Some(why)),
                }
            });
        }
    };
    let remove = move |_| {
        if !window()
            .confirm_with_message(&format!(
                "Remove {name}? Anything that reads it will find it gone."
            ))
            .unwrap_or(false)
        {
            return;
        }
        let id = id.clone();
        spawn_local(async move {
            match crate::api::remove_toggle(&id).await {
                Ok(()) => {
                    trouble.set(None);
                    crate::refresh(live);
                }
                Err(why) => trouble.set(Some(why)),
            }
        });
    };
    view! {
        <button type="button" on:click=rename>"Rename"</button>
        <button type="button" class="danger" on:click=remove>"Remove"</button>
    }
}

/// Why the last change to a helper was refused, shared by the form and the rows.
#[derive(Debug, Clone, Copy)]
struct HelperTrouble(RwSignal<Option<String>>);

/// Making a toggle. Outside the list, which redraws with every reading, so what's being typed
/// survives it (ROADMAP D33).
#[component]
fn AddToggle(#[prop(into)] on_added: Callback<()>) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let trouble = expect_context::<HelperTrouble>().0;
    let name = RwSignal::new(String::new());
    let add = move || {
        let Some(named) = crate::settings::named(name.get(), trouble) else {
            return;
        };
        name.set(String::new());
        spawn_local(async move {
            match crate::api::add_toggle(named).await {
                Ok(()) => {
                    trouble.set(None);
                    crate::refresh(live);
                    on_added.run(());
                }
                Err(why) => trouble.set(Some(why)),
            }
        });
    };
    view! {
        <p class="muted">
            "A helper is a value Irori keeps itself rather than a device reporting it: a switch "
            "for \"guests are over\" or \"holiday mode\", say. It stays as it was left through "
            "restarts, and rules (M1.4) will be able to read and flip it."
        </p>
        {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}
        <form
            class="inline-form"
            on:submit=move |ev| {
                ev.prevent_default();
                add();
            }
        >
            <input
                type="text"
                aria-label="Name of the new toggle"
                placeholder="Guests are over"
                prop:value=name
                on:input:target=move |ev| name.set(ev.target().value())
            />
            <button type="submit" class="add" disabled=move || name.get().trim().is_empty()>
                "Add helper"
            </button>
        </form>
    }
}

/// A device's battery, if one of its entities reports one: a percentage from a battery sensor,
/// or low/ok from a battery binary sensor. `None` when it doesn't have one, which is most
/// mains-powered things.
fn battery_of(home: &Home, device: &DeviceId) -> Option<String> {
    home.entities
        .iter()
        .filter(|entity| entity.device_id.as_ref() == Some(device))
        .filter(|entity| {
            matches!(
                entity.capabilities,
                Capabilities::Sensor(SensorCapabilities {
                    device_class: Some(SensorClass::Battery),
                    ..
                }) | Capabilities::BinarySensor(BinarySensorCapabilities {
                    device_class: Some(BinarySensorClass::Battery),
                })
            )
        })
        .find_map(|entity| {
            let state = home
                .states
                .iter()
                .find(|state| state.entity_id == entity.id)?;
            battery_reading(entity, state)
        })
}

fn battery_reading(entity: &Entity, state: &EntityState) -> Option<String> {
    match (&entity.capabilities, state.state.as_ref()) {
        (
            Capabilities::Sensor(SensorCapabilities {
                device_class: Some(SensorClass::Battery),
                unit,
                ..
            }),
            Some(State::Sensor(sensor)),
        ) => Some(match &sensor.value {
            SensorValue::Number(n) => {
                format!("{}{}", number(*n), unit.as_deref().unwrap_or("%"))
            }
            SensorValue::Text(text) => text.clone(),
        }),
        (
            Capabilities::BinarySensor(BinarySensorCapabilities {
                device_class: Some(BinarySensorClass::Battery),
            }),
            Some(State::BinarySensor(sensor)),
        ) => Some(if sensor.on { "Low" } else { "OK" }.to_owned()),
        _ => None,
    }
}

/// The view this browser was last shown. Browser storage can be unavailable or refused, and it
/// only holds a preference, so anything unexpected just means the default.
fn remembered_view() -> Showing {
    let saved = stored(VIEW_KEY);
    Showing::ALL
        .into_iter()
        .find(|view| saved.as_deref() == Some(view.key()))
        .unwrap_or(Showing::Devices)
}

pub fn stored(key: &str) -> Option<String> {
    window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|storage| storage.get_item(key).ok().flatten())
}

pub fn remember(key: &str, value: &str) {
    if let Ok(Some(storage)) = window().local_storage() {
        let _ = storage.set_item(key, value);
    }
}

/// Where devices come from: pick the extension a device speaks, see everything it has found,
/// and add the ones that belong in the home.
///
/// There's no "scan now" button because there's nothing to scan on demand: protocols that find
/// devices are always listening. Nothing an extension finds joins the home on its own — this is
/// the only way in (`docs/specs/config.md` §3.2).
#[component]
fn AddDevice() -> impl IntoView {
    let live = expect_context::<crate::Live>();
    // Which extension's screen is open, if any — at most one at a time.
    let selected = RwSignal::new(None::<ExtensionId>);
    // The one just left, so going back lands on its card — the card the screen grew out of.
    let came_from = StoredValue::new(None::<ExtensionId>);
    // The extensions and how much each has — not the readings. A redraw on every sensor report
    // would throw away a key being pasted into a form below (D33).
    let extensions = Memo::new(move |_| {
        let home = live.home.get();
        home.extensions
            .iter()
            // Helpers are an extension for the core's own reasons (D40), but nothing here finds
            // a helper: you make one, by naming it. That has its own button on the Helpers tab.
            .filter(|(id, _)| id.as_str() != HELPERS)
            // Only extensions that bring devices: an automation engine or a page has none to
            // find, so offering it here would be a door to nowhere.
            .filter(|(_, extension)| !extension.entity_kinds.is_empty())
            .map(|(id, extension)| {
                let here = home
                    .devices
                    .iter()
                    .filter(|device| device.protocol.as_str() == id.as_str())
                    .count();
                let found = home
                    .held
                    .iter()
                    .filter(|device| device.protocol == id.as_str())
                    .count()
                    + extension.waiting.len();
                (id.clone(), extension.clone(), here, found)
            })
            .collect::<Vec<_>>()
    });
    let ids = Memo::new(move |_| {
        extensions.with(|all| all.iter().map(|(id, ..)| id.clone()).collect::<Vec<_>>())
    });
    // Going somewhere inside the window is a change of its own (`transition.rs`): the step being
    // left slides away and the card you picked grows into the next step's heading, or shrinks
    // back into its card.
    let go = move |to: Option<ExtensionId>| {
        let kind = crate::transition::step(to.is_some());
        if to.is_none() {
            came_from.set_value(selected.get_untracked());
        }
        crate::transition::around(kind, move || selected.set(to));
    };

    view! {
        {move || match selected.get() {
            // Step one: which extension does this device speak? Cards rather than a list that
            // expands in place — the second step is a screen of its own, and a row that grows
            // while the rows below it stay put reads as a disclosure, not as going somewhere.
            None => view! {
                <section class="add-device add-step">
                    <div class="add-intro">
                        <p class="muted">
                            "Irori doesn't talk to devices itself: each kind of device arrives "
                            "through an extension. Pick the one your device speaks to see what it "
                            "has found."
                        </p>
                        <A href="/extensions" attr:class="quiet-button">"Manage extensions"</A>
                    </div>
                    // Keyed by extension, so a card arrives once and then keeps up — what it
                    // has found and what's in the home change while the window is open.
                    <div class="protocol-cards">
                        <For
                            each=move || ids.get()
                            key=|id| id.clone()
                            children=move |id| {
                                let i = ids
                                    .with_untracked(|ids| ids.iter().position(|known| *known == id))
                                    .unwrap_or(0);
                                let back_here = came_from.get_value().as_ref() == Some(&id);
                                protocol_card(i, id, extensions, back_here, go)
                            }
                        />
                    </div>
                </section>
            }
                .into_any(),
            // Step two: that one extension, and nothing else — what it can be asked to do, what
            // it has found, and what of it is already in the home.
            Some(id) => view! { <ProtocolStep id=id on_back=move || go(None) /> }.into_any(),
        }}
    }
}

/// One extension, as something to press: its icon, its name, and what it has.
fn protocol_card(
    i: usize,
    id: ExtensionId,
    extensions: Memo<Vec<(ExtensionId, crate::api::Extension, usize, usize)>>,
    back_here: bool,
    go: impl Fn(Option<ExtensionId>) + Copy + 'static,
) -> impl IntoView {
    let pick = {
        let id = id.clone();
        move |event: ev::MouseEvent| {
            // The card grows into the next step's heading: it takes the heading's name for the
            // change, and gives it up again once it's gone.
            crate::transition::name_target(&event, "add-hero");
            go(Some(id.clone()))
        }
    };
    let style = format!(
        "--i: {i}{}",
        if back_here {
            "; view-transition-name: add-hero"
        } else {
            ""
        }
    );
    let this = {
        let id = id.clone();
        move || {
            extensions.with(|all| {
                all.iter()
                    .find(|(known, ..)| *known == id)
                    .map(|(_, extension, here, found)| (extension.clone(), *here, *found))
            })
        }
    };
    let Some((extension, _, _)) = this() else {
        return ().into_any();
    };
    let counts = this.clone();
    let state = this.clone();

    view! {
        <button type="button" class="protocol-card" style=style on:click=pick>
            {icon(id.as_str(), extension.has_icon)}
            <span class="name">{extension.name.clone()}</span>
            <span class="badge">{how(&extension.iot_class)}</span>
            {move || state().map(|(extension, ..)| view! {
                <span
                    class="state"
                    class:ok=extension.state == "running"
                    class:wants-setup=extension.state == "needs_setup"
                >
                    {extension.state.replace('_', " ")}
                </span>
            })}
            <span class="protocol-card-counts">
                {move || counts().map(|(_, here, found)| view! {
                    {(found > 0).then(|| view! {
                        <span class="found-count">{format!("{found} found")}</span>
                    })}
                    <span class="muted small">{format!("{here} in your home")}</span>
                })}
            </span>
        </button>
    }
    .into_any()
}

/// How long a device that was just added stays drawn in the found list while it leaves it. A
/// little longer than the leaving itself (`--dur-layout`, after the check has shown for
/// `--dur-base`), so it's never cut short.
const LEAVING_FOR: std::time::Duration = std::time::Duration::from_millis(700);

/// What one extension has found, and what of it is already in the home: the screen you open to
/// add a device is also where you watch it arrive and move across.
#[component]
fn ProtocolStep(id: ExtensionId, #[prop(into)] on_back: Callback<()>) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let trouble = RwSignal::new(None::<String>);
    // The extension itself, for the heading: only the parts the heading shows, so a reading
    // arriving doesn't redraw it.
    //
    // Without how long its open actions have left: that number is different at every reading,
    // and everything drawn from this would be redrawn with it. `closing` keeps those instead.
    let extension = {
        let id = id.clone();
        Memo::new(move |_| {
            live.home.with(|home| {
                home.extensions
                    .iter()
                    .find(|(known, _)| **known == id)
                    .map(|(_, extension)| {
                        let mut extension = extension.clone();
                        extension.open_actions.clear();
                        extension
                    })
            })
        })
    };
    let closing = closing_times(live, id.clone());
    // Whether it opens its network for a while to find devices (Zigbee's permit join), rather
    // than hearing them announce themselves all the time. Only the first kind has a moment
    // when it's listening and a moment when it isn't.
    let opens = move || {
        extension.with(|e| {
            e.as_ref()
                .is_some_and(|e| e.actions.iter().any(|action| action.seconds.is_some()))
        })
    };
    let open_now = move || closing.with(|closing| !closing.is_empty());
    let unpairs = move || extension.with(|e| e.as_ref().is_some_and(|e| e.unpairs));
    // The found device being unpaired, while its window is up.
    let unpairing = RwSignal::new(None::<crate::api::HeldDevice>);
    let name = move || extension.with(|e| e.as_ref().map(|e| e.name.clone()).unwrap_or_default());
    let icon_there = move || extension.with(|e| e.as_ref().is_some_and(|e| e.has_icon));

    // What it has found that isn't in the home — what a device is, never what it's reporting, so
    // this changes when a device turns up or leaves, not every couple of seconds.
    let found = {
        let id = id.clone();
        Memo::new(move |_| {
            live.home.with(|home| {
                home.held
                    .iter()
                    .filter(|device| device.protocol == id.as_str())
                    .cloned()
                    .collect::<Vec<_>>()
            })
        })
    };
    // Devices just added, still drawn while they leave the found list.
    let leaving = RwSignal::new(Vec::<crate::api::HeldDevice>::new());
    // Devices with an add on its way.
    let busy = RwSignal::new(BTreeSet::<DeviceId>::new());
    // What the checkboxes have picked.
    let picked = RwSignal::new(BTreeSet::<DeviceId>::new());
    // The found list as drawn: what's found, and what's on its way out of it, in name order so a
    // device leaving keeps its place.
    let drawn = Memo::new(move |_| {
        let mut all = found.get();
        for device in leaving.get() {
            if !all.iter().any(|held| held.id == device.id) {
                all.push(device);
            }
        }
        all.sort_by(|a, b| {
            (a.name.as_str().to_lowercase(), a.id.as_str())
                .cmp(&(b.name.as_str().to_lowercase(), b.id.as_str()))
        });
        all
    });
    // The ones that can still be added: found, and not already on their way.
    let addable = Memo::new(move |_| {
        found
            .get()
            .into_iter()
            .map(|device| device.id)
            .filter(|id| !busy.with(|busy| busy.contains(id)))
            .collect::<Vec<_>>()
    });
    let chosen = Memo::new(move |_| {
        addable
            .get()
            .into_iter()
            .filter(|id| picked.with(|picked| picked.contains(id)))
            .collect::<Vec<_>>()
    });

    // What's already in the home: names and rooms, which change when a person changes them.
    let here = {
        let id = id.clone();
        Memo::new(move |_| {
            live.home.with(|home| {
                let mut here = home
                    .devices
                    .iter()
                    .filter(|device| device.protocol.as_str() == id.as_str())
                    .map(|device| {
                        let room = device
                            .area_id
                            .as_ref()
                            .and_then(|area| home.area(area))
                            .map(|area| area.name.to_string());
                        (device.id.clone(), device.name.to_string(), room)
                    })
                    .collect::<Vec<_>>();
                here.sort_by_key(|a| a.1.to_lowercase());
                here
            })
        })
    };

    // The first rows come in one after another as the step opens; one that turns up later
    // arrives on its own, with a glow that says it's new. After the step's first moment, a row
    // being drawn means a device just arrived.
    let opening = StoredValue::new(true);
    set_timeout(
        move || opening.set_value(false),
        std::time::Duration::from_millis(400),
    );

    // Adds each of `ids`, one after another. One that fails doesn't stop the rest, and the
    // trouble names it. Each one leaves the found list as its add comes back.
    let add = move |ids: Vec<DeviceId>| {
        if ids.is_empty() {
            return;
        }
        busy.update(|busy| busy.extend(ids.iter().cloned()));
        picked.update(|picked| picked.retain(|id| !ids.contains(id)));
        trouble.set(None);
        spawn_local(async move {
            let mut failed = Vec::new();
            for id in ids {
                let edit = crate::api::DeviceEdit {
                    added: Some(true),
                    ..Default::default()
                };
                let snapshot = found
                    .with_untracked(|found| found.iter().find(|device| device.id == id).cloned());
                match crate::api::edit_device(&id, &edit).await {
                    Ok(()) => {
                        if let Some(device) = snapshot {
                            leaving.update(|leaving| leaving.push(device));
                            let gone = id.clone();
                            set_timeout(
                                move || leaving.update(|leaving| leaving.retain(|d| d.id != gone)),
                                LEAVING_FOR,
                            );
                        }
                        crate::refresh(live);
                    }
                    Err(why) => {
                        let called = snapshot
                            .map(|device| device.name.to_string())
                            .unwrap_or_else(|| id.to_string());
                        failed.push(format!("{called}: {why}"));
                    }
                }
                busy.update(|busy| {
                    busy.remove(&id);
                });
            }
            if !failed.is_empty() {
                trouble.set(Some(format!("Couldn't add {}", failed.join("; "))));
            }
        });
    };

    let back = move |_| on_back.run(());
    let protocol = id.clone();
    let for_actions = id.clone();
    let for_home = id.to_string();

    view! {
        <section class="add-device add-step">
            <button type="button" class="quiet-button protocol-back" on:click=back>
                <svg viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor"
                     stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                    <path d="M15 6l-6 6 6 6" />
                </svg>
                "All extensions"
            </button>
            <div class="add-step-head">
                {move || icon(protocol.as_str(), icon_there())}
                <h3>{name}</h3>
                {move || extension.with(|e| e.as_ref().map(|e| view! {
                    <span class="state" class:ok=e.state == "running">
                        {e.state.replace('_', " ")}
                    </span>
                }))}
            </div>
            {move || extension.get().map(|extension| view! {
                <ProtocolActions id=for_actions.clone() extension=extension closing=closing />
            })}
            {move || unpairing.get().map(|device| view! {
                <crate::removal::RemoveDevice
                    id=device.id.clone()
                    name=device.name.to_string()
                    network=name()
                    unpairs=true
                    found=true
                    on_close=move || unpairing.set(None)
                    on_done=move || unpairing.set(None)
                />
            })}
            {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}

            <div class="found">
                <div class="found-head">
                    <h4>
                        "Found on your network"
                        {move || {
                            let count = found.with(Vec::len);
                            (count > 0).then(|| view! { <span class="found-count">{count}</span> })
                        }}
                    </h4>
                    {move || {
                        let count = addable.with(Vec::len);
                        (count > 1).then(|| view! {
                            <label class="pick-all" title="Select all">
                                <input
                                    type="checkbox"
                                    aria-label="Select all"
                                    prop:checked=move || {
                                        !addable.with(Vec::is_empty)
                                            && chosen.with(Vec::len) == addable.with(Vec::len)
                                    }
                                    on:change=move |ev| {
                                        if event_target_checked(&ev) {
                                            picked.update(|set| set.extend(addable.get_untracked()));
                                        } else {
                                            picked.update(BTreeSet::clear);
                                        }
                                    }
                                />
                            </label>
                        })
                    }}
                    <span class="found-actions">
                        {move || {
                            let chosen_now = chosen.get();
                            let all = addable.get();
                            if !chosen_now.is_empty() {
                                let count = chosen_now.len();
                                view! {
                                    <button
                                        type="button"
                                        class="primary"
                                        on:click=move |_| add(chosen.get_untracked())
                                    >
                                        {format!("Add selected ({count})")}
                                    </button>
                                }
                                    .into_any()
                            } else if !all.is_empty() {
                                let count = all.len();
                                view! {
                                    <button
                                        type="button"
                                        class="primary"
                                        on:click=move |_| add(addable.get_untracked())
                                    >
                                        {format!("Add all ({count})")}
                                    </button>
                                }
                                    .into_any()
                            } else {
                                ().into_any()
                            }
                        }}
                    </span>
                </div>
                <crate::waiting::WaitingFor extension=id.clone() />
                {move || {
                    let nothing = drawn.with(Vec::is_empty) && {
                        let waiting = extension.with(|e| e.as_ref().is_some_and(|e| !e.waiting.is_empty()));
                        !waiting
                    };
                    // Listening is only said while it's true. A protocol that opens its network
                    // for a minute isn't listening the rest of the time, and saying so sent
                    // people to hold a device's pairing button at a closed network.
                    let listening = !opens() || open_now();
                    nothing.then(|| if listening {
                        view! {
                            <div class="listening-empty">
                                <span class="listening-ring" aria-hidden="true"></span>
                                <p>
                                    {format!("Listening for {} devices on your network…", name())}
                                </p>
                                <p class="muted small">
                                    "Anything it finds shows up here within a few seconds of joining. "
                                    "Devices already in your home are listed below."
                                </p>
                            </div>
                        }
                            .into_any()
                    } else {
                        let action = extension.with(|e| {
                            e.as_ref()
                                .and_then(|e| e.actions.iter().find(|a| a.seconds.is_some()))
                                .map(|action| action.label.clone())
                                .unwrap_or_default()
                        });
                        view! {
                            <div class="listening-empty closed">
                                <p class="muted small">
                                    "Nothing found yet. Press "
                                    <strong>{action}</strong>
                                    " above, then put the device into pairing mode."
                                </p>
                            </div>
                        }
                            .into_any()
                    })
                }}
                <ul class="device-cards">
                    <For
                        each=move || drawn.get()
                        key=|device| device.id.clone()
                        children=move |device| {
                            let i = found.with_untracked(|found| {
                                found.iter().position(|d| d.id == device.id).unwrap_or(0)
                            });
                            let arrived = !opening.get_value();
                            let id = device.id.clone();
                            let is_leaving = {
                                let id = id.clone();
                                Memo::new(move |_| leaving.with(|l| l.iter().any(|d| d.id == id)))
                            };
                            let is_busy = {
                                let id = id.clone();
                                Memo::new(move |_| busy.with(|busy| busy.contains(&id)))
                            };
                            let is_picked = {
                                let id = id.clone();
                                Memo::new(move |_| picked.with(|picked| picked.contains(&id)))
                            };
                            let chips = provides(&device.provides);
                            let toggle = {
                                let id = id.clone();
                                move |ev| {
                                    let on = event_target_checked(&ev);
                                    picked.update(|set| {
                                        if on {
                                            set.insert(id.clone());
                                        } else {
                                            set.remove(&id);
                                        }
                                    });
                                }
                            };
                            let add_one = {
                                let id = id.clone();
                                move |_| add(vec![id.clone()])
                            };
                            let unpair_one = {
                                let device = device.clone();
                                move |_| unpairing.set(Some(device.clone()))
                            };
                            let made = [device.manufacturer.clone(), device.model.clone()]
                                .into_iter()
                                .flatten()
                                .collect::<Vec<_>>()
                                .join(" · ");
                            let name = device.name.to_string();
                            view! {
                                <li
                                    class="device-card found-card"
                                    class:arrived=arrived
                                    class:leaving=is_leaving
                                    class:picked=is_picked
                                    style=format!("--i: {i}")
                                >
                                    <div class="device-card-inner">
                                        <label class="pick">
                                            <input
                                                type="checkbox"
                                                aria-label=format!("Select {name}")
                                                prop:checked=is_picked
                                                prop:disabled=move || is_leaving.get() || is_busy.get()
                                                on:change=toggle
                                            />
                                        </label>
                                        {icon(&device.protocol, icon_there())}
                                        <span class="device-card-text">
                                            <span class="name">{name.clone()}</span>
                                            {(!made.is_empty()).then(|| view! {
                                                <span class="device-card-made">{made}</span>
                                            })}
                                            <span class="provides">{chips}</span>
                                        </span>
                                        {unpairs().then(|| view! {
                                            <button
                                                type="button"
                                                class="quiet-button unpair-one"
                                                disabled=move || is_busy.get() || is_leaving.get()
                                                aria-label=format!("Unpair {name}")
                                                title="Take it off the network without adding it"
                                                on:click=unpair_one
                                            >
                                                "Unpair"
                                            </button>
                                        })}
                                        <button
                                            type="button"
                                            class="add-one"
                                            class:busy=is_busy
                                            class:done=is_leaving
                                            disabled=move || is_busy.get() || is_leaving.get()
                                            aria-label=format!("Add {name}")
                                            on:click=add_one
                                        >
                                            <span class="add-one-label">"Add"</span>
                                            <svg class="add-one-check" viewBox="0 0 24 24" aria-hidden="true"
                                                 fill="none" stroke="currentColor" stroke-width="2.2"
                                                 stroke-linecap="round" stroke-linejoin="round">
                                                <path d="M5 12.5l4.5 4.5L19 7.5" pathLength="1" />
                                            </svg>
                                        </button>
                                    </div>
                                </li>
                            }
                        }
                    />
                </ul>
            </div>

            <div class="in-home">
                <h4>
                    "In your home"
                    {move || {
                        let count = here.with(Vec::len);
                        (count > 0).then(|| view! { <span class="found-count quiet">{count}</span> })
                    }}
                </h4>
                {move || here.with(Vec::is_empty).then(|| view! {
                    <p class="muted small">"Nothing yet. Add a device above and it moves down here."</p>
                })}
                <ul class="device-cards">
                    <For
                        each=move || here.get()
                        key=|(id, name, room)| (id.clone(), name.clone(), room.clone())
                        children=move |(device, name, room)| {
                            let arrived = !opening.get_value();
                            let for_home = for_home.clone();
                            view! {
                                <li class="device-card home-card" class:arrived=arrived>
                                    <A
                                        href=format!("/devices/{device}")
                                        attr:class="device-card-inner"
                                    >
                                        {icon(&for_home, icon_there())}
                                        <span class="device-card-text">
                                            <span class="name">{name}</span>
                                            <span class="device-card-made">
                                                {room.unwrap_or_else(|| "No room".to_owned())}
                                            </span>
                                        </span>
                                        <svg class="home-chevron" viewBox="0 0 24 24" aria-hidden="true"
                                             fill="none" stroke="currentColor" stroke-width="2"
                                             stroke-linecap="round" stroke-linejoin="round">
                                            <path d="M9 6l6 6-6 6" />
                                        </svg>
                                    </A>
                                </li>
                            }
                        }
                    />
                </ul>
            </div>
        </section>
    }
}

/// What a found device would bring, as small labels: `2 sensors`, `1 light`.
fn provides(kinds: &BTreeMap<String, usize>) -> AnyView {
    if kinds.is_empty() {
        return view! { <span class="chip quiet">"nothing Irori can use yet"</span> }.into_any();
    }
    kinds
        .iter()
        .map(|(kind, count)| {
            let word = kind.replace('_', " ");
            let plural = match (*count, word.ends_with(['s', 'h', 'x'])) {
                (1, _) => "",
                (_, true) => "es",
                _ => "s",
            };
            view! { <span class="chip">{format!("{count} {word}{plural}")}</span> }
        })
        .collect_view()
        .into_any()
}

/// When each of an extension's open actions closes, on this browser's own clock (milliseconds,
/// as `Date.now()` counts them), by action id. Empty when none is open.
///
/// The server says how long is left, not when: a browser whose clock is off would count down
/// from the wrong place. Every reading says it again with a slightly smaller number, so a time
/// already held is only replaced when the new one differs by more than the readings' own jitter
/// — otherwise the countdown would twitch, and everything drawn from it would be redrawn every
/// couple of seconds.
fn closing_times(live: crate::Live, id: ExtensionId) -> RwSignal<BTreeMap<String, f64>> {
    /// How far a new reading may be from what's held before it's believed over it. Readings
    /// take a moment to arrive; a window that was really cut short or extended moves by more.
    const SLACK_MS: f64 = 1500.0;
    let closing = RwSignal::new(BTreeMap::<String, f64>::new());
    Effect::new(move |_| {
        let left = live.home.with(|home| {
            home.extensions
                .iter()
                .find(|(known, _)| **known == id)
                .map(|(_, extension)| extension.open_actions.clone())
                .unwrap_or_default()
        });
        let now = web_sys::js_sys::Date::now();
        let held = closing.get_untracked();
        let mut next = BTreeMap::new();
        for (action, open) in left {
            let closes = now + open.closes_in_ms as f64;
            let closes = match held.get(&action) {
                Some(held) if (held - closes).abs() < SLACK_MS => *held,
                _ => closes,
            };
            next.insert(action, closes);
        }
        if next != held {
            closing.set(next);
        }
    });
    closing
}

/// The button for a protocol's declared action (Zigbee's permit-join, say) — nothing at all for
/// a protocol that declares none.
///
/// A protocol that declares one but doesn't say it's usable yet gets a sentence instead of the
/// button. Rendering nothing there is what made the feature look missing: the extension that has
/// the button is exactly the one that takes a while to come up, so whoever opens this first sees
/// an empty panel and concludes there's no such flow.
///
/// While a timed action is open the button is its countdown: a ring that drains, the time left,
/// and a press that closes it early. The button used to keep its words after a press, with a
/// paragraph underneath saying the network was open "briefly" — nobody could tell whether the
/// press had taken, or how long they had.
#[component]
fn ProtocolActions(
    id: ExtensionId,
    extension: crate::api::Extension,
    /// When each open action closes (`closing_times`).
    closing: RwSignal<BTreeMap<String, f64>>,
) -> impl IntoView {
    let sending = RwSignal::new(None::<String>);
    let trouble = RwSignal::new(None::<String>);
    let live = expect_context::<crate::Live>();

    if extension.actions.is_empty() {
        return ().into_any();
    }
    let (usable, waiting_on): (Vec<_>, Vec<_>) = extension
        .actions
        .iter()
        .cloned()
        .partition(|action| extension.available_actions.contains(&action.id));

    let not_yet = (!waiting_on.is_empty()).then(|| {
        let names = waiting_on
            .iter()
            .map(|action| action.label.clone())
            .collect::<Vec<_>>()
            .join(", ");
        let why = match extension.state.as_str() {
            "needs_setup" => "It needs a setting filled in first".to_owned(),
            "running" => format!("{} is still getting ready", extension.name),
            other => format!("It's {}", other.replace('_', " ")),
        };
        view! { <p class="muted small">{format!("{why} — \"{names}\" appears here once it can be used.")}</p> }
    });

    // The time, for the countdown: read a few times a second, and only while this is drawn.
    let now = RwSignal::new(web_sys::js_sys::Date::now());
    let ticking = set_interval_with_handle(
        move || {
            if closing.with_untracked(|closing| !closing.is_empty()) {
                let at = web_sys::js_sys::Date::now();
                now.set(at);
                // One whose time is up is closed here without waiting to be told, and the
                // extension is read again to hear it from the source.
                if closing.with_untracked(|closing| closing.values().any(|closes| *closes <= at)) {
                    closing.update(|closing| closing.retain(|_, closes| *closes > at));
                    crate::refresh(live);
                }
            }
        },
        std::time::Duration::from_millis(250),
    )
    .ok();
    on_cleanup(move || {
        if let Some(ticking) = ticking {
            ticking.clear();
        }
    });

    view! {
        <div class="protocol-actions">
            {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
            {usable
                .into_iter()
                .map(|action| {
                    let id = id.clone();
                    let action_id = action.id.clone();
                    let seconds = action.seconds;
                    // One that opens for a while is confirmed by the extension, and waited for.
                    let timed = seconds.is_some();
                    let asked_of = extension.name.clone();
                    let is_busy = Memo::new({
                        let action_id = action_id.clone();
                        move |_| sending.get().as_deref() == Some(action_id.as_str())
                    });
                    // Milliseconds left, while it's open.
                    let left = {
                        let action_id = action_id.clone();
                        move || {
                            // Read again on every tick, but measured against the clock itself:
                            // the tick only runs while something is open, so the time it last
                            // wrote is from before this opened.
                            now.track();
                            let at = web_sys::js_sys::Date::now();
                            closing
                                .with(|closing| closing.get(&action_id).copied())
                                .map(|closes| (closes - at).max(0.0))
                        }
                    };
                    let is_open = Memo::new({
                        let action_id = action_id.clone();
                        move |_| closing.with(|closing| closing.contains_key(&action_id))
                    });
                    // What the ring is full at: the action's own length, or longer when
                    // something else opened it for longer (the bridge's own switch opens
                    // Zigbee's network for four minutes).
                    let whole = StoredValue::new(f64::from(seconds.unwrap_or(0)) * 1000.0);
                    let fraction = {
                        let left = left.clone();
                        move || {
                            let left = left().unwrap_or(0.0);
                            if left > whole.get_value() {
                                whole.set_value(left);
                            }
                            let whole = whole.get_value();
                            if whole > 0.0 { (left / whole).clamp(0.0, 1.0) } else { 0.0 }
                        }
                    };
                    let seconds_left = {
                        let left = left.clone();
                        Signal::derive(move || (left().unwrap_or(0.0) / 1000.0).ceil() as u32)
                    };
                    let fraction = Signal::derive(fraction);
                    let label = match seconds {
                        Some(seconds) => format!("{} for {seconds}s", action.label),
                        None => action.label.clone(),
                    };
                    let said = action.label.clone();
                    let spoken = action.label.clone();
                    let closed_label = label.clone();
                    view! {
                        <button
                            type="button"
                            class="add"
                            class:join-open=is_open
                            class:join-waiting=is_busy
                            aria-busy=move || is_busy.get().to_string()
                            disabled=move || sending.get().is_some()
                            aria-label=move || if is_open.get() {
                                format!(
                                    "{spoken} is open, {} seconds left. Press to close it now.",
                                    seconds_left.get(),
                                )
                            } else {
                                closed_label.clone()
                            }
                            on:click=move |_| {
                                let id = id.clone();
                                let action_id = action_id.clone();
                                let closing_it = is_open.get_untracked();
                                let asked_of = asked_of.clone();
                                sending.set(Some(action_id.clone()));
                                spawn_local(async move {
                                    let asked = if closing_it {
                                        crate::api::stop_action(&id, &action_id).await
                                    } else {
                                        crate::api::trigger_action(&id, &action_id).await
                                    };
                                    match asked {
                                        // Asked for, not yet so: the button keeps turning
                                        // until the extension says the network really is
                                        // open (or shut), which is also where the time left
                                        // comes from. Zigbee takes a moment over it.
                                        Ok(()) if timed => {
                                            trouble.set(None);
                                            if !confirmed(live, &id, &action_id, !closing_it).await {
                                                trouble.set(Some(format!(
                                                    "{asked_of} hasn't said it {} yet. It may still do so.",
                                                    if closing_it { "closed" } else { "opened" },
                                                )));
                                            }
                                        }
                                        Ok(()) => {
                                            trouble.set(None);
                                            crate::refresh(live);
                                        }
                                        Err(why) => trouble.set(Some(why)),
                                    }
                                    sending.set(None);
                                });
                            }
                        >
                            {move || if is_open.get() && is_busy.get() {
                                view! {
                                    <span class="join-wait" aria-hidden="true"></span>
                                    "Closing…"
                                }
                                    .into_any()
                            } else if is_open.get() {
                                view! {
                                    <svg class="join-ring" viewBox="0 0 24 24" aria-hidden="true">
                                        <circle class="join-ring-track" cx="12" cy="12" r="9" />
                                        <circle
                                            class="join-ring-left"
                                            cx="12" cy="12" r="9" pathLength="1"
                                            style:stroke-dashoffset=move || format!("{:.4}", 1.0 - fraction.get())
                                        />
                                    </svg>
                                    <span class="join-words">
                                        <span class="join-word-open">{format!("{said} is open")}</span>
                                        <span class="join-word-close">"Close now"</span>
                                    </span>
                                    <span class="join-time">
                                        {move || {
                                            let left = seconds_left.get();
                                            format!("{}:{:02}", left / 60, left % 60)
                                        }}
                                    </span>
                                }
                                    .into_any()
                            } else if is_busy.get() {
                                view! {
                                    <span class="join-wait" aria-hidden="true"></span>
                                    {if timed { "Opening…" } else { "Working…" }}
                                }
                                    .into_any()
                            } else {
                                label.clone().into_any()
                            }}
                        </button>
                    }
                })
                .collect_view()}
            {not_yet}
        </div>
    }
        .into_any()
}

/// Waits for an extension to say its action is open (or no longer is), reading the home again
/// until it does. `false` when it hasn't within [`CONFIRM_WITHIN`]: asked, but not confirmed.
async fn confirmed(live: crate::Live, id: &ExtensionId, action_id: &str, open: bool) -> bool {
    /// Zigbee2MQTT answers in well under a second on a healthy stick; this is for a slow one.
    const CONFIRM_WITHIN: std::time::Duration = std::time::Duration::from_secs(8);
    const EVERY: std::time::Duration = std::time::Duration::from_millis(250);
    let mut waited = std::time::Duration::ZERO;
    loop {
        if let Ok(fetched) = crate::api::fetch_home().await {
            let is_open = fetched
                .extensions
                .iter()
                .find(|(known, _)| *known == id)
                .is_some_and(|(_, extension)| extension.open_actions.contains_key(action_id));
            if fetched != live.home.get_untracked() {
                live.home.set(fetched);
            }
            if is_open == open {
                return true;
            }
        }
        if waited >= CONFIRM_WITHIN {
            return false;
        }
        gloo_timers::future::sleep(EVERY).await;
        waited += EVERY;
    }
}

/// Plain words for an `iot_class`: where the device's brain is and what it needs.
fn how(iot_class: &Option<String>) -> &'static str {
    match iot_class.as_deref() {
        Some("local_push") => "local · pushes",
        Some("local_polling") => "local · polled",
        Some("cloud_push") => "cloud · pushes",
        Some("cloud_polling") => "cloud · polled",
        Some("assumed_state") => "no feedback",
        _ => "unknown",
    }
}

/// A row's way into its own last 24 hours (`history.rs` keeps which are open).
#[derive(Debug, Clone, Copy)]
pub struct Unroll {
    pub open: Signal<bool>,
    pub toggle: Callback<()>,
}

/// The call to open a row's history, where the eye already is. A reading *is* the thing to ask
/// about, so for sensors the reading itself is the button; a light or switch's control is for
/// switching, so there the button is the chevron beside it.
pub(crate) fn unrolling(entity: &Entity, control: AnyView, unroll: Option<Unroll>) -> AnyView {
    let Some(Unroll { open, toggle }) = unroll else {
        return control;
    };
    let expanded = move || open.get().to_string();
    let chevron = view! { <span class="unroll-mark" aria-hidden="true"></span> };
    let hint = view! { <span class="visually-hidden">" — last 24 hours"</span> };
    // A button has nothing to remember from one day to the next.
    if matches!(entity.capabilities, Capabilities::Button(_)) {
        return control;
    }
    match entity.capabilities {
        Capabilities::Sensor(_) | Capabilities::BinarySensor(_) | Capabilities::Event(_) => view! {
            <button
                type="button"
                class="unroll reading-unroll"
                class:open=move || open.get()
                title="Last 24 hours"
                aria-expanded=expanded
                on:click=move |event| {
                    if !crate::gesture::swallow_click(&event) {
                        toggle.run(());
                    }
                }
                on:pointerdown=|event| crate::gesture::pull_down(&event)
                on:pointermove=move |event| pull(&event, open, toggle)
                on:pointerup=|event| crate::gesture::pull_up(&event)
                on:pointercancel=|event| crate::gesture::pull_up(&event)
            >
                {control}
                {hint}
                {chevron}
            </button>
        }
        .into_any(),
        Capabilities::Light(_)
        | Capabilities::Switch(_)
        | Capabilities::Number(_)
        | Capabilities::Select(_)
        | Capabilities::Text(_)
        | Capabilities::Button(_)
        | Capabilities::Cover(_)
        | Capabilities::Lock(_)
        | Capabilities::Fan(_)
        | Capabilities::Valve(_)
        | Capabilities::Siren(_)
        | Capabilities::Climate(_)
        | Capabilities::WaterHeater(_)
        | Capabilities::Humidifier(_)
        | Capabilities::MediaPlayer(_) => view! {
            {control}
            <button
                type="button"
                class="unroll"
                class:open=move || open.get()
                title="Last 24 hours"
                aria-label=format!("{} — last 24 hours", entity.name)
                aria-expanded=expanded
                on:click=move |event| {
                    if !crate::gesture::swallow_click(&event) {
                        toggle.run(());
                    }
                }
                on:pointerdown=|event| crate::gesture::pull_down(&event)
                on:pointermove=move |event| pull(&event, open, toggle)
                on:pointerup=|event| crate::gesture::pull_up(&event)
                on:pointercancel=|event| crate::gesture::pull_up(&event)
            >
                {chevron}
            </button>
        }
        .into_any(),
    }
}

/// A pull on a history handle: down opens the drawer, up closes it.
fn pull(event: &ev::PointerEvent, open: Signal<bool>, toggle: Callback<()>) {
    if let Some(down) = crate::gesture::pull_move(event)
        && down != open.get_untracked()
    {
        toggle.run(());
    }
}
