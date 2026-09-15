//! The home: the registry (what exists) and live state (what it's doing), changed one operation at
//! a time. Pure and synchronous, so every rule of the contract is easy to test. The spec's "what
//! the core checks" (`docs/specs/integrations.md` §8) lives here.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use irori_integration::{AvailabilityTarget, Rejected};
use irori_types::{
    Availability, Capabilities, ColorMode, Context, ContextId, Device, DeviceDescription, DeviceId,
    Entity, EntityDescription, EntityId, EntityKind, EntityState, IntegrationId, LightCapabilities,
    LightTurnOn, Name, Origin, SLUG_MAX_LEN, SensorValue, SensorValueType, Service, State,
    StateReport, Timestamp, UniqueId,
};

use crate::Event;
use crate::services::{CallError, Command};

/// When an operation happens, and the id for the context it creates if it changes something.
#[derive(Debug, Clone)]
pub(crate) struct Stamp {
    pub now: Timestamp,
    pub context_id: ContextId,
}

type Key = (IntegrationId, UniqueId);

/// How many recent service-call contexts to remember per integration, to check `caused_by`.
const RECENT_CALLS: usize = 64;

#[derive(Debug, Default)]
pub(crate) struct Home {
    devices: BTreeMap<DeviceId, Device>,
    device_keys: HashMap<Key, DeviceId>,
    entities: BTreeMap<EntityId, Entity>,
    entity_keys: HashMap<Key, EntityId>,
    states: BTreeMap<EntityId, EntityState>,
    /// Entities described without a name, which use (and follow) their device's name.
    nameless: HashSet<EntityId>,
    recent_calls: HashMap<IntegrationId, VecDeque<ContextId>>,
}

/// A service call resolved to the integration that handles it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub integration: IntegrationId,
    pub unique_id: UniqueId,
    pub service: Service,
}

impl Home {
    pub fn devices(&self) -> impl Iterator<Item = &Device> {
        self.devices.values()
    }

    pub fn entities(&self) -> impl Iterator<Item = &Entity> {
        self.entities.values()
    }

    pub fn states(&self) -> impl Iterator<Item = &EntityState> {
        self.states.values()
    }

    pub fn state(&self, entity_id: &EntityId) -> Option<&EntityState> {
        self.states.get(entity_id)
    }

    fn device_id(&self, integration: &IntegrationId, unique_id: &UniqueId) -> Option<&DeviceId> {
        self.device_keys
            .get(&(integration.clone(), unique_id.clone()))
    }

    fn entity_id(&self, integration: &IntegrationId, unique_id: &UniqueId) -> Option<&EntityId> {
        self.entity_keys
            .get(&(integration.clone(), unique_id.clone()))
    }

    // --- Registry -------------------------------------------------------------------------

    pub fn describe_device(
        &mut self,
        integration: &IntegrationId,
        description: DeviceDescription,
    ) -> Result<Vec<Event>, Rejected> {
        description
            .validate()
            .map_err(|e| Rejected(e.to_string()))?;
        let unique_id = &description.unique_id;
        let via = match &description.via_device_unique_id {
            Some(via) => Some(self.device_id(integration, via).cloned().ok_or_else(|| {
                Rejected(format!(
                    "device `{unique_id}`: via_device_unique_id `{via}` isn't a device this integration described"
                ))
            })?),
            None => None,
        };

        if let Some(id) = self.device_id(integration, unique_id).cloned() {
            if let Some(via_id) = &via
                && self.reaches(via_id, &id)
            {
                return Err(Rejected(format!(
                    "device `{unique_id}`: being reached via `{}` would make a loop",
                    description
                        .via_device_unique_id
                        .as_ref()
                        .map_or("", UniqueId::as_str)
                )));
            }
            let device = self.devices.get_mut(&id).expect("keys and devices agree");
            let before = device.clone();
            // Nobody can rename devices in Irori yet (config spec, M0.7), so the integration's
            // name is the only one. Once people can, a name they set wins.
            device.name = description.name;
            device.manufacturer = description.manufacturer;
            device.model = description.model;
            device.sw_version = description.sw_version;
            device.hw_version = description.hw_version;
            device.via_device_id = via;
            if *device == before {
                return Ok(vec![]);
            }
            let device = device.clone();
            let mut events = Vec::new();
            if device.name != before.name {
                for entity in self.entities.values_mut() {
                    if entity.device_id.as_ref() == Some(&id) && self.nameless.contains(&entity.id)
                    {
                        entity.name = device.name.clone();
                        events.push(Event::EntityUpdated {
                            entity: entity.clone(),
                        });
                    }
                }
            }
            events.insert(0, Event::DeviceUpdated { device });
            return Ok(events);
        }

        let id = unique_id_for(&slugify(description.name.as_str()), "device", |c| {
            DeviceId::try_from(c).is_ok_and(|id| self.devices.contains_key(&id))
        });
        let id = DeviceId::try_from(id).expect("unique_id_for returns a slug");
        let device = Device {
            id: id.clone(),
            integration: integration.clone(),
            unique_id: description.unique_id.clone(),
            name: description.name,
            manufacturer: description.manufacturer,
            model: description.model,
            sw_version: description.sw_version,
            hw_version: description.hw_version,
            // Areas arrive with the config spec (M0.7); `suggested_area` is used then.
            area_id: None,
            via_device_id: via,
        };
        self.device_keys
            .insert((integration.clone(), description.unique_id), id.clone());
        self.devices.insert(id, device.clone());
        Ok(vec![Event::DeviceAdded { device }])
    }

    /// Whether following `via_device_id` from `from` reaches `target`.
    fn reaches(&self, from: &DeviceId, target: &DeviceId) -> bool {
        let mut current = Some(from);
        for _ in 0..=self.devices.len() {
            match current {
                Some(id) if id == target => return true,
                Some(id) => current = self.devices.get(id).and_then(|d| d.via_device_id.as_ref()),
                None => return false,
            }
        }
        true
    }

    pub fn describe_entity(
        &mut self,
        integration: &IntegrationId,
        kinds: &[EntityKind],
        description: EntityDescription,
        stamp: &Stamp,
    ) -> Result<Vec<Event>, Rejected> {
        description
            .validate()
            .map_err(|e| Rejected(e.to_string()))?;
        let unique_id = &description.unique_id;
        let kind = description.kind();
        if !kinds.contains(&kind) {
            return Err(Rejected(format!(
                "entity `{unique_id}` is a {kind}, but the manifest's entity_kinds doesn't list {kind}"
            )));
        }
        let device = match &description.device_unique_id {
            Some(device) => {
                let id = self.device_id(integration, device).ok_or_else(|| {
                    Rejected(format!(
                        "entity `{unique_id}`: device_unique_id `{device}` isn't a device this integration described (describe the device first)"
                    ))
                })?;
                Some(&self.devices[id])
            }
            None => None,
        };
        let device_id = device.map(|d| d.id.clone());

        if let Some(id) = self.entity_id(integration, unique_id).cloned() {
            let entity = self.entities.get_mut(&id).expect("keys and entities agree");
            if entity.id.kind() != kind {
                return Err(Rejected(format!(
                    "entity `{unique_id}` is already a {}; it can't become a {kind} (use a different unique_id)",
                    entity.id.kind()
                )));
            }
            let before = entity.clone();
            let name = match (description.name, device) {
                (Some(name), _) => {
                    self.nameless.remove(&id);
                    name
                }
                (None, Some(device)) => {
                    self.nameless.insert(id.clone());
                    device.name.clone()
                }
                (None, None) => {
                    unreachable!("EntityDescription::validate requires a name or a device")
                }
            };
            let entity = self.entities.get_mut(&id).expect("keys and entities agree");
            // As for devices: the integration's name, until people can rename entities (M0.7).
            entity.name = name;
            entity.capabilities = description.capabilities;
            entity.device_id = device_id;
            let mut events = if *entity == before {
                vec![]
            } else {
                vec![Event::EntityUpdated {
                    entity: entity.clone(),
                }]
            };
            // If its abilities shrank (e.g. a firmware update removed RGB), a stored value that
            // no longer fits is forgotten: unknown until the integration reports again.
            let entity = self.entities[&id].clone();
            let old = self.states[&id].clone();
            if old
                .state
                .as_ref()
                .is_some_and(|state| fits(&entity, state).is_err())
            {
                // Irori's own change, not a report: `last_reported` stays.
                let now = stamp.now.max(old.last_updated);
                let mut new = old.clone();
                new.state = None;
                new.last_changed = now;
                new.last_updated = now;
                new.context = device_context(integration, stamp, None);
                self.states.insert(id.clone(), new.clone());
                events.push(Event::StateChanged {
                    entity_id: id.clone(),
                    old_state: Some(Box::new(old)),
                    new_state: Box::new(new),
                });
            }
            // Being described means the integration is back in touch with it (e.g. after a
            // restart). If the device is still offline, the integration says so next.
            let context = device_context(integration, stamp, None);
            events.extend(self.set_availability_of(
                vec![id],
                Availability::Available,
                stamp.now,
                context,
                Reported::No,
            ));
            return Ok(events);
        }

        let name: Name = match (&description.name, device) {
            (Some(name), _) => name.clone(),
            (None, Some(device)) => device.name.clone(),
            (None, None) => unreachable!("EntityDescription::validate requires a name or a device"),
        };
        let base = match (&description.suggested_object_id, &description.name, device) {
            (Some(object_id), _, _) => object_id.as_str().to_owned(),
            (None, Some(name), Some(device)) => {
                slugify(&format!("{} {}", device.name.as_str(), name.as_str()))
            }
            (None, _, _) => slugify(name.as_str()),
        };
        let object_id = unique_id_for(&base, kind.domain(), |c| {
            EntityId::new(kind, c).is_ok_and(|id| self.entities.contains_key(&id))
        });
        let id = EntityId::new(kind, &object_id).expect("unique_id_for returns a slug");
        let entity = Entity {
            id: id.clone(),
            integration: integration.clone(),
            unique_id: description.unique_id.clone(),
            name,
            device_id,
            area_id: None,
            capabilities: description.capabilities,
        };
        let state = EntityState {
            entity_id: id.clone(),
            availability: Availability::Available,
            state: None,
            attributes: BTreeMap::new(),
            last_changed: stamp.now,
            last_updated: stamp.now,
            last_reported: stamp.now,
            context: device_context(integration, stamp, None),
        };
        if description.name.is_none() {
            self.nameless.insert(id.clone());
        }
        self.entity_keys
            .insert((integration.clone(), description.unique_id), id.clone());
        self.entities.insert(id.clone(), entity.clone());
        self.states.insert(id.clone(), state.clone());
        Ok(vec![
            Event::EntityAdded { entity },
            Event::StateChanged {
                entity_id: id,
                old_state: None,
                new_state: Box::new(state),
            },
        ])
    }

    pub fn remove_entity(
        &mut self,
        integration: &IntegrationId,
        unique_id: &UniqueId,
    ) -> Result<Vec<Event>, Rejected> {
        let key = (integration.clone(), unique_id.clone());
        let id = self.entity_keys.remove(&key).ok_or_else(|| {
            Rejected(format!(
                "can't remove entity `{unique_id}`: this integration has no such entity"
            ))
        })?;
        self.entities.remove(&id);
        self.states.remove(&id);
        self.nameless.remove(&id);
        Ok(vec![Event::EntityRemoved { entity_id: id }])
    }

    pub fn remove_device(
        &mut self,
        integration: &IntegrationId,
        unique_id: &UniqueId,
    ) -> Result<Vec<Event>, Rejected> {
        let key = (integration.clone(), unique_id.clone());
        let id = self.device_keys.remove(&key).ok_or_else(|| {
            Rejected(format!(
                "can't remove device `{unique_id}`: this integration has no such device"
            ))
        })?;
        let mut events = Vec::new();
        let owned: Vec<Entity> = self
            .entities
            .values()
            .filter(|e| e.device_id.as_ref() == Some(&id))
            .cloned()
            .collect();
        for entity in owned {
            events.extend(self.remove_entity(&entity.integration, &entity.unique_id)?);
        }
        for device in self.devices.values_mut() {
            if device.via_device_id.as_ref() == Some(&id) {
                device.via_device_id = None;
                events.push(Event::DeviceUpdated {
                    device: device.clone(),
                });
            }
        }
        self.devices.remove(&id);
        events.push(Event::DeviceRemoved { device_id: id });
        Ok(events)
    }

    // --- State ----------------------------------------------------------------------------

    pub fn report_state(
        &mut self,
        integration: &IntegrationId,
        report: StateReport,
        stamp: &Stamp,
    ) -> Result<Vec<Event>, Rejected> {
        report.validate().map_err(|e| Rejected(e.to_string()))?;
        let unique_id = &report.unique_id;
        let id = self.entity_id(integration, unique_id).cloned().ok_or_else(|| {
            Rejected(format!(
                "state report for `{unique_id}`: this integration has no such entity (describe it first)"
            ))
        })?;
        if let Some(state) = &report.state {
            fits(&self.entities[&id], state)?;
        }
        let parent = match &report.caused_by {
            Some(caused_by)
                if self
                    .recent_calls
                    .get(integration)
                    .is_some_and(|calls| calls.contains(caused_by)) =>
            {
                Some(caused_by.clone())
            }
            Some(caused_by) => {
                return Err(Rejected(format!(
                    "state report for `{unique_id}`: caused_by `{caused_by}` isn't a recent service call sent to this integration"
                )));
            }
            None => None,
        };

        let old = self.states[&id].clone();
        // A clock that steps backwards never moves a timestamp back.
        let now = stamp.now.max(old.last_updated).max(old.last_reported);
        let state_changed = old.state != report.state;
        let changed = state_changed || old.attributes != report.attributes;
        let mut new = old.clone();
        new.last_reported = now;
        if changed {
            new.state = report.state;
            new.attributes = report.attributes;
            new.last_updated = now;
            if state_changed {
                new.last_changed = now;
            }
            new.context = device_context(integration, stamp, parent);
        }
        self.states.insert(id.clone(), new.clone());
        Ok(if changed {
            vec![Event::StateChanged {
                entity_id: id,
                old_state: Some(Box::new(old)),
                new_state: Box::new(new),
            }]
        } else {
            vec![]
        })
    }

    pub fn set_availability(
        &mut self,
        integration: &IntegrationId,
        target: AvailabilityTarget,
        availability: Availability,
        stamp: &Stamp,
    ) -> Result<Vec<Event>, Rejected> {
        let ids: Vec<EntityId> = match &target {
            AvailabilityTarget::Device(device) => {
                let device_id = self.device_id(integration, device).ok_or_else(|| {
                    Rejected(format!(
                        "can't set availability of device `{device}`: this integration has no such device"
                    ))
                })?;
                self.entities
                    .values()
                    .filter(|e| e.device_id.as_ref() == Some(device_id))
                    .map(|e| e.id.clone())
                    .collect()
            }
            AvailabilityTarget::Entities(unique_ids) => unique_ids
                .iter()
                .map(|u| {
                    self.entity_id(integration, u).cloned().ok_or_else(|| {
                        Rejected(format!(
                            "can't set availability of entity `{u}`: this integration has no such entity"
                        ))
                    })
                })
                .collect::<Result<_, _>>()?,
        };
        let context = device_context(integration, stamp, None);
        Ok(self.set_availability_of(ids, availability, stamp.now, context, Reported::Yes))
    }

    /// Marks every entity of an integration unavailable, e.g. because it crashed or stopped.
    pub fn mark_unavailable(&mut self, integration: &IntegrationId, stamp: &Stamp) -> Vec<Event> {
        let ids = self
            .entities
            .values()
            .filter(|e| &e.integration == integration)
            .map(|e| e.id.clone())
            .collect();
        let context = Context {
            id: stamp.context_id.clone(),
            parent_id: None,
            origin: Origin::System,
        };
        self.set_availability_of(
            ids,
            Availability::Unavailable,
            stamp.now,
            context,
            Reported::No,
        )
    }

    fn set_availability_of(
        &mut self,
        ids: Vec<EntityId>,
        availability: Availability,
        now: Timestamp,
        context: Context,
        reported: Reported,
    ) -> Vec<Event> {
        let mut events = Vec::new();
        for id in ids {
            let Some(old) = self.states.get_mut(&id) else {
                continue;
            };
            if old.availability == availability {
                // Nothing changed, but hearing it again from the integration is a report.
                if reported == Reported::Yes {
                    old.last_reported = now.max(old.last_reported);
                }
                continue;
            }
            let old = &*old;
            let now = now.max(old.last_updated);
            let mut new = old.clone();
            new.availability = availability;
            new.last_changed = now;
            new.last_updated = now;
            if reported == Reported::Yes {
                new.last_reported = now.max(old.last_reported);
            }
            new.context = context.clone();
            events.push(Event::StateChanged {
                entity_id: id.clone(),
                old_state: Some(Box::new(old.clone())),
                new_state: Box::new(new.clone()),
            });
            self.states.insert(id, new);
        }
        events
    }

    // --- Services -------------------------------------------------------------------------

    /// Turns a request on an entity into the service call its integration receives, checking
    /// what the entity supports. Resolves `Toggle` from the current state.
    pub fn resolve(&self, entity_id: &EntityId, command: Command) -> Result<Resolved, CallError> {
        let entity = self
            .entities
            .get(entity_id)
            .ok_or_else(|| CallError::UnknownEntity(entity_id.clone()))?;
        let is_on = || match self.states.get(entity_id).and_then(|s| s.state.as_ref()) {
            Some(State::Light(light)) => light.on,
            Some(State::Switch(switch)) => switch.on,
            _ => false,
        };
        let not_supported = |what: String| CallError::NotSupported(format!("`{entity_id}` {what}"));
        let service = match (&entity.capabilities, command) {
            (Capabilities::Light(_), Command::TurnOff) => Service::LightTurnOff,
            (Capabilities::Light(_), Command::Toggle) if is_on() => Service::LightTurnOff,
            (Capabilities::Light(_), Command::Toggle) => {
                Service::LightTurnOn(LightTurnOn::default())
            }
            (Capabilities::Light(caps), Command::TurnOn(data)) => {
                data.validate()
                    .map_err(|e| CallError::NotSupported(e.to_string()))?;
                light_supports(caps, &data).map_err(not_supported)?;
                Service::LightTurnOn(data)
            }
            (Capabilities::Switch(_), Command::TurnOn(data)) if data != LightTurnOn::default() => {
                return Err(not_supported(
                    "is a switch; it takes no brightness or color".into(),
                ));
            }
            (Capabilities::Switch(_), Command::TurnOn(_)) => Service::SwitchTurnOn,
            (Capabilities::Switch(_), Command::TurnOff) => Service::SwitchTurnOff,
            (Capabilities::Switch(_), Command::Toggle) if is_on() => Service::SwitchTurnOff,
            (Capabilities::Switch(_), Command::Toggle) => Service::SwitchTurnOn,
            (Capabilities::Sensor(_) | Capabilities::BinarySensor(_), _) => {
                return Err(not_supported(format!(
                    "is a {}; it has no services",
                    entity.id.kind()
                )));
            }
        };
        Ok(Resolved {
            integration: entity.integration.clone(),
            unique_id: entity.unique_id.clone(),
            service,
        })
    }

    /// Forgets a call that never reached its integration.
    pub fn forget_call(&mut self, integration: &IntegrationId, context_id: &ContextId) {
        if let Some(calls) = self.recent_calls.get_mut(integration) {
            calls.retain(|id| id != context_id);
        }
    }

    /// Remembers a call's context, so a state report can say it was caused by it.
    pub fn record_call(&mut self, integration: &IntegrationId, context_id: ContextId) {
        let calls = self.recent_calls.entry(integration.clone()).or_default();
        if calls.len() == RECENT_CALLS {
            calls.pop_front();
        }
        calls.push_back(context_id);
    }
}

/// Whether a change comes from the integration saying something (it moves `last_reported`
/// even when nothing changed), or from the core, e.g. after a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reported {
    Yes,
    No,
}

fn device_context(
    integration: &IntegrationId,
    stamp: &Stamp,
    parent: Option<ContextId>,
) -> Context {
    Context {
        id: stamp.context_id.clone(),
        parent_id: parent,
        origin: Origin::Device {
            integration: integration.clone(),
        },
    }
}

fn light_supports(caps: &LightCapabilities, data: &LightTurnOn) -> Result<(), String> {
    if data.brightness.is_some() && !caps.brightness {
        return Err("isn't dimmable".into());
    }
    if let Some(kelvin) = data.color_temp_kelvin {
        match caps.color_temp_kelvin {
            None => return Err("doesn't support color temperature".into()),
            Some(range) if !(range.min..=range.max).contains(&kelvin) => {
                return Err(format!(
                    "supports color temperatures from {} to {} K, not {kelvin} K",
                    range.min, range.max
                ));
            }
            Some(_) => {}
        }
    }
    if data.rgb.is_some() && !caps.rgb {
        return Err("doesn't support RGB color".into());
    }
    Ok(())
}

/// Whether a reported state fits the entity's kind and capabilities.
fn fits(entity: &Entity, state: &State) -> Result<(), Rejected> {
    let id = &entity.id;
    let reject = |what: String| Err(Rejected(format!("state report for `{id}`: {what}")));
    match (&entity.capabilities, state) {
        (caps, state) if caps.kind() != state.kind() => reject(format!(
            "it's a {}, but the report is for a {}",
            caps.kind(),
            state.kind()
        )),
        (Capabilities::Light(caps), State::Light(light)) => {
            match light.color_mode {
                Some(ColorMode::ColorTemp) if caps.color_temp_kelvin.is_none() => {
                    return reject(
                        "it's in color_temp mode, but doesn't support color temperature".into(),
                    );
                }
                Some(ColorMode::Rgb) if !caps.rgb => {
                    return reject("it's in rgb mode, but doesn't support RGB color".into());
                }
                _ => {}
            }
            let data = LightTurnOn {
                brightness: light.brightness,
                color_temp_kelvin: light.color_temp_kelvin,
                rgb: light.rgb,
            };
            match light_supports(caps, &data) {
                Ok(()) => Ok(()),
                Err(what) => reject(format!("it {what}")),
            }
        }
        (Capabilities::Sensor(caps), State::Sensor(sensor)) => {
            match (caps.value_type, &sensor.value) {
                (SensorValueType::Number, SensorValue::Text(text)) => reject(format!(
                    "it reports numbers, but the value is text {text:?}"
                )),
                (SensorValueType::Text, SensorValue::Number(n)) => {
                    reject(format!("it reports text, but the value is the number {n}"))
                }
                _ => Ok(()),
            }
        }
        _ => Ok(()),
    }
}

/// Lowercase ASCII letters and digits in `_`-separated words, at most 64 characters. Other
/// characters separate words, so `Küche Decke` becomes `k_che_decke` and a name with no ASCII
/// letters or digits becomes empty.
pub(crate) fn slugify(text: &str) -> String {
    let mut slug = String::new();
    let mut separate = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            if separate && !slug.is_empty() {
                slug.push('_');
            }
            separate = false;
            slug.push(c.to_ascii_lowercase());
        } else {
            separate = true;
        }
    }
    slug.truncate(SLUG_MAX_LEN);
    slug.trim_end_matches('_').to_owned()
}

/// `base` (or `fallback` if empty), with `_2`, `_3`, … appended until `taken` says it's free.
fn unique_id_for(base: &str, fallback: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = if base.is_empty() { fallback } else { base };
    if !taken(base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| {
            let suffix = format!("_{n}");
            let stem = base[..base.len().min(SLUG_MAX_LEN - suffix.len())].trim_end_matches('_');
            format!("{stem}{suffix}")
        })
        .find(|candidate| !taken(candidate))
        .expect("an unbounded range always finds a free id")
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::{
        BinarySensorCapabilities, ColorTempRange, LightState, SensorCapabilities, SensorState,
        SwitchCapabilities, SwitchState,
    };

    fn integration() -> IntegrationId {
        IntegrationId::try_from("demo").expect("valid")
    }

    fn uid(s: &str) -> UniqueId {
        UniqueId::try_from(s).expect("valid")
    }

    fn name(s: &str) -> Name {
        Name::try_from(s).expect("valid")
    }

    fn stamp(second: i64) -> Stamp {
        let now = Timestamp::from_jiff(jiff::Timestamp::from_second(second).expect("in range"));
        Stamp {
            now,
            context_id: crate::context_id::new_context_id(now),
        }
    }

    const ALL: [EntityKind; 4] = EntityKind::ALL;

    fn device(unique: &str, device_name: &str) -> DeviceDescription {
        DeviceDescription {
            unique_id: uid(unique),
            name: name(device_name),
            manufacturer: None,
            model: None,
            sw_version: None,
            hw_version: None,
            suggested_area: None,
            via_device_unique_id: None,
        }
    }

    fn entity(
        unique: &str,
        entity_name: Option<&str>,
        device: Option<&str>,
        caps: Capabilities,
    ) -> EntityDescription {
        EntityDescription {
            unique_id: uid(unique),
            name: entity_name.map(name),
            device_unique_id: device.map(uid),
            suggested_object_id: None,
            capabilities: caps,
        }
    }

    fn dimmable() -> Capabilities {
        Capabilities::Light(LightCapabilities {
            brightness: true,
            color_temp_kelvin: Some(ColorTempRange {
                min: 2700,
                max: 6500,
            }),
            rgb: false,
        })
    }

    fn report(unique: &str, state: Option<State>) -> StateReport {
        StateReport {
            unique_id: uid(unique),
            state,
            attributes: BTreeMap::new(),
            caused_by: None,
        }
    }

    fn light(on: bool, brightness: Option<u8>) -> State {
        State::Light(LightState {
            on,
            brightness,
            color_mode: None,
            color_temp_kelvin: None,
            rgb: None,
        })
    }

    /// A home with one lamp device and its nameless main light.
    fn home_with_lamp() -> Home {
        let mut home = Home::default();
        home.describe_device(&integration(), device("lamp", "Desk lamp"))
            .expect("device");
        home.describe_entity(
            &integration(),
            &ALL,
            entity("lamp-light", None, Some("lamp"), dimmable()),
            &stamp(0),
        )
        .expect("entity");
        home
    }

    fn lamp_id() -> EntityId {
        EntityId::try_from("light.desk_lamp").expect("valid")
    }

    #[test]
    fn ids_come_from_names_and_never_collide() {
        assert_eq!(slugify("Desk lamp"), "desk_lamp");
        assert_eq!(slugify("  Küche · Decke!! "), "k_che_decke");
        assert_eq!(slugify("玄関"), "");
        assert_eq!(unique_id_for("", "device", |_| false), "device");
        let taken = ["lamp", "lamp_2"];
        assert_eq!(
            unique_id_for("lamp", "device", |c| taken.contains(&c)),
            "lamp_3"
        );
        let long = "a".repeat(64);
        let id = unique_id_for(&long, "device", |c| c == long);
        assert_eq!(id.len(), 64);
        assert!(id.ends_with("_2"));

        let mut home = Home::default();
        for unique in ["a", "b"] {
            home.describe_device(&integration(), device(unique, "玄関 light"))
                .expect("device");
        }
        let ids: Vec<_> = home.devices().map(|d| d.id.to_string()).collect();
        assert_eq!(ids, ["light", "light_2"]);
    }

    #[test]
    fn entities_get_ids_from_device_and_entity_names() {
        let mut home = home_with_lamp();
        home.describe_device(&integration(), device("sensor", "Hallway sensor"))
            .expect("device");
        home.describe_entity(
            &integration(),
            &ALL,
            entity(
                "sensor-motion",
                Some("Motion"),
                Some("sensor"),
                Capabilities::BinarySensor(BinarySensorCapabilities::default()),
            ),
            &stamp(0),
        )
        .expect("entity");
        let ids: Vec<_> = home.entities().map(|e| e.id.to_string()).collect();
        assert_eq!(
            ids,
            ["binary_sensor.hallway_sensor_motion", "light.desk_lamp"]
        );
        // A nameless entity takes its device's name.
        assert_eq!(home.entities[&lamp_id()].name.as_str(), "Desk lamp");
        // New entities start available and unknown.
        let state = home.state(&lamp_id()).expect("state");
        assert_eq!(state.availability, Availability::Available);
        assert_eq!(state.state, None);
    }

    #[test]
    fn describing_again_updates_in_place() {
        let mut home = home_with_lamp();
        let again = home
            .describe_entity(
                &integration(),
                &ALL,
                entity("lamp-light", None, Some("lamp"), dimmable()),
                &stamp(1),
            )
            .expect("same entity");
        assert!(again.is_empty(), "nothing changed, so no events");
        assert_eq!(home.entities().count(), 1);

        let err = home
            .describe_entity(
                &integration(),
                &ALL,
                entity(
                    "lamp-light",
                    None,
                    Some("lamp"),
                    Capabilities::Switch(SwitchCapabilities::default()),
                ),
                &stamp(1),
            )
            .expect_err("kind change");
        assert!(
            err.0
                .contains("is already a light; it can't become a switch"),
            "{err}"
        );
    }

    #[test]
    fn the_core_checks_what_the_types_cant() {
        let mut home = Home::default();
        let err = home
            .describe_entity(
                &integration(),
                &[EntityKind::Switch],
                entity("x", Some("X"), None, dimmable()),
                &stamp(0),
            )
            .expect_err("kind not in manifest");
        assert!(
            err.0
                .contains("the manifest's entity_kinds doesn't list light"),
            "{err}"
        );

        let err = home
            .describe_entity(
                &integration(),
                &ALL,
                entity("x", None, Some("missing"), dimmable()),
                &stamp(0),
            )
            .expect_err("no device");
        assert!(err.0.contains("describe the device first"), "{err}");

        home.describe_device(&integration(), device("hub", "Hub"))
            .expect("hub");
        let mut child = device("child", "Child");
        child.via_device_unique_id = Some(uid("hub"));
        home.describe_device(&integration(), child).expect("child");
        let mut hub = device("hub", "Hub");
        hub.via_device_unique_id = Some(uid("child"));
        let err = home.describe_device(&integration(), hub).expect_err("loop");
        assert!(err.0.contains("would make a loop"), "{err}");

        // Another integration can't reach this one's devices.
        let other = IntegrationId::try_from("other").expect("valid");
        let err = home
            .describe_entity(
                &other,
                &ALL,
                entity("y", None, Some("hub"), dimmable()),
                &stamp(0),
            )
            .expect_err("foreign device");
        assert!(
            err.0.contains("isn't a device this integration described"),
            "{err}"
        );
    }

    #[test]
    fn reports_update_timestamps_and_context() {
        let mut home = home_with_lamp();
        let events = home
            .report_state(
                &integration(),
                report("lamp-light", Some(light(true, Some(128)))),
                &stamp(10),
            )
            .expect("fits");
        assert_eq!(events.len(), 1);
        let state = home.state(&lamp_id()).expect("state").clone();
        assert_eq!(state.last_changed, stamp(10).now);
        assert!(matches!(state.context.origin, Origin::Device { .. }));

        // The same value again: only last_reported moves, and nothing is published.
        let events = home
            .report_state(
                &integration(),
                report("lamp-light", Some(light(true, Some(128)))),
                &stamp(20),
            )
            .expect("fits");
        assert!(events.is_empty());
        let again = home.state(&lamp_id()).expect("state");
        assert_eq!(again.last_changed, stamp(10).now);
        assert_eq!(again.last_reported, stamp(20).now);

        // A clock that steps back never breaks the timestamp order.
        home.report_state(
            &integration(),
            report("lamp-light", Some(light(false, Some(128)))),
            &stamp(5),
        )
        .expect("fits");
        home.state(&lamp_id())
            .expect("state")
            .validate()
            .expect("timestamps in order");
    }

    #[test]
    fn reports_must_fit_the_entity() {
        let mut home = home_with_lamp();
        let err = home
            .report_state(
                &integration(),
                report("lamp-light", Some(State::Switch(SwitchState { on: true }))),
                &stamp(1),
            )
            .expect_err("wrong kind");
        assert!(
            err.0
                .contains("it's a light, but the report is for a switch"),
            "{err}"
        );

        let mut rgb = light(true, None);
        if let State::Light(l) = &mut rgb {
            l.rgb = Some([255, 0, 0]);
        }
        let err = home
            .report_state(&integration(), report("lamp-light", Some(rgb)), &stamp(1))
            .expect_err("no rgb");
        assert!(err.0.contains("doesn't support RGB color"), "{err}");

        home.describe_entity(
            &integration(),
            &ALL,
            entity(
                "temp",
                Some("Temperature"),
                None,
                Capabilities::Sensor(SensorCapabilities {
                    value_type: SensorValueType::Number,
                    device_class: None,
                    unit: None,
                    state_class: None,
                }),
            ),
            &stamp(0),
        )
        .expect("sensor");
        let err = home
            .report_state(
                &integration(),
                report(
                    "temp",
                    Some(State::Sensor(SensorState {
                        value: SensorValue::Text("warm".into()),
                    })),
                ),
                &stamp(1),
            )
            .expect_err("text for a number sensor");
        assert!(
            err.0
                .contains("it reports numbers, but the value is text \"warm\""),
            "{err}"
        );

        let err = home
            .report_state(&integration(), report("nope", None), &stamp(1))
            .expect_err("unknown");
        assert!(err.0.contains("describe it first"), "{err}");
    }

    #[test]
    fn color_mode_must_be_supported() {
        let mut home = home_with_lamp();
        let mut rgb_mode = light(true, None);
        if let State::Light(l) = &mut rgb_mode {
            l.color_mode = Some(ColorMode::Rgb);
        }
        let err = home
            .report_state(
                &integration(),
                report("lamp-light", Some(rgb_mode)),
                &stamp(1),
            )
            .expect_err("no rgb");
        assert!(
            err.0
                .contains("it's in rgb mode, but doesn't support RGB color"),
            "{err}"
        );
    }

    #[test]
    fn narrowed_capabilities_forget_a_value_that_no_longer_fits() {
        let mut home = home_with_lamp();
        home.report_state(
            &integration(),
            report("lamp-light", Some(light(true, Some(80)))),
            &stamp(1),
        )
        .expect("fits");
        let not_dimmable = Capabilities::Light(LightCapabilities::default());
        home.describe_entity(
            &integration(),
            &ALL,
            entity("lamp-light", None, Some("lamp"), not_dimmable.clone()),
            &stamp(2),
        )
        .expect("re-described");
        assert_eq!(home.state(&lamp_id()).expect("state").state, None);

        // A value that still fits is kept.
        home.report_state(
            &integration(),
            report("lamp-light", Some(light(true, None))),
            &stamp(3),
        )
        .expect("fits");
        home.describe_entity(
            &integration(),
            &ALL,
            entity("lamp-light", None, Some("lamp"), not_dimmable),
            &stamp(4),
        )
        .expect("re-described");
        assert_eq!(
            home.state(&lamp_id()).expect("state").state,
            Some(light(true, None))
        );
    }

    #[test]
    fn caused_by_must_be_a_call_to_this_integration() {
        let mut home = home_with_lamp();
        let call = stamp(1).context_id;
        let mut confirmed = report("lamp-light", Some(light(false, None)));
        confirmed.caused_by = Some(call.clone());
        let err = home
            .report_state(&integration(), confirmed.clone(), &stamp(2))
            .expect_err("unknown call");
        assert!(err.0.contains("isn't a recent service call"), "{err}");

        home.record_call(&integration(), call.clone());
        home.report_state(&integration(), confirmed, &stamp(2))
            .expect("known call");
        assert_eq!(
            home.state(&lamp_id()).expect("state").context.parent_id,
            Some(call)
        );
    }

    #[test]
    fn unavailable_keeps_the_last_value() {
        let mut home = home_with_lamp();
        home.report_state(
            &integration(),
            report("lamp-light", Some(light(true, Some(50)))),
            &stamp(1),
        )
        .expect("fits");
        let events = home.mark_unavailable(&integration(), &stamp(2));
        assert_eq!(events.len(), 1);
        let state = home.state(&lamp_id()).expect("state");
        assert_eq!(state.availability, Availability::Unavailable);
        assert_eq!(state.state, Some(light(true, Some(50))));
        assert!(matches!(state.context.origin, Origin::System));

        home.set_availability(
            &integration(),
            AvailabilityTarget::Device(uid("lamp")),
            Availability::Available,
            &stamp(3),
        )
        .expect("device exists");
        assert_eq!(
            home.state(&lamp_id()).expect("state").availability,
            Availability::Available
        );
    }

    #[test]
    fn names_follow_the_integration_and_nameless_entities_follow_their_device() {
        let mut home = home_with_lamp();
        let events = home
            .describe_device(&integration(), device("lamp", "Reading lamp"))
            .expect("renamed");
        assert_eq!(events.len(), 2, "device and its nameless entity updated");
        let lamp = home.entities.get(&lamp_id()).expect("same id");
        assert_eq!(lamp.name.as_str(), "Reading lamp");

        home.describe_entity(
            &integration(),
            &ALL,
            entity("lamp-light", Some("Bulb"), Some("lamp"), dimmable()),
            &stamp(1),
        )
        .expect("named now");
        home.describe_device(&integration(), device("lamp", "Desk lamp"))
            .expect("renamed");
        assert_eq!(home.entities[&lamp_id()].name.as_str(), "Bulb");
    }

    #[test]
    fn repeated_availability_reports_count_as_reports_but_crashes_dont() {
        let mut home = home_with_lamp();
        let events = home
            .set_availability(
                &integration(),
                AvailabilityTarget::Device(uid("lamp")),
                Availability::Available,
                &stamp(5),
            )
            .expect("device exists");
        assert!(events.is_empty());
        assert_eq!(
            home.state(&lamp_id()).expect("state").last_reported,
            stamp(5).now
        );

        // A crash changes availability, but nothing was heard: last_reported stays.
        home.mark_unavailable(&integration(), &stamp(6));
        let state = home.state(&lamp_id()).expect("state");
        assert_eq!(state.availability, Availability::Unavailable);
        assert_eq!(state.last_changed, stamp(6).now);
        assert_eq!(state.last_reported, stamp(5).now);
        state
            .validate()
            .expect("valid with last_reported before last_changed");

        // The integration saying so itself is a report.
        home.set_availability(
            &integration(),
            AvailabilityTarget::Device(uid("lamp")),
            Availability::Available,
            &stamp(8),
        )
        .expect("device exists");
        assert_eq!(
            home.state(&lamp_id()).expect("state").last_reported,
            stamp(8).now
        );
    }

    #[test]
    fn forgotten_calls_cant_be_blamed() {
        let mut home = home_with_lamp();
        let call = stamp(1).context_id;
        home.record_call(&integration(), call.clone());
        home.forget_call(&integration(), &call);
        let mut confirmed = report("lamp-light", Some(light(false, None)));
        confirmed.caused_by = Some(call);
        assert!(
            home.report_state(&integration(), confirmed, &stamp(2))
                .is_err()
        );
    }

    #[test]
    fn removing_a_device_removes_its_entities() {
        let mut home = home_with_lamp();
        let events = home
            .remove_device(&integration(), &uid("lamp"))
            .expect("exists");
        assert_eq!(events.len(), 2);
        assert_eq!(home.entities().count(), 0);
        assert_eq!(home.states().count(), 0);
        assert!(home.remove_device(&integration(), &uid("lamp")).is_err());
    }

    #[test]
    fn service_calls_are_checked_and_toggles_resolved() {
        let mut home = home_with_lamp();
        let resolved = home.resolve(&lamp_id(), Command::Toggle).expect("light");
        assert_eq!(
            resolved.service,
            Service::LightTurnOn(LightTurnOn::default())
        );
        assert_eq!(resolved.unique_id, uid("lamp-light"));

        home.report_state(
            &integration(),
            report("lamp-light", Some(light(true, None))),
            &stamp(1),
        )
        .expect("fits");
        assert_eq!(
            home.resolve(&lamp_id(), Command::Toggle)
                .expect("light")
                .service,
            Service::LightTurnOff
        );

        let too_warm = LightTurnOn {
            color_temp_kelvin: Some(2000),
            ..LightTurnOn::default()
        };
        let err = home
            .resolve(&lamp_id(), Command::TurnOn(too_warm))
            .expect_err("range");
        assert_eq!(
            err.to_string(),
            "`light.desk_lamp` supports color temperatures from 2700 to 6500 K, not 2000 K"
        );

        let unknown = EntityId::try_from("light.nope").expect("valid");
        assert!(matches!(
            home.resolve(&unknown, Command::TurnOff),
            Err(CallError::UnknownEntity(_))
        ));
    }
}
