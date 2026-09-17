//! The Devices page: everything in the home, grouped by the device it belongs to, with a switch
//! for the things that can be switched.

use std::collections::{BTreeMap, BTreeSet};

use irori_types::{
    AreaId, Availability, BinarySensorCapabilities, BinarySensorClass, Capabilities, Device,
    Entity, EntityId, EntityState, SensorCapabilities, SensorClass, SensorValue, State,
};
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
}

/// One device and the entities it provides. `device` is `None` for entities that belong to no
/// device, which the integration contract allows.
#[derive(Debug)]
pub struct Group {
    pub device: Option<Device>,
    pub entities: Vec<(Entity, Option<EntityState>)>,
}

/// Whether a device matches the shared Devices/Entities search: name, id, room, make, model.
fn matches_device(home: &Home, device: &Device, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let haystack = [
        device.name.to_string(),
        device.id.to_string(),
        device.integration.to_string(),
        device
            .description
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        device.manufacturer.clone().unwrap_or_default(),
        device.model.clone().unwrap_or_default(),
        home.room_of(device).unwrap_or_default(),
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
    // hallway sensor"), so typing it keeps the whole device. Room and make too: the same
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
            group
                .entities
                .sort_by(|(a, _), (b, _)| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
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
    /// One row per device, grouped by the integration it came through. The default: a device is
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
    let showing = RwSignal::new(remembered_view());
    let folded = RwSignal::new(remembered_folded());
    provide_context(HelperTrouble(RwSignal::new(None)));
    let waiting = crate::waiting::everything_waiting(live);

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
            <button type="button" class="add" on:click=move |_| adding.update(|a| *a = !*a)>
                {move || if adding.get() { "Close" } else { "+ Add device" }}
            </button>
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
        {move || adding.get().then(|| view! { <AddDevice /> })}
        <NewDevices />

        {move || (showing.get() != Showing::Helpers).then(|| view! {
            <input
                class="filter"
                type="search"
                placeholder="Filter by name, id, room or make"
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
        // Outside the block above, which redraws on every reading: an opened list mustn't snap
        // shut two seconds later (ROADMAP D33).
        {move || (showing.get() == Showing::Devices).then(|| view! { <Ignored /> })}
        {move || (showing.get() == Showing::Helpers).then(|| view! { <AddToggle /> })}
    }
}

/// A device with its entities and what they're reporting: one row of the device table.
type DeviceRow = (Device, Vec<(Entity, Option<EntityState>)>);

/// One row per device: what it is and where it is, grouped by the integration it came through.
///
/// Built from the devices rather than from their entities, so a device Irori is connected to
/// still appears when it provides nothing Irori can model — a Bluetooth proxy, say. Those are
/// invisible in the entity view by their nature, and being unable to find them would be worse.
fn table(home: &Home, needle: &str, folded: RwSignal<BTreeSet<String>>) -> AnyView {
    let needle = needle.trim().to_lowercase();
    let mut by_integration: BTreeMap<String, Vec<DeviceRow>> = BTreeMap::new();
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
        by_integration
            .entry(device.integration.to_string())
            .or_default()
            .push((device.clone(), entities));
    }
    if by_integration.is_empty() {
        let message = if home.devices.is_empty() {
            "No devices yet. Extensions bring them in; \"Add device\" says how."
        } else {
            "Nothing matches that."
        };
        return view! { <p class="empty">{message}</p> }.into_any();
    }
    let rooms: BTreeMap<Option<AreaId>, String> = home
        .areas
        .iter()
        .map(|area| (Some(area.id.clone()), area.name.to_string()))
        .collect();
    let mut groups: Vec<_> = by_integration
        .into_iter()
        .map(|(integration, mut devices)| {
            devices.sort_by(|(a, _), (b, _)| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
            let extension = home
                .extensions
                .iter()
                .find(|(id, _)| id.as_str() == integration);
            let name = extension
                .map(|(_, extension)| extension.name.clone())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| integration.clone());
            let has_icon = extension.is_some_and(|(_, extension)| extension.has_icon);
            (integration, name, has_icon, devices)
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
                            <span class="visually-hidden">"Integration"</span>
                        </th>
                        <th scope="col">"Device"</th>
                        <th scope="col">"Room"</th>
                        <th scope="col">"Make"</th>
                        <th scope="col">"Model"</th>
                        <th scope="col">"Battery"</th>
                        <th scope="col" class="number">"Entities"</th>
                    </tr>
                </thead>
                {groups
                    .into_iter()
                    .map(|(integration, name, has_icon, devices)| {
                        group(integration, name, has_icon, devices, &rooms, folded, filtering)
                    })
                    .collect_view()}
            </table>
        </div>
    }
    .into_any()
}

/// One integration's devices: a header that folds them away, and a row each.
fn group(
    integration: String,
    name: String,
    has_icon: bool,
    devices: Vec<DeviceRow>,
    rooms: &BTreeMap<Option<AreaId>, String>,
    folded: RwSignal<BTreeSet<String>>,
    filtering: bool,
) -> AnyView {
    let open = {
        let key = integration.clone();
        move || filtering || !folded.get().contains(&key)
    };
    let toggle = {
        let key = integration.clone();
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
            let room = rooms.get(&device.area_id).cloned();
            view! {
                <tr>
                    <td class="icon-col">{icon(&integration, has_icon)}</td>
                    <th scope="row">
                        <A href=format!("/devices/{id}")>{device.name.to_string()}</A>
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
    let header_icon = icon(&integration, has_icon);
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

/// An integration's icon, or its initial when it has none. Always an `<img>`: an extension's SVG
/// is loaded as an image, where it can't run script (`docs/specs/extensions.md`).
pub fn icon(integration: &str, has_icon: bool) -> AnyView {
    if has_icon {
        view! {
            <img
                class="integration-icon"
                src=format!("/api/dev/extensions/{integration}/icon.svg")
                alt=""
                width="20"
                height="20"
            />
        }
        .into_any()
    } else {
        let initial = integration
            .chars()
            .next()
            .map(|c| c.to_ascii_uppercase().to_string())
            .unwrap_or_default();
        view! { <span class="integration-icon letter" aria-hidden="true">{initial}</span> }
            .into_any()
    }
}

/// Devices a person keeps out of the home, each with a way back in. Only shown when there are some.
#[component]
fn Ignored() -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let ignored = Memo::new(move |_| {
        live.home
            .get()
            .held
            .into_iter()
            .filter(|held| held.why == "ignored")
            .collect::<Vec<_>>()
    });
    let trouble = RwSignal::new(None::<String>);
    move || {
        let all = ignored.get();
        (!all.is_empty()).then(|| {
            let count = all.len();
            view! {
                <details class="card ignored">
                    <summary>
                        {format!("{count} ignored device{}", if count == 1 { "" } else { "s" })}
                    </summary>
                    <p class="muted small">
                        "Kept out of Irori. Their integrations may still talk to them; nothing "
                        "they say reaches the home."
                    </p>
                    {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
                    <ul class="room-devices">
                        {all
                            .into_iter()
                            .map(|device| {
                                let id = device.id.clone();
                                let let_back = move |_| {
                                    let id = id.clone();
                                    spawn_local(async move {
                                        let edit = crate::api::DeviceEdit {
                                            ignored: Some(false),
                                            added: Some(true),
                                            ..Default::default()
                                        };
                                        match crate::api::edit_device(&id, &edit).await {
                                            Ok(()) => crate::refresh(live),
                                            Err(why) => trouble.set(Some(why)),
                                        }
                                    });
                                };
                                view! {
                                    <li>
                                        <span class="name">{device.name.to_string()}</span>
                                        <span class="muted small">{device.integration.clone()}</span>
                                        <button type="button" class="link" on:click=let_back>
                                            "Let back in"
                                        </button>
                                    </li>
                                }
                            })
                            .collect_view()}
                    </ul>
                </details>
            }
        })
    }
}

/// Devices found while Irori asks before adding them (`irori.toml`, `[devices] new = "ask"`),
/// each waiting for a person to add or ignore it. Nothing shows when nothing waits.
#[component]
fn NewDevices() -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let waiting = Memo::new(move |_| {
        live.home
            .get()
            .held
            .into_iter()
            .filter(|held| held.why == "new")
            .collect::<Vec<_>>()
    });
    let trouble = RwSignal::new(None::<String>);
    let decide = move |ids: Vec<irori_types::DeviceId>, add: bool| {
        spawn_local(async move {
            for id in ids {
                let edit = crate::api::DeviceEdit {
                    added: add.then_some(true),
                    ignored: (!add).then_some(true),
                    ..Default::default()
                };
                if let Err(why) = crate::api::edit_device(&id, &edit).await {
                    trouble.set(Some(why));
                    break;
                }
            }
            crate::refresh(live);
        });
    };
    move || {
        let all = waiting.get();
        (!all.is_empty()).then(|| {
            let count = all.len();
            let every: Vec<_> = all.iter().map(|device| device.id.clone()).collect();
            view! {
                <section class="card waiting new-devices">
                    <div class="room-head">
                        <h2>
                            {format!("{count} new device{} found", if count == 1 { "" } else { "s" })}
                        </h2>
                        {(count > 1).then(|| {
                            let every = every.clone();
                            view! {
                                <span class="room-actions">
                                    <button type="button" on:click=move |_| decide(every.clone(), true)>
                                        "Add all"
                                    </button>
                                </span>
                            }
                        })}
                    </div>
                    <p class="muted small">
                        "Irori asks before adding what it finds. Add a device to use it, or ignore "
                        "it to stop being asked."
                    </p>
                    {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
                    <ul class="room-devices">
                        {all
                            .into_iter()
                            .map(|device| {
                                let (add, ignore) = (device.id.clone(), device.id.clone());
                                view! {
                                    <li>
                                        {icon(&device.integration, has_icon(live, &device.integration))}
                                        <span class="name">{device.name.to_string()}</span>
                                        <span class="muted small">{device.integration.clone()}</span>
                                        <span class="room-actions">
                                            <button type="button" on:click=move |_| decide(vec![add.clone()], true)>
                                                "Add"
                                            </button>
                                            <button type="button" on:click=move |_| decide(vec![ignore.clone()], false)>
                                                "Ignore"
                                            </button>
                                        </span>
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

fn has_icon(live: crate::Live, integration: &str) -> bool {
    live.home
        .get_untracked()
        .extensions
        .iter()
        .any(|(id, extension)| id.as_str() == integration && extension.has_icon)
}

/// The helpers in the home: the switches Irori keeps itself, each with its switch and a way to
/// remove it. Redrawn with every reading, so it holds nothing a person is typing.
fn helpers(home: &Home, controls: Controls) -> AnyView {
    let states: BTreeMap<_, _> = home.states.iter().map(|s| (&s.entity_id, s)).collect();
    let mut toggles: Vec<_> = home
        .entities
        .iter()
        .filter(|entity| entity.integration.as_str() == HELPERS)
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
                    "left through restarts, and rules (M1.4) will be able to read and flip it."
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

/// The integration that keeps helpers.
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
            let Some(named) = crate::rooms::named(typed, trouble) else {
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
fn AddToggle() -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let trouble = expect_context::<HelperTrouble>().0;
    let name = RwSignal::new(String::new());
    let add = move || {
        let Some(named) = crate::rooms::named(name.get(), trouble) else {
            return;
        };
        name.set(String::new());
        spawn_local(async move {
            match crate::api::add_toggle(named).await {
                Ok(()) => {
                    trouble.set(None);
                    crate::refresh(live);
                }
                Err(why) => trouble.set(Some(why)),
            }
        });
    };
    view! {
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
                "Add toggle"
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

/// Where devices come from, and how to get more of them.
///
/// There's no "scan now" button because there's nothing to scan on demand: integrations that
/// find devices are always listening. What a person *can* do here is unlock what they found but
/// couldn't use — a device waiting for its encryption key.
#[component]
fn AddDevice() -> impl IntoView {
    let live = expect_context::<crate::Live>();
    // The extensions and how many devices each has — not the readings. A redraw on every sensor
    // report would throw away a key being pasted into the form above (ROADMAP D33).
    let extensions = Memo::new(move |_| {
        let home = live.home.get();
        home.extensions
            .iter()
            .map(|(id, extension)| {
                let devices = home
                    .devices
                    .iter()
                    .filter(|device| device.integration.as_str() == id.as_str())
                    .count();
                (extension.clone(), devices)
            })
            .collect::<Vec<_>>()
    });

    view! {
        <crate::waiting::Waiting />
        <section class="card add-device">
            <h2>"Where devices come from"</h2>
            <p class="muted">
                "Irori doesn't talk to devices itself: each kind of device arrives through an "
                "extension. These are the ones this build has."
            </p>
            <ul class="integrations">
                {move || {
                    extensions
                        .get()
                        .into_iter()
                        .map(|(extension, devices)| {
                            let kinds = extension.entity_kinds.join(", ");
                            view! {
                                <li>
                                    <div class="integration-head">
                                        <span class="name">{extension.name.clone()}</span>
                                        <span class="badge">{how(&extension.iot_class)}</span>
                                        <span class="state" class:ok=extension.state == "running">
                                            {extension.state.clone()}
                                        </span>
                                    </div>
                                    <p class="muted">
                                        {extension.description.clone().unwrap_or_default()}
                                    </p>
                                    <p class="muted small">
                                        {format!(
                                            "Provides {kinds}. {devices} device{} here now.",
                                            if devices == 1 { "" } else { "s" },
                                        )}
                                    </p>
                                </li>
                            }
                        })
                        .collect_view()
                }}
            </ul>
            <p class="muted small">
                "Devices appear on their own: an extension that can find them is always "
                "listening, so flashing a board or plugging one in is all it takes. A device that "
                "encrypts its connection shows up above until it has its key. To choose which "
                "found devices to keep, set `[devices] new = \"ask\"` in irori.toml — they wait "
                "here with Add and Ignore instead of joining on their own. Giving one an address "
                "by hand is still to come."
            </p>
        </section>
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
                .map(|(entity, state)| row(entity, state, controls))
                .collect_view()}
        </section>
    }
    .into_any()
}

pub fn row(entity: Entity, state: Option<EntityState>, controls: Controls) -> AnyView {
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
            {control(&entity, state.as_ref(), offline, controls)}
            {move || failure().map(|why| view! { <p class="why">{why}</p> })}
        </div>
    }
    .into_any()
}

fn control(
    entity: &Entity,
    state: Option<&EntityState>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let value = state.and_then(|s| s.state.as_ref());
    match &entity.capabilities {
        Capabilities::Light(_) | Capabilities::Switch(_) => {
            let on = match value {
                Some(State::Light(light)) => Some(light.on),
                Some(State::Switch(switch)) => Some(switch.on),
                _ => None,
            };
            let brightness = match value {
                Some(State::Light(light)) if light.on => light.brightness,
                _ => None,
            };
            toggle(entity, on, brightness, offline, controls)
        }
        Capabilities::Sensor(capabilities) => sensor(capabilities, value),
        Capabilities::BinarySensor(capabilities) => binary(capabilities.device_class, value),
    }
}

/// A switch showing what the entity is doing, not what was last clicked: it moves when the
/// device reports back. A light that has never reported sits in between, and clicking turns it
/// on.
fn toggle(
    entity: &Entity,
    on: Option<bool>,
    brightness: Option<u8>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let entity_id = entity.id.clone();
    let busy = {
        let entity_id = entity_id.clone();
        move || controls.busy.get().contains(&entity_id)
    };
    // What the click means is "I want it off", not "flip whatever it is now": the page sends the
    // state the person asked for, so a second click on a stale row can't undo the first.
    let wanted = !on.unwrap_or(false);
    let click = {
        let entity_id = entity_id.clone();
        move |_| controls.set_on.run((entity_id.clone(), wanted))
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
        <span class="reading">
            {brightness.map(|level| format!("{}%", (u16::from(level) * 100).div_ceil(255)))}
        </span>
        <button
            type="button"
            class="toggle"
            class:unknown=on.is_none()
            aria-label=label
            aria-pressed=pressed
            disabled=move || offline || busy()
            on:click=click
        >
            <span class="knob"></span>
        </button>
    }
    .into_any()
}

fn sensor(capabilities: &SensorCapabilities, value: Option<&State>) -> AnyView {
    let (reading, unit) = match value {
        Some(State::Sensor(sensor)) => (
            match &sensor.value {
                SensorValue::Number(n) => number(*n),
                SensorValue::Text(text) => text.clone(),
            },
            capabilities.unit.clone().unwrap_or_default(),
        ),
        _ => (UNKNOWN.to_owned(), String::new()),
    };
    view! {
        <span class="reading">
            {reading}
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
fn wording(class: Option<BinarySensorClass>, on: bool) -> &'static str {
    use BinarySensorClass::*;
    match (class, on) {
        (Some(Motion | Vibration), true) => "Motion",
        (Some(Motion | Vibration), false) => "Still",
        (Some(Occupancy), true) => "Occupied",
        (Some(Occupancy), false) => "Empty",
        (Some(Door | Window), true) => "Open",
        (Some(Door | Window), false) => "Closed",
        (Some(Moisture), true) => "Wet",
        (Some(Moisture), false) => "Dry",
        (Some(Smoke), true) => "Smoke",
        (Some(Gas), true) => "Gas",
        (Some(Smoke | Gas), false) => "Clear",
        (Some(Plug), true) => "Plugged in",
        (Some(Plug), false) => "Unplugged",
        (Some(Connectivity), true) => "Connected",
        (Some(Connectivity), false) => "Disconnected",
        (Some(Battery), true) => "Low",
        (Some(Problem), true) => "Problem",
        (Some(Problem | Battery), false) => "OK",
        (None, true) => "On",
        (None, false) => "Off",
    }
}

/// A reading a person can read: whole numbers stay whole, the rest keep up to three decimals
/// without trailing zeros. 22.299999999999997 is 22.3.
fn number(n: f64) -> String {
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
            integration: "esphome".parse().expect("valid"),
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
            integration: device.integration.clone(),
            unique_id: "moving".parse().expect("valid"),
            name: "Moving".parse().expect("valid"),
            device_id: Some(device.id.clone()),
            area_id: None,
            capabilities: Capabilities::BinarySensor(BinarySensorCapabilities {
                device_class: None,
            }),
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

    /// The Devices and Entities views share one search box. Typing a room or a make has to
    /// keep the entity, not look like a broken filter.
    #[test]
    fn filtering_entities_matches_room_and_make() {
        let home = entity_home();
        assert_eq!(groups(&home, "hall").len(), 1);
        assert_eq!(groups(&home, "espressif").len(), 1);
        assert_eq!(groups(&home, "rd-03").len(), 1);
        assert!(groups(&home, "nowhere").is_empty());
    }
}
