//! The Devices page: everything in the home, grouped by the device it belongs to, with a switch
//! for the things that can be switched.

use std::collections::{BTreeMap, BTreeSet};

use irori_types::{
    AreaId, Availability, BinarySensorCapabilities, BinarySensorClass, Capabilities, Device,
    Entity, EntityId, EntityState, SensorCapabilities, SensorClass, SensorValue, State,
};
use leptos::prelude::*;
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

/// Groups the home by device, keeping only what matches `needle`, and sorts everything by name so
/// the page doesn't reshuffle between refreshes.
pub fn groups(home: &Home, needle: &str) -> Vec<Group> {
    let needle = needle.trim().to_lowercase();
    let states: BTreeMap<_, _> = home.states.iter().map(|s| (&s.entity_id, s)).collect();
    let devices: BTreeMap<_, _> = home.devices.iter().map(|d| (&d.id, d)).collect();

    // A device's name is part of what its entities are called in conversation ("the lamp in the
    // hallway sensor"), so typing it keeps the whole device.
    let matches = |entity: &Entity| {
        needle.is_empty()
            || entity.id.to_string().to_lowercase().contains(&needle)
            || entity.name.as_str().to_lowercase().contains(&needle)
            || entity
                .device_id
                .as_ref()
                .and_then(|id| devices.get(id))
                .is_some_and(|device| device.name.as_str().to_lowercase().contains(&needle))
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

/// Which way the list is shown. Remembered per browser, because it's a preference about
/// reading rather than anything Irori needs to know.
const VIEW_KEY: &str = "irori.devices.view";

#[component]
pub fn Devices() -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let controls = expect_context::<Controls>();
    let filter = RwSignal::new(String::new());
    let adding = RwSignal::new(false);
    let as_table = RwSignal::new(remembered_view());

    Effect::new(move |_| remember_view(as_table.get()));

    view! {
        <div class="page-head">
            <h1>"Devices"</h1>
            <div class="switcher" role="group" aria-label="How to show the devices">
                <button
                    type="button"
                    class:chosen=move || !as_table.get()
                    aria-pressed=move || (!as_table.get()).to_string()
                    on:click=move |_| as_table.set(false)
                >
                    "Entities"
                </button>
                <button
                    type="button"
                    class:chosen=move || as_table.get()
                    aria-pressed=move || as_table.get().to_string()
                    on:click=move |_| as_table.set(true)
                >
                    "Devices"
                </button>
            </div>
            <button type="button" class="add" on:click=move |_| adding.update(|a| *a = !*a)>
                {move || if adding.get() { "Close" } else { "+ Add device" }}
            </button>
        </div>

        {move || adding.get().then(|| view! { <AddDevice /> })}

        <input
            class="filter"
            type="search"
            placeholder="Filter by name or id"
            aria-label="Filter devices"
            prop:value=filter
            on:input:target=move |ev| filter.set(ev.target().value())
        />

        {move || {
            let home = live.home.get();
            let needle = filter.get();
            if as_table.get() {
                table(&home, &needle)
            } else {
                view(&home, &needle, controls)
            }
        }}
    }
}

/// One row per device: what it is and where it came from, rather than what it's doing.
///
/// Built from the devices rather than from their entities, so a device Irori is connected to
/// still appears when it provides nothing Irori can model — a Bluetooth proxy, say. Those are
/// invisible in the entity view by their nature, and being unable to find them would be worse.
fn table(home: &Home, needle: &str) -> AnyView {
    let needle = needle.trim().to_lowercase();
    let matches = |device: &Device| {
        let haystack = [
            device.name.to_string(),
            device.id.to_string(),
            device.integration.to_string(),
            device.manufacturer.clone().unwrap_or_default(),
            device.model.clone().unwrap_or_default(),
            // Typing a room's name is one of the most useful things to be able to type.
            home.room_of(device).unwrap_or_default(),
        ];
        needle.is_empty()
            || haystack
                .iter()
                .any(|field| field.to_lowercase().contains(&needle))
    };
    let mut devices: Vec<_> = home
        .devices
        .iter()
        .filter(|device| matches(device))
        .map(|device| {
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
            (device.clone(), entities)
        })
        .collect();
    devices.sort_by(|(a, _), (b, _)| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
    // Room names by area id, so each row is a lookup rather than a scan of every area.
    let rooms: BTreeMap<Option<AreaId>, String> = home
        .areas
        .iter()
        .map(|area| (Some(area.id.clone()), area.name.to_string()))
        .collect();
    if devices.is_empty() {
        let message = if home.devices.is_empty() {
            "No devices yet. Extensions bring them in; \"Add device\" says how."
        } else {
            "Nothing matches that."
        };
        return view! { <p class="empty">{message}</p> }.into_any();
    }
    view! {
        <div class="table-scroll">
            <table class="devices">
                <thead>
                    <tr>
                        <th scope="col">"Device"</th>
                        <th scope="col">"Room"</th>
                        <th scope="col">"Through"</th>
                        <th scope="col">"Make"</th>
                        <th scope="col">"Model"</th>
                        <th scope="col">"Battery"</th>
                        <th scope="col" class="number">"Entities"</th>
                    </tr>
                </thead>
                <tbody>
                    {devices
                        .into_iter()
                        .map(|(device, entities)| {
                            let id = device.id.to_string();
                            let entity_count = entities.len();
                            let battery = battery(&entities);
                            let room = rooms.get(&device.area_id).cloned();
                            view! {
                                <tr>
                                    <th scope="row">
                                        <A href=format!("/devices/{id}")>{device.name.to_string()}</A>
                                        <span class="id">{id}</span>
                                    </th>
                                    <td class="room">{room.unwrap_or_else(|| "—".to_owned())}</td>
                                    <td>{device.integration.to_string()}</td>
                                    <td>{device.manufacturer.clone().unwrap_or_default()}</td>
                                    <td>{device.model.clone().unwrap_or_default()}</td>
                                    <td class="battery">{battery.unwrap_or_else(|| "—".to_owned())}</td>
                                    <td class="number">{entity_count}</td>
                                </tr>
                            }
                        })
                        .collect_view()}
                </tbody>
            </table>
        </div>
    }
    .into_any()
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

/// The list style this browser was last shown. Browser storage can be unavailable or refused,
/// and it only holds a preference, so anything unexpected just means the default.
fn remembered_view() -> bool {
    window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|storage| storage.get_item(VIEW_KEY).ok().flatten())
        .is_some_and(|value| value == "devices")
}

fn remember_view(as_table: bool) {
    if let Ok(Some(storage)) = window().local_storage() {
        let _ = storage.set_item(VIEW_KEY, if as_table { "devices" } else { "entities" });
    }
}

/// Where devices come from, and how to get more of them.
///
/// There's no "scan now" button because there's nothing to scan on demand: integrations that
/// find devices are always listening. And there's no way to adopt one device but not another
/// yet — that needs somewhere to record the decision, which is the config dir (M0.7).
#[component]
fn AddDevice() -> impl IntoView {
    let live = expect_context::<crate::Live>();

    view! {
        <section class="card add-device">
            <h2>"Where devices come from"</h2>
            <p class="muted">
                "Irori doesn't talk to devices itself: each kind of device arrives through an "
                "extension. These are the ones this build has."
            </p>
            <ul class="integrations">
                {move || {
                    let home = live.home.get();
                    home.extensions
                        .iter()
                        .map(|(id, extension)| {
                            let devices = home
                                .devices
                                .iter()
                                .filter(|device| device.integration.as_str() == id.as_str())
                                .count();
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
                "listening, so flashing a board or plugging one in is all it takes. Choosing "
                "which found devices to keep, giving one an address by hand, and holding the "
                "keys encrypted devices need all wait for Irori's config dir (M0.7)."
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
}
