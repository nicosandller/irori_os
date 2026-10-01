//! The Devices page: everything in the home, grouped by the device it belongs to, with a switch
//! for the things that can be switched.

use std::collections::{BTreeMap, BTreeSet};

use irori_types::{
    AreaId, Availability, BinarySensorCapabilities, BinarySensorClass, Capabilities, Device,
    DeviceId, Entity, EntityId, EntityState, ExtensionId, LightCapabilities, LightState,
    LightTurnOn, NumberCapabilities, NumberMode, SelectCapabilities, SensorCapabilities,
    SensorClass, SensorValue, State,
};
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;

use crate::api::Home;

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
    /// Ask a number to take this value.
    pub set_number: Callback<(EntityId, f64)>,
    /// Ask a select to take this option.
    pub set_option: Callback<(EntityId, String)>,
}

/// One device and the entities it provides. `device` is `None` for entities that belong to no
/// device, which the protocol contract allows.
#[derive(Debug)]
pub struct Group {
    pub device: Option<Device>,
    pub entities: Vec<(Entity, Option<EntityState>)>,
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

/// Groups the home by device, keeping only what matches `needle`, and sorts everything by name so
/// the page doesn't reshuffle between refreshes.
pub fn groups(home: &Home, needle: &str) -> Vec<Group> {
    let needle = needle.trim().to_lowercase();
    let states: BTreeMap<_, _> = home.states.iter().map(|s| (&s.entity_id, s)).collect();
    let devices: BTreeMap<_, _> = home.devices.iter().map(|d| (&d.id, d)).collect();

    // A device's name is part of what its entities are called in conversation ("the lamp in the
    // hallway sensor"), so typing it keeps the whole device. Area and make too: the same
    // search box is used on the Devices table.
    let matches = |entity: &Entity| {
        needle.is_empty()
            || entity.id.to_string().to_lowercase().contains(&needle)
            || entity.name.as_str().to_lowercase().contains(&needle)
            || entity
                .device_id
                .as_ref()
                .and_then(|id| devices.get(id))
                .is_some_and(|device| matches_device(home, device, &needle))
    };

    // Grouped by device name, then device id so two devices sharing a name keep a stable order.
    // The leading flag puts the entities that belong to no device after all the real ones.
    let mut by_device: BTreeMap<(bool, &str, &str), Group> = BTreeMap::new();
    for entity in home.entities.iter().filter(|e| matches(e)) {
        let device = entity
            .device_id
            .as_ref()
            .and_then(|id| devices.get(id))
            .copied();
        let key = match device {
            Some(device) => (false, device.name.as_str(), device.id.as_str()),
            None => (true, "", ""),
        };
        by_device
            .entry(key)
            .or_insert_with(|| Group {
                device: device.cloned(),
                entities: Vec::new(),
            })
            .entities
            .push((
                entity.clone(),
                states.get(&entity.id).map(|state| (*state).clone()),
            ));
    }

    by_device
        .into_values()
        .map(|mut group| {
            group.entities.sort_by(|(a, _), (b, _)| {
                // What the device is for first, then its settings and diagnostics.
                (a.entity_category, &a.name, &a.id).cmp(&(b.entity_category, &b.name, &b.id))
            });
            group
        })
        .collect()
}

/// Which view the page shows. Remembered per browser, because it's a preference about reading
/// rather than anything Irori needs to know.
const VIEW_KEY: &str = "irori.devices.view";

/// Which groups of the device table are folded away, remembered the same way.
const FOLDED_KEY: &str = "irori.devices.folded";

/// The three ways to look at what's in the home.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Showing {
    /// One row per device, grouped by the protocol it came through. The default: a device is
    /// the thing a person bought and put somewhere.
    Devices,
    /// Every entity with its reading and its switch, grouped by device.
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
    let folded = RwSignal::new(remembered_folded());
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
    Effect::new(move |_| {
        remember(
            FOLDED_KEY,
            &folded.get().into_iter().collect::<Vec<_>>().join(","),
        )
    });

    view! {
        <div class="page-head">
            <h1>"Devices"</h1>
            <div class="switcher" role="group" aria-label="What to show">
                {Showing::ALL
                    .into_iter()
                    .map(|view| view! {
                        <button
                            type="button"
                            class:chosen=move || showing.get() == view
                            aria-pressed=move || (showing.get() == view).to_string()
                            on:click=move |_| showing.set(view)
                        >
                            {view.label()}
                        </button>
                    })
                    .collect_view()}
            </div>
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
                    <button type="button" class="link" on:click=move |_| adding.set(true)>
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

        {move || {
            let home = live.home.get();
            let needle = filter.get();
            match showing.get() {
                Showing::Devices => table(&home, &needle, folded),
                Showing::Entities => view(&home, &needle, controls),
                Showing::Helpers => helpers(&home, controls),
            }
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

/// A device with its entities and what they're reporting: one row of the device table.
type DeviceRow = (Device, Vec<(Entity, Option<EntityState>)>);

/// One row per device: what it is and where it is, grouped by the protocol it came through.
///
/// Built from the devices rather than from their entities, so a device Irori is connected to
/// still appears when it provides nothing Irori can model — a Bluetooth proxy, say. Those are
/// invisible in the entity view by their nature, and being unable to find them would be worse.
fn table(home: &Home, needle: &str, folded: RwSignal<BTreeSet<String>>) -> AnyView {
    let needle = needle.trim().to_lowercase();
    let mut by_protocol: BTreeMap<String, Vec<DeviceRow>> = BTreeMap::new();
    for device in home
        .devices
        .iter()
        .filter(|device| matches_device(home, device, &needle))
    {
        let entities: Vec<_> = home
            .entities
            .iter()
            .filter(|entity| entity.device_id.as_ref() == Some(&device.id))
            .map(|entity| {
                let state = home
                    .states
                    .iter()
                    .find(|state| state.entity_id == entity.id)
                    .cloned();
                (entity.clone(), state)
            })
            .collect();
        by_protocol
            .entry(device.protocol.to_string())
            .or_default()
            .push((device.clone(), entities));
    }
    if by_protocol.is_empty() {
        let message = if home.devices.is_empty() {
            "No devices yet. Extensions bring them in; \"Add device\" says how."
        } else {
            "Nothing matches that."
        };
        return view! { <p class="empty">{message}</p> }.into_any();
    }
    let areas: BTreeMap<Option<AreaId>, String> = home
        .areas
        .iter()
        .map(|area| (Some(area.id.clone()), area.name.to_string()))
        .collect();
    let mut groups: Vec<_> = by_protocol
        .into_iter()
        .map(|(protocol, mut devices)| {
            devices.sort_by(|(a, _), (b, _)| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
            let extension = home
                .extensions
                .iter()
                .find(|(id, _)| id.as_str() == protocol);
            let name = extension
                .map(|(_, extension)| extension.name.clone())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| protocol.clone());
            let has_icon = extension.is_some_and(|(_, extension)| extension.has_icon);
            (protocol, name, has_icon, devices)
        })
        .collect();
    groups.sort_by_key(|group| group.1.to_lowercase());
    // While filtering, every group with a match is open: a folded group hiding the one result
    // would look like no result at all.
    let filtering = !needle.is_empty();

    view! {
        <div class="table-scroll">
            <table class="devices">
                <thead>
                    <tr>
                        <th scope="col" class="icon-col">
                            <span class="visually-hidden">"Protocol"</span>
                        </th>
                        <th scope="col">"Device"</th>
                        <th scope="col">"Area"</th>
                        <th scope="col">"Make"</th>
                        <th scope="col">"Model"</th>
                        <th scope="col">"Battery"</th>
                        <th scope="col" class="number">"Entities"</th>
                    </tr>
                </thead>
                {groups
                    .into_iter()
                    .map(|(protocol, name, has_icon, devices)| {
                        group(protocol, name, has_icon, devices, &areas, folded, filtering)
                    })
                    .collect_view()}
            </table>
        </div>
    }
    .into_any()
}

/// One protocol's devices: a header that folds them away, and a row each.
fn group(
    protocol: String,
    name: String,
    has_icon: bool,
    devices: Vec<DeviceRow>,
    areas: &BTreeMap<Option<AreaId>, String>,
    folded: RwSignal<BTreeSet<String>>,
    filtering: bool,
) -> AnyView {
    let open = {
        let key = protocol.clone();
        move || filtering || !folded.get().contains(&key)
    };
    let toggle = {
        let key = protocol.clone();
        move |_| {
            folded.update(|folded| {
                if !folded.remove(&key) {
                    folded.insert(key.clone());
                }
            })
        }
    };
    let count = devices.len();
    let rows = devices
        .into_iter()
        .map(|(device, entities)| {
            let id = device.id.to_string();
            let entity_count = entities.len();
            let battery = battery(&entities);
            let room = areas.get(&device.area_id).cloned();
            view! {
                <tr>
                    <td class="icon-col">{icon(&protocol, has_icon)}</td>
                    <th scope="row">
                        <A
                            href=format!("/devices/{id}")
                            attr:style=crate::transition::list_name(&id)
                            on:click={
                                let (id, travelling) = (id.clone(), expect_context::<crate::transition::Travelling>().0);
                                move |_| travelling.set(Some(id.clone()))
                            }
                        >
                            {device.name.to_string()}
                        </A>
                        {device.description.as_ref().map(|description| view! {
                            <span class="description">{description.to_string()}</span>
                        })}
                    </th>
                    <td class="room">{room.unwrap_or_else(|| "—".to_owned())}</td>
                    <td>{device.manufacturer.clone().unwrap_or_default()}</td>
                    <td>{device.model.clone().unwrap_or_default()}</td>
                    <td class="battery">{battery.unwrap_or_else(|| "—".to_owned())}</td>
                    <td class="number">{entity_count}</td>
                </tr>
            }
        })
        .collect_view();
    let header_icon = icon(&protocol, has_icon);
    let folded_class = {
        let open = open.clone();
        move || !open()
    };
    view! {
        <tbody class="group" class:folded=folded_class>
            <tr class="group-head">
                <th scope="rowgroup" colspan="7">
                    <button
                        type="button"
                        aria-expanded=move || open().to_string()
                        on:click=toggle
                    >
                        <span class="chevron" aria-hidden="true"></span>
                        {header_icon}
                        <span class="group-name">{name}</span>
                        <span class="muted small">
                            {format!("{count} device{}", if count == 1 { "" } else { "s" })}
                        </span>
                    </button>
                </th>
            </tr>
            {rows}
        </tbody>
    }
    .into_any()
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
pub fn battery(entities: &[(Entity, Option<EntityState>)]) -> Option<String> {
    entities.iter().find_map(|(entity, state)| {
        let value = state.as_ref()?.state.as_ref();
        match (&entity.capabilities, value) {
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
    })
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

fn remembered_folded() -> BTreeSet<String> {
    stored(FOLDED_KEY)
        .map(|saved| {
            saved
                .split(',')
                .filter(|key| !key.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
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
    let extension = {
        let id = id.clone();
        Memo::new(move |_| {
            live.home.with(|home| {
                home.extensions
                    .iter()
                    .find(|(known, _)| **known == id)
                    .map(|(_, extension)| extension.clone())
            })
        })
    };
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
                <ProtocolActions id=for_actions.clone() extension=extension />
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
                    nothing.then(|| view! {
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

/// The button for a protocol's declared action (Zigbee's permit-join, say) — nothing at all for
/// a protocol that declares none.
///
/// A protocol that declares one but doesn't say it's usable yet gets a sentence instead of the
/// button. Rendering nothing there is what made the feature look missing: the extension that has
/// the button is exactly the one that takes a while to come up, so whoever opens this first sees
/// an empty panel and concludes there's no such flow.
#[component]
fn ProtocolActions(id: ExtensionId, extension: crate::api::Extension) -> impl IntoView {
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

    // What this protocol has found so far, so watching a device join is something the person can
    // actually see happen rather than having to go and look on another page.
    let found = {
        let id = id.clone();
        Memo::new(move |_| {
            live.home
                .get()
                .devices
                .iter()
                .filter(|device| device.protocol.as_str() == id.as_str())
                .count()
        })
    };
    // Set once an action with a duration has been triggered — what to say while it's open.
    let listening = RwSignal::new(None::<String>);

    view! {
        <div class="protocol-actions">
            {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
            {usable
                .into_iter()
                .map(|action| {
                    let id = id.clone();
                    let action_id = action.id.clone();
                    let seconds = action.seconds;
                    let is_busy = Memo::new({
                        let action_id = action_id.clone();
                        move |_| sending.get().as_deref() == Some(action_id.as_str())
                    });
                    let label = match seconds {
                        Some(seconds) => format!("{} for {seconds}s", action.label),
                        None => action.label.clone(),
                    };
                    let said = action.label.clone();
                    view! {
                        <button
                            type="button"
                            class="add"
                            disabled=move || sending.get().is_some()
                            on:click=move |_| {
                                let id = id.clone();
                                let action_id = action_id.clone();
                                let said = said.clone();
                                sending.set(Some(action_id.clone()));
                                spawn_local(async move {
                                    match crate::api::trigger_action(&id, &action_id).await {
                                        Ok(()) => {
                                            trouble.set(None);
                                            // No countdown, and no number: this says what to
                                            // do now, and nothing that goes stale while it's
                                            // still on screen. The protocol closes its own
                                            // window; the page has no way to know when.
                                            listening.set(Some(match seconds {
                                                Some(_) => format!(
                                                    "{said} is open — briefly. Put the device \
                                                     into pairing mode now: most need a button \
                                                     held down, or a power cycle or three.",
                                                ),
                                                None => format!("{said} done."),
                                            }));
                                            crate::refresh(live);
                                        }
                                        Err(why) => trouble.set(Some(why)),
                                    }
                                    sending.set(None);
                                });
                            }
                        >
                            {move || if is_busy.get() { "Working…".to_owned() } else { label.clone() }}
                        </button>
                    }
                })
                .collect_view()}
            {not_yet}
            {move || listening.get().map(|said| view! {
                <div class="listening-empty">
                    <p>{said}</p>
                    <p class="muted small">
                        {move || {
                            let found = found.get();
                            format!(
                                "{found} device{} here from this extension so far. A new one \
                                 shows up on this page on its own, within a few seconds of \
                                 joining.",
                                if found == 1 { "" } else { "s" },
                            )
                        }}
                    </p>
                </div>
            })}
        </div>
    }
        .into_any()
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

fn view(home: &Home, needle: &str, controls: Controls) -> AnyView {
    let groups = groups(home, needle);
    if groups.is_empty() {
        let message = if home.entities.is_empty() {
            "No devices yet. Extensions bring them in; the demo extension provides a few."
        } else {
            "Nothing matches that."
        };
        return view! { <p class="empty">{message}</p> }.into_any();
    }
    groups
        .into_iter()
        .map(|group| group_view(group, controls))
        .collect_view()
        .into_any()
}

fn group_view(group: Group, controls: Controls) -> AnyView {
    let (title, model) = match &group.device {
        Some(device) => (
            device.name.to_string(),
            [device.manufacturer.as_deref(), device.model.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" "),
        ),
        None => ("Not on a device".to_owned(), String::new()),
    };
    view! {
        <section class="device">
            <h2>{title}<span class="model">{model}</span></h2>
            {group
                .entities
                .into_iter()
                .map(|(entity, state)| row(entity, state, controls, None))
                .collect_view()}
        </section>
    }
    .into_any()
}

/// A row's way into its own last 24 hours. The device page gives its rows one; the list, which
/// is for switching things rather than reading them back, doesn't.
#[derive(Debug, Clone, Copy)]
pub struct Unroll {
    pub open: RwSignal<bool>,
    pub toggle: Callback<()>,
}

pub fn row(
    entity: Entity,
    state: Option<EntityState>,
    controls: Controls,
    unroll: Option<Unroll>,
) -> AnyView {
    let offline = state
        .as_ref()
        .is_some_and(|s| s.availability == Availability::Unavailable);
    let failure = {
        let id = entity.id.clone();
        move || controls.failures.get().get(&id).cloned()
    };
    let (id, full_id) = (entity.id.to_string(), entity.id.to_string());
    view! {
        <div class="entity" class:offline=offline>
            <span class="names">
                <span class="name">{entity.name.to_string()}</span>
                <span class="id" title=full_id>{id}</span>
            </span>
            {offline.then(|| view! { <span class="badge">"offline"</span> })}
            {unrolling(&entity, control(&entity, state.as_ref(), offline, controls), unroll)}
            {move || failure().map(|why| view! { <p class="why">{why}</p> })}
        </div>
    }
    .into_any()
}

/// The call to open a row's history, where the eye already is. A reading *is* the thing to ask
/// about, so for sensors the reading itself is the button; a light or switch's control is for
/// switching, so there the button is the chevron beside it.
fn unrolling(entity: &Entity, control: AnyView, unroll: Option<Unroll>) -> AnyView {
    let Some(Unroll { open, toggle }) = unroll else {
        return control;
    };
    let expanded = move || open.get().to_string();
    let chevron = view! { <span class="unroll-mark" aria-hidden="true"></span> };
    let hint = view! { <span class="visually-hidden">" — last 24 hours"</span> };
    match entity.capabilities {
        Capabilities::Sensor(_) | Capabilities::BinarySensor(_) => view! {
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
        | Capabilities::Select(_) => view! {
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
fn pull(event: &ev::PointerEvent, open: RwSignal<bool>, toggle: Callback<()>) {
    if let Some(down) = crate::gesture::pull_move(event)
        && down != open.get_untracked()
    {
        toggle.run(());
    }
}

fn control(
    entity: &Entity,
    state: Option<&EntityState>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let value = state.and_then(|s| s.state.as_ref());
    match &entity.capabilities {
        Capabilities::Light(capabilities) => light(entity, capabilities, value, offline, controls),
        Capabilities::Switch(_) => switch(entity, value, offline, controls),
        Capabilities::Sensor(capabilities) => sensor(capabilities, value),
        Capabilities::BinarySensor(capabilities) => binary(capabilities.device_class, value),
        Capabilities::Number(capabilities) => {
            number_control(entity, capabilities, value, offline, controls)
        }
        Capabilities::Select(capabilities) => {
            select_control(entity, capabilities, value, offline, controls)
        }
    }
}

/// A select: its choices in a dropdown, on the one the device last reported. Choosing sends it.
fn select_control(
    entity: &Entity,
    capabilities: &SelectCapabilities,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let current = match value {
        Some(State::Select(select)) => Some(select.option.clone()),
        _ => None,
    };
    let entity_id = entity.id.clone();
    let disable = {
        let entity_id = entity_id.clone();
        move || offline || controls.busy.get().contains(&entity_id)
    };
    let options = capabilities
        .options
        .iter()
        .map(|option| {
            let chosen = current.as_deref() == Some(option.as_str());
            view! { <option value=option.clone() selected=chosen>{option.clone()}</option> }
        })
        .collect_view();
    view! {
        <select
            class="select-control"
            aria-label=entity.name.to_string()
            disabled=disable
            on:change:target=move |ev| {
                controls.set_option.run((entity_id.clone(), ev.target().value()));
            }
        >
            // Until it says, nothing is chosen rather than its first option.
            {current.is_none().then(|| view! { <option value="" selected=true disabled=true>{UNKNOWN}</option> })}
            {options}
        </select>
    }
    .into_any()
}

/// The unit a reading or a setting is in, for labels: `°C`, `s`. Empty when it has none.
pub(crate) fn unit_of(capabilities: &Capabilities) -> String {
    match capabilities {
        Capabilities::Sensor(sensor) => sensor.unit.clone(),
        Capabilities::Number(number) => number.unit.clone(),
        _ => None,
    }
    .unwrap_or_default()
}

/// A number: its value and unit, and a way to change it. A slider when its range is short
/// enough to drag through (Home Assistant's rule: at most 256 steps), or when the device asks
/// for one; a box to type into otherwise. Like a light's sliders, only letting go (or leaving
/// the box) sends anything.
fn number_control(
    entity: &Entity,
    capabilities: &NumberCapabilities,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let current = match value {
        Some(State::Number(number)) => Some(number.value),
        _ => None,
    };
    let (min, max, step) = (capabilities.min, capabilities.max, capabilities.step);
    let slider = match capabilities.mode {
        NumberMode::Slider => true,
        NumberMode::Box => false,
        NumberMode::Auto => (max - min) / step <= 256.0,
    };
    let unit = capabilities.unit.clone().unwrap_or_default();
    let label = format!(
        "{} ({})",
        entity.name,
        if unit.is_empty() { "value" } else { &unit }
    );
    let entity_id = entity.id.clone();
    let disable = {
        let entity_id = entity_id.clone();
        move || offline || controls.busy.get().contains(&entity_id)
    };
    // Anything outside the range, or not a number at all, isn't sent: the box goes back to what
    // the device last said when the page next draws it.
    let send = move |text: String| {
        if let Ok(value) = text.trim().parse::<f64>()
            && (min..=max).contains(&value)
        {
            controls.set_number.run((entity_id.clone(), value));
        }
    };
    if slider {
        let dragged = RwSignal::new(current.unwrap_or(min));
        let title = label.clone();
        // The same row a light's sliders sit in, so the two look and behave alike.
        view! {
            <span class="light-controls">
            <label class="dim" title=title>
                <span class="lv">
                    {move || current.map_or_else(|| UNKNOWN.to_owned(), |_| number(dragged.get()))}
                    {(!unit.is_empty()).then(|| format!(" {unit}"))}
                </span>
                <input
                    type="range"
                    min=min.to_string()
                    max=max.to_string()
                    step=step.to_string()
                    aria-label=label
                    prop:value=current.unwrap_or(min).to_string()
                    style:--fill=move || format!("{}%", fill(dragged.get(), min, max))
                    disabled=disable
                    on:input:target=move |ev| {
                        if let Ok(value) = ev.target().value().parse::<f64>() {
                            dragged.set(value);
                        }
                    }
                    on:change:target=move |ev| send(ev.target().value())
                />
            </label>
            </span>
        }
        .into_any()
    } else {
        view! {
            <label class="number-box">
                <input
                    type="number"
                    inputmode="decimal"
                    min=min.to_string()
                    max=max.to_string()
                    step=step.to_string()
                    aria-label=label
                    prop:value=current.map(number).unwrap_or_default()
                    placeholder=UNKNOWN
                    disabled=disable
                    on:change:target=move |ev| send(ev.target().value())
                />
                {(!unit.is_empty()).then(|| view! { <span class="unit">{unit}</span> })}
            </label>
        }
        .into_any()
    }
}

fn switch(entity: &Entity, value: Option<&State>, offline: bool, controls: Controls) -> AnyView {
    let on = match value {
        Some(State::Switch(switch)) => Some(switch.on),
        _ => None,
    };
    view! {
        <>
            {knob(entity, on, offline, controls)}
        </>
    }
    .into_any()
}

/// A light: the current brightness beside the switch, and — while it's on — the sliders that
/// set the level it comes back to, its color temperature or its color. The switch is what the
/// entity is doing and the sliders are what a person is asking for, which is why the position a
/// slider shows can sit between sends: the row follows the device once it reports back (M1.5's
/// push will make that instant).
fn light(
    entity: &Entity,
    capabilities: &LightCapabilities,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let on = match value {
        Some(State::Light(light)) => Some(light.on),
        _ => None,
    };
    // Brightness and color are shown and changeable only while the light is on. A change here is
    // `light.turn_on` with a level, which comes on (the demo's `apply_light` turns a lit light
    // on at the level it asked for); a slider that switched the light on from a standing start
    // would be a light that seemed off before it was touched.
    let current = match value {
        Some(State::Light(light)) if light.on => Some(light),
        _ => None,
    };
    view! {
        <>
            {current.and_then(|l| l.brightness).map(reading)}
            {knob(entity, on, offline, controls)}
            {light_controls(entity, capabilities, current, offline, controls)}
        </>
    }
    .into_any()
}

/// The level a light is at, as a percentage the page shows and its sliders work in. `div_ceil`
/// keeps whole levels honest: 180 of 255 is 71%, not 70.
fn brightness_pct(level: u8) -> u16 {
    (u16::from(level) * 100).div_ceil(255)
}

/// The slider's percentage back to the 1-255 the `light.turn_on` data takes. Flooring keeps the
/// round trip stable: a level the page shows as 71% sends back a level the page will also read
/// as 71%, so the slider doesn't creep one notch per change.
fn pct_to_brightness(pct: u16) -> u8 {
    ((u32::from(pct.clamp(1, 100)) * 255 / 100) as u8).clamp(1, 255)
}

/// The color an `<input type="color">` gives, "#rrggbb", as the `[r, g, b]` the data takes.
fn rgb_from_hex(hex: &str) -> Option<[u8; 3]> {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let read = |from: usize| u8::from_str_radix(&hex[from..from + 2], 16).ok();
    Some([read(0)?, read(2)?, read(4)?])
}

/// `[r, g, b]` back into the "#rrggbb" a color input wants.
fn hex_from_rgb(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

/// The sliders that set a lit light's level and color: brightness when it can dim, color
/// temperature within its range, and a color well when it has one. Each sends one `turn_on` with
/// that one thing, keeping the level and color it already has.
fn light_controls(
    entity: &Entity,
    capabilities: &LightCapabilities,
    light: Option<&LightState>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let entity_id = entity.id.clone();
    let Some(light) = light else {
        return ().into_any();
    };

    let mut sub = Vec::new();
    if capabilities.brightness {
        let level = brightness_pct(light.brightness.unwrap_or(255));
        let id = entity_id.clone();
        let run = {
            let id = id.clone();
            move |pct| {
                controls.set_light.run((
                    id.clone(),
                    LightTurnOn {
                        brightness: Some(pct_to_brightness(pct)),
                        ..Default::default()
                    },
                ))
            }
        };
        // A command is one thing at a time per entity: while one is in flight the sliders stand
        // still, so dragging can't pile up commands the device will chew through in its own order.
        let disable = {
            let entity_id = entity_id.clone();
            move || offline || controls.busy.get().contains(&entity_id)
        };
        // Where the thumb is while it's being dragged: the label and the filled part of the track
        // follow it, and only letting go sends anything. Starts at what the device reported, and
        // does again whenever the device reports.
        let dragged = RwSignal::new(level);
        sub.push(
            view! {
                <label class="dim" title="Brightness">
                    <span class="lv">{move || dragged.get()}%</span>
                    <input
                        type="range"
                        min="1"
                        max="100"
                        step="1"
                        aria-label={format!("Brightness for {}", entity.name)}
                        prop:value=level.to_string()
                        style:--fill=move || format!("{}%", fill(dragged.get(), 1, 100))
                        disabled=disable
                        on:input:target=move |ev| {
                            if let Ok(pct) = ev.target().value().parse::<u16>() {
                                dragged.set(pct);
                            }
                        }
                        on:change:target=move |ev| {
                            let pct = ev.target().value().parse::<u16>().unwrap_or(level);
                            run(pct)
                        }
                    />
                </label>
            }
            .into_any(),
        );
    }
    if let Some(range) = capabilities.color_temp_kelvin {
        let current = light
            .color_temp_kelvin
            .unwrap_or_else(|| range.min + (range.max - range.min) / 2);
        let id = entity_id.clone();
        let run = {
            let id = id.clone();
            move |kelvin| {
                controls.set_light.run((
                    id.clone(),
                    LightTurnOn {
                        color_temp_kelvin: Some(kelvin),
                        ..Default::default()
                    },
                ))
            }
        };
        let disable = {
            let entity_id = entity_id.clone();
            move || offline || controls.busy.get().contains(&entity_id)
        };
        let dragged = RwSignal::new(current);
        sub.push(
            view! {
                <label class="dim" title="Color temperature">
                    <span class="lv">{move || dragged.get()}K</span>
                    // No fill here: the track is the colors themselves, warm to cool.
                    <input
                        type="range"
                        class="kelvin"
                        min=range.min.to_string()
                        max=range.max.to_string()
                        step="100"
                        aria-label={format!("Color temperature for {}", entity.name)}
                        prop:value=current.to_string()
                        disabled=disable
                        on:input:target=move |ev| {
                            if let Ok(kelvin) = ev.target().value().parse::<u16>() {
                                dragged.set(kelvin);
                            }
                        }
                        on:change:target=move |ev| {
                            let kelvin = ev.target().value().parse::<u16>().unwrap_or(current);
                            run(kelvin)
                        }
                    />
                </label>
            }
            .into_any(),
        );
    }
    if capabilities.rgb {
        let current = hex_from_rgb(light.rgb.unwrap_or([255, 255, 255]));
        let id = entity_id.clone();
        let run = {
            let id = id.clone();
            move |rgb| {
                controls.set_light.run((
                    id.clone(),
                    LightTurnOn {
                        rgb: Some(rgb),
                        ..Default::default()
                    },
                ))
            }
        };
        let disable = {
            let entity_id = entity_id.clone();
            move || offline || controls.busy.get().contains(&entity_id)
        };
        sub.push(
            view! {
                <label class="dim" title="Color">
                    <input
                        type="color"
                        aria-label={format!("Color for {}", entity.name)}
                        prop:value=current.clone()
                        disabled=disable
                        on:change:target=move |ev| {
                            if let Some(rgb) = rgb_from_hex(&ev.target().value()) {
                                run(rgb)
                            }
                        }
                    />
                </label>
            }
            .into_any(),
        );
    }
    if sub.is_empty() {
        return ().into_any();
    }

    view! {
        <span class="light-controls">
            {sub}
        </span>
    }
    .into_any()
}

/// How far along a slider's track `value` sits, as a whole percentage — where its filled part
/// ends. Any number type a slider holds: a light's level, a wall's thickness, a snap step.
pub(crate) fn fill(value: impl Into<f64>, min: impl Into<f64>, max: impl Into<f64>) -> u16 {
    let (value, min, max) = (value.into(), min.into(), max.into());
    if max <= min {
        return 100;
    }
    ((value.clamp(min, max) - min) * 100.0 / (max - min)).floor() as u16
}

fn reading(level: u8) -> impl IntoView {
    view! {
        <span class="reading">
            <span class="n" data-n=brightness_pct(level).to_string()>
                {brightness_pct(level).to_string()}
            </span>
            "%"
        </span>
    }
}

/// A switch showing what the entity is doing, not what was last clicked: it moves when the
/// device reports back. A light that has never reported sits in between, and clicking turns it
/// on.
fn knob(entity: &Entity, on: Option<bool>, offline: bool, controls: Controls) -> impl IntoView {
    let entity_id = entity.id.clone();
    let busy = {
        let entity_id = entity_id.clone();
        move || controls.busy.get().contains(&entity_id)
    };
    let pending = busy.clone();
    // What the click means is "I want it off", not "flip whatever it is now": the page sends the
    // state the person asked for, so a second click on a stale row can't undo the first.
    let wanted = !on.unwrap_or(false);
    let click = {
        let entity_id = entity_id.clone();
        move |event: ev::MouseEvent| {
            // A swipe already said what it wanted when it let go.
            if !crate::gesture::swallow_click(&event) {
                controls.set_on.run((entity_id.clone(), wanted));
            }
        }
    };
    // Swiped: whichever side the knob was let go on, if that's not where it already is.
    let let_go = {
        let entity_id = entity_id.clone();
        move |event: ev::PointerEvent| {
            if let Some(side) = crate::gesture::knob_up(&event)
                && Some(side) != on
            {
                controls.set_on.run((entity_id.clone(), side));
            }
        }
    };
    let label = format!("Turn {} {}", entity.name, if wanted { "on" } else { "off" });
    // A screen reader is told what the knob shows. `mixed` is how ARIA says "neither", which is
    // what an entity that has never reported is: saying `false` would claim it's off.
    let pressed = match on {
        Some(true) => "true",
        Some(false) => "false",
        None => "mixed",
    };
    view! {
        <button
            type="button"
            class="toggle"
            class:unknown=on.is_none()
            // Waiting on the device, as opposed to unable to reach it: both disable the switch,
            // but only this one is going to change.
            class:pending=pending
            aria-label=label
            aria-pressed=pressed
            disabled=move || offline || busy()
            on:click=click
            on:pointerdown=|event| crate::gesture::knob_down(&event)
            on:pointermove=|event| crate::gesture::knob_move(&event)
            on:pointerup=let_go
            on:pointercancel=|event| {
                crate::gesture::knob_up(&event);
            }
        >
            <span class="knob"></span>
        </button>
    }
}

fn sensor(capabilities: &SensorCapabilities, value: Option<&State>) -> AnyView {
    let (reading, unit, counts) = match value {
        Some(State::Sensor(sensor)) => {
            let unit = capabilities.unit.clone().unwrap_or_default();
            match &sensor.value {
                SensorValue::Number(n) => (number(*n), unit, Some(n.to_string())),
                SensorValue::Text(text) => (text.clone(), unit, None),
            }
        }
        _ => (UNKNOWN.to_owned(), String::new(), None),
    };
    // A number counts to its next value as it changes (count.rs); words just change.
    view! {
        <span class="reading">
            <span class="n" data-n=counts>{reading}</span>
            <span class="unit">{(!unit.is_empty()).then(|| format!(" {unit}"))}</span>
        </span>
    }
    .into_any()
}

fn binary(class: Option<BinarySensorClass>, value: Option<&State>) -> AnyView {
    let on = match value {
        Some(State::BinarySensor(sensor)) => Some(sensor.on),
        _ => None,
    };
    let reading = on.map_or(UNKNOWN, |on| wording(class, on));
    view! { <span class="reading" class:on=on.unwrap_or(false)>{reading}</span> }.into_any()
}

const UNKNOWN: &str = "unknown";

/// What a binary sensor's `true` and `false` mean in words. Without a device class there's
/// nothing better to say than on and off.
pub(crate) fn wording(class: Option<BinarySensorClass>, on: bool) -> &'static str {
    use BinarySensorClass::*;
    // What `on` means follows Home Assistant's classes: a `lock` that's on is unlocked, a
    // `battery` that's on is low.
    let (yes, no) = match class {
        Some(Motion | Vibration) => ("Motion", "Still"),
        Some(Occupancy | Presence) => ("Detected", "Clear"),
        Some(Door | GarageDoor | Window | Opening) => ("Open", "Closed"),
        Some(Moisture) => ("Wet", "Dry"),
        Some(Smoke) => ("Smoke", "Clear"),
        Some(Gas) => ("Gas", "Clear"),
        Some(CarbonMonoxide) => ("Carbon monoxide", "Clear"),
        Some(GlassBreak) => ("Glass broken", "Clear"),
        Some(Sound) => ("Sound", "Quiet"),
        Some(Tamper) => ("Tampered", "Clear"),
        Some(Plug) => ("Plugged in", "Unplugged"),
        Some(Power) => ("Powered", "No power"),
        Some(Connectivity) => ("Connected", "Disconnected"),
        Some(Battery) => ("Low", "OK"),
        Some(BatteryCharging) => ("Charging", "Not charging"),
        Some(Cold) => ("Cold", "Normal"),
        Some(Heat) => ("Hot", "Normal"),
        Some(Light) => ("Light", "Dark"),
        Some(Lock) => ("Unlocked", "Locked"),
        Some(Moving) => ("Moving", "Stopped"),
        Some(Running) => ("Running", "Stopped"),
        Some(Problem) => ("Problem", "OK"),
        Some(Safety) => ("Unsafe", "Safe"),
        Some(Update) => ("Update available", "Up to date"),
        None => ("On", "Off"),
    };
    if on { yes } else { no }
}

/// A reading a person can read: whole numbers stay whole, the rest keep up to three decimals
/// without trailing zeros. 22.299999999999997 is 22.3.
pub(crate) fn number(n: f64) -> String {
    if !n.is_finite() {
        return UNKNOWN.to_owned();
    }
    let mut text = format!("{n:.3}");
    if text.contains('.') {
        text.truncate(text.trim_end_matches('0').trim_end_matches('.').len());
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_are_readable() {
        assert_eq!(number(22.299999999999997), "22.3");
        assert_eq!(number(21.0), "21");
        assert_eq!(number(-0.5), "-0.5");
        assert_eq!(number(1234.56789), "1234.568");
        assert_eq!(number(f64::NAN), UNKNOWN);
    }

    fn entity_home() -> Home {
        let device = Device {
            id: "radar".parse().expect("valid"),
            protocol: "esphome".parse().expect("valid"),
            unique_id: "00:11:22:33:44:55".parse().expect("valid"),
            name: "Radar".parse().expect("valid"),
            description: None,
            manufacturer: Some("Espressif".into()),
            model: Some("rd-03d".into()),
            sw_version: None,
            hw_version: None,
            area_id: Some("hall".parse().expect("valid")),
            suggested_area: None,
            via_device_id: None,
        };
        let entity = Entity {
            id: "binary_sensor.radar_moving".parse().expect("valid"),
            protocol: device.protocol.clone(),
            unique_id: "moving".parse().expect("valid"),
            name: "Moving".parse().expect("valid"),
            device_id: Some(device.id.clone()),
            area_id: None,
            capabilities: Capabilities::BinarySensor(BinarySensorCapabilities {
                device_class: None,
            }),
            entity_category: None,
        };
        Home {
            devices: vec![device],
            entities: vec![entity],
            areas: vec![irori_types::Area {
                id: "hall".parse().expect("valid"),
                name: "Hall".parse().expect("valid"),
                floor_id: None,
            }],
            ..Home::default()
        }
    }

    /// The Devices and Entities views share one search box. Typing an area or a make has to
    /// keep the entity, not look like a broken filter, and something that isn't there finds
    /// nothing.
    #[test]
    fn filtering_entities_matches_area_and_make() {
        let home = entity_home();
        assert_eq!(groups(&home, "hall").len(), 1);
        assert_eq!(groups(&home, "espressif").len(), 1);
        assert_eq!(groups(&home, "rd-03").len(), 1);
        assert!(groups(&home, "nowhere").is_empty());
    }

    /// The row shows a light's level as a percentage, and the slider turns that percentage back
    /// into a level. The two must agree in both directions, or dimming would read one thing and
    /// set another.
    #[test]
    fn a_light_round_trips_between_level_and_percentage() {
        assert_eq!(brightness_pct(0), 0);
        assert_eq!(brightness_pct(1), 1);
        assert_eq!(brightness_pct(180), 71);
        assert_eq!(brightness_pct(255), 100);
        assert_eq!(pct_to_brightness(1), 2);
        assert_eq!(pct_to_brightness(50), 127);
        assert_eq!(pct_to_brightness(71), 181);
        assert_eq!(pct_to_brightness(100), 255);
        assert_eq!(pct_to_brightness(u16::MAX), 255, "a level can't exceed 255");
        for pct in 1..=100 {
            assert_eq!(
                brightness_pct(pct_to_brightness(pct)),
                pct,
                "level for {pct}% must read back as {pct}%"
            );
        }
    }

    /// A slider's filled part ends where its thumb is, at either end of any range — and a value
    /// outside the range, or a range with nothing in it, can't push the fill off the track.
    #[test]
    fn a_slider_fills_up_to_its_thumb() {
        assert_eq!(fill(1, 1, 100), 0);
        assert_eq!(fill(100, 1, 100), 100);
        assert_eq!(fill(2700, 2000, 6500), 15);
        assert_eq!(fill(9000, 2000, 6500), 100);
        assert_eq!(fill(0, 2000, 6500), 0);
        assert_eq!(fill(50, 50, 50), 100);
    }

    #[test]
    fn colors_round_trip_between_the_page_and_the_light() {
        assert_eq!(hex_from_rgb([255, 0, 128]), "#ff0080");
        assert_eq!(hex_from_rgb([0, 0, 0]), "#000000");
        assert_eq!(rgb_from_hex("#ff0080"), Some([255, 0, 128]));
        assert_eq!(rgb_from_hex("ff0080"), Some([255, 0, 128]));
        assert_eq!(
            rgb_from_hex("FF00FF"),
            Some([255, 0, 255]),
            "upper case is a color too"
        );
        assert_eq!(
            rgb_from_hex("#fff"),
            None,
            "short form isn't what the input gives"
        );
        assert_eq!(rgb_from_hex("#ff00"), None);
        assert_eq!(rgb_from_hex("#gg0000"), None);
        assert_eq!(rgb_from_hex(""), None);
        for rgb in [[255, 255, 255], [12, 34, 56]] {
            assert_eq!(rgb_from_hex(&hex_from_rgb(rgb)), Some(rgb));
        }
    }
}
