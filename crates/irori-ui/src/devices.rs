//! The Devices page: everything in the home, grouped by the device it belongs to, with a switch
//! for the things that can be switched.

use std::collections::{BTreeMap, BTreeSet};

use irori_types::{
    Availability, BinarySensorClass, Capabilities, Device, Entity, EntityId, EntityState,
    SensorCapabilities, SensorValue, State,
};
use leptos::prelude::*;

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
    device: Option<Device>,
    entities: Vec<(Entity, Option<EntityState>)>,
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

pub fn view(home: &Home, needle: &str, controls: Controls) -> AnyView {
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

fn row(entity: Entity, state: Option<EntityState>, controls: Controls) -> AnyView {
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
