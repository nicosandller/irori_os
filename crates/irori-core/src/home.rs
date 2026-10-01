//! The home: the registry (what exists) and live state (what it's doing), changed one operation at
//! a time. Pure and synchronous, so every rule of the contract is easy to test. The spec's "what
//! the core checks" (`docs/specs/protocols.md` §8) lives here.

use std::collections::{BTreeMap, HashMap, VecDeque};

use irori_protocol::{AvailabilityTarget, Rejected};
use irori_types::{
    Area, AreaId, Availability, Context, ContextId, Device, DeviceDescription, DeviceId, Entity,
    EntityDescription, EntityId, EntityKind, EntityState, Name, Origin, Placement, ProtocolId,
    SLUG_MAX_LEN, Service, ServiceName, Settings, SettingsKey, State, StateReport, Timestamp,
    Typed, UniqueId,
};

use crate::Event;
use crate::services::{CallError, Command};

/// When an operation happens, and the id for the context it creates if it changes something.
#[derive(Debug, Clone)]
pub(crate) struct Stamp {
    pub now: Timestamp,
    pub context_id: ContextId,
}

type Key = (ProtocolId, UniqueId);

/// How long a delivered service call can be named in `caused_by`: the window for a device to
/// confirm a command, even when its protocol is busy.
const CALL_WINDOW: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// A memory bound per protocol: past this many calls within the window, the oldest are
/// forgotten. Far more than any home sends to one protocol in five minutes.
const MAX_RECENT_CALLS: usize = 1024;

/// The protocol Irori's own device comes from (`irori_hub`: its version, uptime, load).
pub const SYSTEM_PROTOCOL: &str = "irori";

#[derive(Debug, Default)]
pub(crate) struct Home {
    devices: BTreeMap<DeviceId, Device>,
    device_keys: HashMap<Key, DeviceId>,
    entities: BTreeMap<EntityId, Entity>,
    entity_keys: HashMap<Key, EntityId>,
    states: BTreeMap<EntityId, EntityState>,
    /// What each protocol calls its devices, kept even while a person's name is being shown,
    /// so removing that name restores the original rather than freezing it.
    reported_device_names: HashMap<DeviceId, Name>,
    /// The same for entities. An entity that is *absent* here was described without a name, and
    /// uses (and follows) its device's name.
    reported_entity_names: HashMap<EntityId, Name>,
    /// What a person has said about all this (`docs/specs/config.md`). Overrides what the
    /// protocols report.
    settings: Settings,
    /// What each protocol last said about its devices and entities, as it said it. Kept so
    /// a device can be taken out of the home and put back without asking the protocol again.
    device_descriptions: HashMap<DeviceId, DeviceDescription>,
    entity_descriptions: HashMap<EntityId, (Vec<EntityKind>, EntityDescription)>,
    /// Devices a protocol has found that aren't in the home: out of the registry, but what their
    /// protocol says about them is kept, so adding one shows it as it is now.
    found: BTreeMap<DeviceId, Found>,
    /// Which found device each of their entities belongs to, to recognise what arrives for them.
    found_entities: HashMap<Key, DeviceId>,
    /// What the core last told an entity to be, until the device reports back. `toggle` uses it,
    /// so two toggles in a row don't both see the old value while the first is still in flight.
    commanded: HashMap<EntityId, Typed>,
    recent_calls: HashMap<ProtocolId, VecDeque<(ContextId, Timestamp)>>,
}

/// A device a protocol has found that isn't in the home (`docs/specs/config.md` §3.2): nobody has
/// added it yet, or somebody removed it.
///
/// Everything its protocol says about it lands here instead of in the registry: nothing is
/// listed, nothing can be switched, nothing is recorded. The latest of it is kept, so adding the
/// device puts it in as it is now, not as it was.
#[derive(Debug, Clone)]
struct Found {
    protocol: ProtocolId,
    description: DeviceDescription,
    entities: BTreeMap<UniqueId, (Vec<EntityKind>, EntityDescription)>,
    reports: BTreeMap<UniqueId, StateReport>,
    /// What the protocol last said about whether the device can be reached.
    available: bool,
}

/// A device found but not in the home, as "+ Add device" lists it: enough to recognise it and
/// decide whether it belongs.
///
/// Only what the device *is*, never what it's reporting: the page redraws this list when it
/// changes, and a list that changed with every reading would never hold still long enough to
/// pick from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HeldDevice {
    pub id: DeviceId,
    pub protocol: ProtocolId,
    pub name: Name,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// How many entities of each kind it would bring — `{"light": 1, "sensor": 2}`.
    pub provides: BTreeMap<String, usize>,
}

/// A service call resolved to the protocol that handles it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Resolved {
    pub protocol: ProtocolId,
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

    fn device_id(&self, protocol: &ProtocolId, unique_id: &UniqueId) -> Option<&DeviceId> {
        self.device_keys.get(&(protocol.clone(), unique_id.clone()))
    }

    fn entity_id(&self, protocol: &ProtocolId, unique_id: &UniqueId) -> Option<&EntityId> {
        self.entity_keys.get(&(protocol.clone(), unique_id.clone()))
    }

    // --- Settings -------------------------------------------------------------------------

    pub fn areas(&self) -> &[Area] {
        &self.settings.areas
    }

    pub fn floors(&self) -> &[irori_types::Floor] {
        &self.settings.floors
    }

    pub fn floorplan(&self) -> &irori_types::Floorplan {
        &self.settings.floorplan
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Whether a device is kept out of the home: while Irori asks before adding — always, outside
    /// tests — anything a person hasn't said a word about. Any `devices.toml` entry is a word
    /// about it: a name or a room is as much a decision as `added = true`, and removing a device
    /// is what takes its entry away.
    /// Whether a device waits for a person to add it. Irori's own device never does: nobody
    /// has to be asked whether Irori may be in the home.
    fn is_held(&self, protocol: &ProtocolId, id: &DeviceId) -> bool {
        protocol.as_str() != SYSTEM_PROTOCOL
            && self.settings.ask_before_adding
            && !self.settings.devices.contains_key(id)
    }

    /// Every device found but not in the home, in id order.
    pub fn held_devices(&self) -> Vec<HeldDevice> {
        self.found
            .iter()
            .map(|(id, found)| {
                let mut provides = BTreeMap::new();
                for (_, description) in found.entities.values() {
                    *provides.entry(description.kind().to_string()).or_insert(0) += 1;
                }
                HeldDevice {
                    id: id.clone(),
                    protocol: found.protocol.clone(),
                    name: self.name_for_device(id, &found.description.name),
                    manufacturer: found.description.manufacturer.clone(),
                    model: found.description.model.clone(),
                    provides,
                }
            })
            .collect()
    }

    /// Takes a device out of the home, keeping what its protocol said so it can be added again.
    fn hold(&mut self, id: &DeviceId) -> Vec<Event> {
        let (Some(device), Some(description)) = (
            self.devices.get(id).cloned(),
            self.device_descriptions.get(id).cloned(),
        ) else {
            return vec![];
        };
        let mut found = Found {
            protocol: device.protocol.clone(),
            description,
            entities: BTreeMap::new(),
            reports: BTreeMap::new(),
            available: true,
        };
        let owned: Vec<Entity> = self
            .entities
            .values()
            .filter(|entity| entity.device_id.as_ref() == Some(id))
            .cloned()
            .collect();
        for entity in &owned {
            if let Some(described) = self.entity_descriptions.get(&entity.id) {
                found
                    .entities
                    .insert(entity.unique_id.clone(), described.clone());
            }
            if let Some(state) = self.states.get(&entity.id) {
                if state.availability == Availability::Unavailable {
                    found.available = false;
                }
                found.reports.insert(
                    entity.unique_id.clone(),
                    StateReport {
                        unique_id: entity.unique_id.clone(),
                        state: state.state.clone(),
                        attributes: state.attributes.clone(),
                        caused_by: None,
                    },
                );
            }
        }
        // Removed first, then recorded as found: the other way round, removal would take the
        // entities for already-found ones and leave them in the registry without their device.
        let events = self
            .remove_device(&device.protocol, &device.unique_id)
            .unwrap_or_default();
        for entity in &owned {
            self.found_entities.insert(
                (device.protocol.clone(), entity.unique_id.clone()),
                id.clone(),
            );
        }
        self.found.insert(id.clone(), found);
        events
    }

    /// Puts a found device in the home, as its protocol last described it.
    fn admit(&mut self, id: &DeviceId, stamp: &Stamp) -> Vec<Event> {
        let Some(found) = self.found.remove(id) else {
            return vec![];
        };
        let protocol = found.protocol;
        for unique_id in found.entities.keys() {
            self.found_entities
                .remove(&(protocol.clone(), unique_id.clone()));
        }
        let mut events = Vec::new();
        // Each step is what the protocol said and the core accepted before; if one is
        // refused now (the registry changed meanwhile), the rest still go in.
        events.extend(
            self.describe_device(&protocol, found.description.clone())
                .unwrap_or_default(),
        );
        for (kinds, description) in found.entities.into_values() {
            events.extend(
                self.describe_entity(&protocol, &kinds, description, stamp)
                    .unwrap_or_default(),
            );
        }
        for report in found.reports.into_values() {
            events.extend(
                self.report_state(&protocol, report, stamp)
                    .unwrap_or_default(),
            );
        }
        if !found.available {
            events.extend(
                self.set_availability(
                    &protocol,
                    AvailabilityTarget::Device(found.description.unique_id),
                    Availability::Unavailable,
                    stamp,
                )
                .unwrap_or_default(),
            );
        }
        events
    }

    pub fn entity_key(&self, id: &EntityId) -> Option<SettingsKey> {
        let entity = self.entities.get(id)?;
        Some(SettingsKey::new(
            entity.protocol.clone(),
            entity.unique_id.clone(),
        ))
    }

    /// What a device is called: the name a person gave it, else the one its protocol reports
    /// (`docs/specs/config.md` §5). One name, never two side by side (ROADMAP D36).
    fn name_for_device(&self, id: &DeviceId, reported: &Name) -> Name {
        self.settings
            .devices
            .get(id)
            .and_then(|settings| settings.name.clone())
            .unwrap_or_else(|| reported.clone())
    }

    fn description_for_device(&self, id: &DeviceId) -> Option<irori_types::Description> {
        self.settings.devices.get(id)?.description.clone()
    }

    /// Which room a device is in: the one a person put it in, else one whose name matches what
    /// the device suggests for itself.
    ///
    /// A choice that points at an area which no longer exists leaves the device unplaced rather
    /// than falling back to the suggestion — the choice is still in the file, and deleting a room
    /// shouldn't quietly hand the device back to its firmware. Same for a deliberate "no room":
    /// that's an answer, and the suggestion doesn't get to overrule it.
    fn area_for_device(&self, id: &DeviceId, suggested: Option<&Name>) -> Option<AreaId> {
        let chosen = self
            .settings
            .devices
            .get(id)
            .map(|settings| settings.area.clone())
            .unwrap_or_default();
        placed(&self.settings, &chosen, suggested)
    }

    /// What an entity is called: the name a person gave it, else the one its protocol
    /// reports, else its device's name — which it goes on following.
    fn name_for_entity(&self, id: &EntityId, device_id: Option<&DeviceId>) -> Name {
        if let Some(chosen) = self
            .entity_key(id)
            .and_then(|key| self.settings.entities.get(&key)?.name.clone())
        {
            return chosen;
        }
        match self.reported_entity_names.get(id) {
            Some(reported) => reported.clone(),
            None => device_id
                .and_then(|device| self.devices.get(device))
                .map(|device| device.name.clone())
                .expect("a nameless entity has a device (EntityDescription::validate)"),
        }
    }

    /// Takes what a person has said about the home and brings the registry in line with it.
    ///
    /// Devices first: an entity with no name of its own follows its device's, so it has to see
    /// the device's new name rather than the old one.
    pub fn apply_settings(&mut self, settings: Settings, stamp: &Stamp) -> Vec<Event> {
        if self.settings == settings {
            return vec![];
        }
        self.settings = settings;
        let mut events = Vec::new();

        // In and out of the home first, so the renames below only touch what's in it.
        let now_held: Vec<DeviceId> = self
            .devices
            .iter()
            .filter(|(id, device)| self.is_held(&device.protocol, id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in now_held {
            events.extend(self.hold(&id));
        }
        let now_added: Vec<DeviceId> = self
            .found
            .iter()
            .filter(|(id, found)| !self.is_held(&found.protocol, id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in now_added {
            events.extend(self.admit(&id, stamp));
        }

        for id in self.devices.keys().cloned().collect::<Vec<_>>() {
            let reported = self.reported_device_names[&id].clone();
            let name = self.name_for_device(&id, &reported);
            let description = self.description_for_device(&id);
            let suggested = self.devices[&id].suggested_area.clone();
            let area_id = self.area_for_device(&id, suggested.as_ref());
            let device = self.devices.get_mut(&id).expect("just read");
            if device.name == name && device.area_id == area_id && device.description == description
            {
                continue;
            }
            device.name = name;
            device.description = description;
            device.area_id = area_id;
            events.push(Event::DeviceUpdated {
                device: device.clone(),
            });
        }

        for id in self.entities.keys().cloned().collect::<Vec<_>>() {
            let device_id = self.entities[&id].device_id.clone();
            let name = self.name_for_entity(&id, device_id.as_ref());
            let entity = self.entities.get_mut(&id).expect("just read");
            if entity.name == name {
                continue;
            }
            entity.name = name;
            events.push(Event::EntityUpdated {
                entity: entity.clone(),
            });
        }
        events
    }

    // --- Registry -------------------------------------------------------------------------

    pub fn describe_device(
        &mut self,
        protocol: &ProtocolId,
        description: DeviceDescription,
    ) -> Result<Vec<Event>, Rejected> {
        description
            .validate()
            .map_err(|e| Rejected(e.to_string()))?;
        let unique_id = &description.unique_id;
        let held_id = device_id_for(protocol, unique_id);
        if self.is_held(protocol, &held_id) || self.found.contains_key(&held_id) {
            match self.found.get_mut(&held_id) {
                Some(found) => found.description = description,
                None => {
                    self.found.insert(
                        held_id,
                        Found {
                            protocol: protocol.clone(),
                            description,
                            entities: BTreeMap::new(),
                            reports: BTreeMap::new(),
                            available: true,
                        },
                    );
                }
            }
            return Ok(vec![]);
        }
        // Reached through a device that isn't in the home: as far as the home knows, it's reached
        // directly.
        let via_unique_id = description
            .via_device_unique_id
            .clone()
            .filter(|via| !self.found.contains_key(&device_id_for(protocol, via)));
        let via = match &via_unique_id {
            Some(via) => Some(self.device_id(protocol, via).cloned().ok_or_else(|| {
                Rejected(format!(
                    "device `{unique_id}`: via_device_unique_id `{via}` isn't a device this protocol described"
                ))
            })?),
            None => None,
        };

        if let Some(id) = self.device_id(protocol, unique_id).cloned() {
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
            self.reported_device_names
                .insert(id.clone(), description.name.clone());
            self.device_descriptions
                .insert(id.clone(), description.clone());
            let name = self.name_for_device(&id, &description.name);
            let area_id = self.area_for_device(&id, description.suggested_area.as_ref());
            let device = self.devices.get_mut(&id).expect("keys and devices agree");
            let before = device.clone();
            device.name = name;
            device.manufacturer = description.manufacturer;
            device.model = description.model;
            device.sw_version = description.sw_version;
            device.hw_version = description.hw_version;
            device.suggested_area = description.suggested_area;
            device.area_id = area_id;
            device.via_device_id = via;
            if *device == before {
                return Ok(vec![]);
            }
            let device = device.clone();
            let mut events = Vec::new();
            if device.name != before.name {
                let following: Vec<EntityId> = self
                    .entities
                    .values()
                    .filter(|entity| entity.device_id.as_ref() == Some(&id))
                    .map(|entity| entity.id.clone())
                    .collect();
                for entity_id in following {
                    // Not every entity of a renamed device follows it: one with a name of its
                    // own, from the protocol or from a person, keeps it.
                    let name = self.name_for_entity(&entity_id, Some(&id));
                    let entity = self.entities.get_mut(&entity_id).expect("just listed");
                    if entity.name == name {
                        continue;
                    }
                    entity.name = name;
                    events.push(Event::EntityUpdated {
                        entity: entity.clone(),
                    });
                }
            }
            events.insert(0, Event::DeviceUpdated { device });
            return Ok(events);
        }

        // The one id, from what never changes: never from a name, so a rename can't move it and
        // it's the same after every restart (ROADMAP D36).
        let id = device_id_for(protocol, &description.unique_id);
        if let Some(holder) = self.devices.get(&id) {
            // Two handles that differ only in case or punctuation. Vanishingly rare for real
            // hardware addresses, and refusing says so plainly where renumbering one of them
            // would make its id depend on which device happened to be found first.
            return Err(Rejected(format!(
                "device `{unique_id}` would have the id `{id}`, which `{}` already has; its \
                 unique_id has to differ by more than case or punctuation",
                holder.unique_id
            )));
        }
        self.device_descriptions
            .insert(id.clone(), description.clone());
        let settings = self.settings.devices.get(&id);
        let name = settings
            .and_then(|settings| settings.name.clone())
            .unwrap_or_else(|| description.name.clone());
        let area_id = placed(
            &self.settings,
            &settings
                .map(|settings| settings.area.clone())
                .unwrap_or_default(),
            description.suggested_area.as_ref(),
        );
        let device = Device {
            id: id.clone(),
            protocol: protocol.clone(),
            unique_id: description.unique_id.clone(),
            name,
            description: settings.and_then(|settings| settings.description.clone()),
            manufacturer: description.manufacturer,
            model: description.model,
            sw_version: description.sw_version,
            hw_version: description.hw_version,
            area_id,
            suggested_area: description.suggested_area,
            via_device_id: via,
        };
        self.reported_device_names
            .insert(id.clone(), description.name);
        self.device_keys
            .insert((protocol.clone(), description.unique_id), id.clone());
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
        protocol: &ProtocolId,
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
        if let Some(device) = &description.device_unique_id {
            let device_id = device_id_for(protocol, device);
            if let Some(found) = self.found.get_mut(&device_id) {
                self.found_entities
                    .insert((protocol.clone(), unique_id.clone()), device_id);
                found
                    .entities
                    .insert(unique_id.clone(), (kinds.to_vec(), description));
                return Ok(vec![]);
            }
        }
        let described = (kinds.to_vec(), description.clone());
        let device = match &description.device_unique_id {
            Some(device) => {
                let id = self.device_id(protocol, device).ok_or_else(|| {
                    Rejected(format!(
                        "entity `{unique_id}`: device_unique_id `{device}` isn't a device this protocol described (describe the device first)"
                    ))
                })?;
                Some(&self.devices[id])
            }
            None => None,
        };
        let device_id = device.map(|d| d.id.clone());

        if let Some(id) = self.entity_id(protocol, unique_id).cloned() {
            let entity = self.entities.get_mut(&id).expect("keys and entities agree");
            if entity.id.kind() != kind {
                return Err(Rejected(format!(
                    "entity `{unique_id}` is already a {}; it can't become a {kind} (use a different unique_id)",
                    entity.id.kind()
                )));
            }
            let before = entity.clone();
            match description.name {
                Some(name) => {
                    self.reported_entity_names.insert(id.clone(), name);
                }
                // Nameless: it follows its device's name, whatever that currently is.
                None => {
                    self.reported_entity_names.remove(&id);
                }
            }
            self.entity_descriptions.insert(id.clone(), described);
            let name = self.name_for_entity(&id, device_id.as_ref());
            let entity = self.entities.get_mut(&id).expect("keys and entities agree");
            entity.name = name;
            entity.capabilities = description.capabilities;
            entity.entity_category = description.entity_category;
            entity.device_id = device_id;
            let mut events = if *entity == before {
                vec![]
            } else {
                vec![Event::EntityUpdated {
                    entity: entity.clone(),
                }]
            };
            // If its abilities shrank (e.g. a firmware update removed RGB), a stored value that
            // no longer fits is forgotten: unknown until the protocol reports again.
            let entity = self.entities[&id].clone();
            let old = self.states[&id].clone();
            if old
                .state
                .as_ref()
                .is_some_and(|state| fits(&entity, state).is_err())
            {
                // Irori's own change, not a report: `last_reported` stays, and the context says
                // the core did it (like marking entities unavailable after a crash).
                let now = stamp.now.max(old.last_updated);
                // A command in flight was for the abilities it no longer has.
                self.commanded.remove(&id);
                let mut new = old.clone();
                new.state = None;
                new.last_changed = now;
                new.last_updated = now;
                new.context = system_context(stamp);
                self.states.insert(id.clone(), new.clone());
                events.push(Event::StateChanged {
                    entity_id: id.clone(),
                    old_state: Some(Box::new(old)),
                    new_state: Box::new(new),
                });
            }
            // Being described means the protocol is back in touch with it (e.g. after a
            // restart). If the device is still offline, the protocol says so next.
            let context = device_context(protocol, stamp, None);
            // Describing an entity is the protocol telling Irori about it, so it counts as
            // hearing from it (`docs/specs/entities.md` §5.1).
            events.extend(self.set_availability_of(
                vec![id],
                Availability::Available,
                stamp.now,
                context,
                Reported::Yes,
            ));
            return Ok(events);
        }

        // The entity doesn't exist yet, so its settings have to be looked up by hand rather than
        // through `name_for_entity`.
        let chosen = self
            .settings
            .entities
            .get(&SettingsKey::new(
                protocol.clone(),
                description.unique_id.clone(),
            ))
            .and_then(|settings| settings.name.clone());
        let otherwise: Name = match (&description.name, device) {
            (Some(name), _) => name.clone(),
            (None, Some(device)) => device.name.clone(),
            (None, None) => {
                unreachable!("EntityDescription::validate requires a name or a device")
            }
        };
        // The id is built from the device's id and the name the protocol gave the entity —
        // never from a name a person chose, and never from the device's name, so renaming either
        // can't leave an id that says something else (ROADMAP D36).
        let base = match (&description.suggested_object_id, &description.name, device) {
            (Some(object_id), _, _) => object_id.as_str().to_owned(),
            (None, Some(reported), Some(device)) => {
                slugify(&format!("{} {}", device.id.as_str(), reported.as_str()))
            }
            (None, None, Some(device)) => device.id.as_str().to_owned(),
            (None, _, None) => slugify(otherwise.as_str()),
        };
        let name: Name = chosen.unwrap_or(otherwise);
        let object_id = unique_id_for(&base, kind.domain(), |c| {
            EntityId::new(kind, c).is_ok_and(|id| self.entities.contains_key(&id))
        });
        let id = EntityId::new(kind, &object_id).expect("unique_id_for returns a slug");
        let entity = Entity {
            id: id.clone(),
            protocol: protocol.clone(),
            unique_id: description.unique_id.clone(),
            name,
            device_id,
            area_id: None,
            capabilities: description.capabilities,
            entity_category: description.entity_category,
        };
        let state = EntityState {
            entity_id: id.clone(),
            availability: Availability::Available,
            state: None,
            attributes: BTreeMap::new(),
            last_changed: stamp.now,
            last_updated: stamp.now,
            last_reported: stamp.now,
            // Irori creates this "nothing reported yet" entry; the device hasn't said anything.
            context: system_context(stamp),
        };
        if let Some(reported) = description.name {
            self.reported_entity_names.insert(id.clone(), reported);
        }
        self.entity_descriptions.insert(id.clone(), described);
        self.entity_keys
            .insert((protocol.clone(), description.unique_id), id.clone());
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
        protocol: &ProtocolId,
        unique_id: &UniqueId,
    ) -> Result<Vec<Event>, Rejected> {
        let key = (protocol.clone(), unique_id.clone());
        if let Some(device) = self.found_entities.remove(&key) {
            if let Some(found) = self.found.get_mut(&device) {
                found.entities.remove(unique_id);
                found.reports.remove(unique_id);
            }
            return Ok(vec![]);
        }
        let id = self.entity_keys.remove(&key).ok_or_else(|| {
            Rejected(format!(
                "can't remove entity `{unique_id}`: this protocol has no such entity"
            ))
        })?;
        self.entity_descriptions.remove(&id);
        self.entities.remove(&id);
        self.states.remove(&id);
        self.reported_entity_names.remove(&id);
        self.commanded.remove(&id);
        Ok(vec![Event::EntityRemoved { entity_id: id }])
    }

    pub fn remove_device(
        &mut self,
        protocol: &ProtocolId,
        unique_id: &UniqueId,
    ) -> Result<Vec<Event>, Rejected> {
        let key = (protocol.clone(), unique_id.clone());
        if let Some(found) = self.found.remove(&device_id_for(protocol, unique_id)) {
            for entity in found.entities.keys() {
                self.found_entities
                    .remove(&(protocol.clone(), entity.clone()));
            }
            return Ok(vec![]);
        }
        let id = self.device_keys.remove(&key).ok_or_else(|| {
            Rejected(format!(
                "can't remove device `{unique_id}`: this protocol has no such device"
            ))
        })?;
        self.device_descriptions.remove(&id);
        let mut events = Vec::new();
        let owned: Vec<Entity> = self
            .entities
            .values()
            .filter(|e| e.device_id.as_ref() == Some(&id))
            .cloned()
            .collect();
        for entity in owned {
            events.extend(self.remove_entity(&entity.protocol, &entity.unique_id)?);
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

    /// Removes every device this protocol owns, in the home or found.
    pub fn remove_protocol(&mut self, protocol: &ProtocolId) -> Vec<Event> {
        let live: Vec<UniqueId> = self
            .devices
            .values()
            .filter(|device| &device.protocol == protocol)
            .map(|device| device.unique_id.clone())
            .collect();
        let found: Vec<UniqueId> = self
            .found
            .values()
            .filter(|found| &found.protocol == protocol)
            .map(|found| found.description.unique_id.clone())
            .collect();
        let mut events = Vec::new();
        for unique_id in live.into_iter().chain(found) {
            if let Ok(ev) = self.remove_device(protocol, &unique_id) {
                events.extend(ev);
            }
        }
        events
    }

    /// Removes a device from the home (`docs/specs/config.md` §3.2): out of the registry —
    /// entities, states, names, what reaches the home through it — and back among what its
    /// protocol has found, as the protocol last described it. So it's listed under
    /// "+ Add device" straight away, without the device having to announce itself again, and
    /// adding it back is like adding it the first time. A device that's already only found stays
    /// found.
    pub fn forget_device(&mut self, id: &DeviceId) -> Result<Vec<Event>, Rejected> {
        if self.devices.contains_key(id) {
            return Ok(self.hold(id));
        }
        if self.found.contains_key(id) {
            return Ok(vec![]);
        }
        Err(Rejected(format!("there's no device `{id}`")))
    }

    /// The entities a device keeps — in the home, or remembered for a found one — as protocol
    /// and unique id pairs, the two fields an `entities.toml` key is made of.
    pub fn device_entity_keys(&self, id: &DeviceId) -> Option<Vec<(ProtocolId, UniqueId)>> {
        if let Some(device) = self.devices.get(id) {
            return Some(
                self.entities
                    .iter()
                    .filter(|(_, entity)| entity.device_id.as_ref() == Some(id))
                    .map(|(_, entity)| (device.protocol.clone(), entity.unique_id.clone()))
                    .collect(),
            );
        }
        self.found.get(id).map(|found| {
            found
                .entities
                .keys()
                .map(|unique_id| (found.protocol.clone(), unique_id.clone()))
                .collect()
        })
    }

    // --- State ----------------------------------------------------------------------------

    pub fn report_state(
        &mut self,
        protocol: &ProtocolId,
        report: StateReport,
        stamp: &Stamp,
    ) -> Result<Vec<Event>, Rejected> {
        report.validate().map_err(|e| Rejected(e.to_string()))?;
        let unique_id = &report.unique_id;
        if let Some(device) = self
            .found_entities
            .get(&(protocol.clone(), unique_id.clone()))
        {
            if let Some(found) = self.found.get_mut(device) {
                // Kept without its cause: by the time it's put back, no call is waiting on it.
                let mut report = report;
                report.caused_by = None;
                found.reports.insert(report.unique_id.clone(), report);
            }
            return Ok(vec![]);
        }
        let id = self.entity_id(protocol, unique_id).cloned().ok_or_else(|| {
            Rejected(format!(
                "state report for `{unique_id}`: this protocol has no such entity (describe it first)"
            ))
        })?;
        if let Some(state) = &report.state {
            fits(&self.entities[&id], state)?;
        }
        let parent = match &report.caused_by {
            Some(caused_by)
                if self.recent_calls.get(protocol).is_some_and(|calls| {
                    calls
                        .iter()
                        .any(|(id, at)| id == caused_by && within_call_window(*at, stamp.now))
                }) =>
            {
                Some(caused_by.clone())
            }
            Some(caused_by) => {
                return Err(Rejected(format!(
                    "state report for `{unique_id}`: caused_by `{caused_by}` isn't a service call sent to this protocol in the last 5 minutes"
                )));
            }
            None => None,
        };

        // The device has spoken, so what it was told to be doesn't matter any more.
        self.commanded.remove(&id);
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
            new.context = device_context(protocol, stamp, parent);
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
        protocol: &ProtocolId,
        target: AvailabilityTarget,
        availability: Availability,
        stamp: &Stamp,
    ) -> Result<Vec<Event>, Rejected> {
        let target = match target {
            AvailabilityTarget::Device(device) => {
                if let Some(found) = self.found.get_mut(&device_id_for(protocol, &device)) {
                    found.available = availability == Availability::Available;
                    return Ok(vec![]);
                }
                AvailabilityTarget::Device(device)
            }
            AvailabilityTarget::Entities(unique_ids) => {
                let kept: Vec<UniqueId> = unique_ids
                    .into_iter()
                    .filter(|u| {
                        !self
                            .found_entities
                            .contains_key(&(protocol.clone(), u.clone()))
                    })
                    .collect();
                if kept.is_empty() {
                    return Ok(vec![]);
                }
                AvailabilityTarget::Entities(kept)
            }
        };
        let ids: Vec<EntityId> = match &target {
            AvailabilityTarget::Device(device) => {
                let device_id = self.device_id(protocol, device).ok_or_else(|| {
                    Rejected(format!(
                        "can't set availability of device `{device}`: this protocol has no such device"
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
                    self.entity_id(protocol, u).cloned().ok_or_else(|| {
                        Rejected(format!(
                            "can't set availability of entity `{u}`: this protocol has no such entity"
                        ))
                    })
                })
                .collect::<Result<_, _>>()?,
        };
        let context = device_context(protocol, stamp, None);
        Ok(self.set_availability_of(ids, availability, stamp.now, context, Reported::Yes))
    }

    /// Marks every entity of a protocol unavailable, e.g. because it crashed or stopped.
    pub fn mark_unavailable(&mut self, protocol: &ProtocolId, stamp: &Stamp) -> Vec<Event> {
        let ids = self
            .entities
            .values()
            .filter(|e| &e.protocol == protocol)
            .map(|e| e.id.clone())
            .collect();
        let context = system_context(stamp);
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
                // Nothing changed, but hearing it again from the protocol is a report.
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

    /// Turns a request on an entity into the service call its protocol receives, checking
    /// what the entity supports. Resolves `toggle` from the current state.
    pub fn resolve(&self, entity_id: &EntityId, command: Command) -> Result<Resolved, CallError> {
        let entity = self
            .entities
            .get(entity_id)
            .ok_or_else(|| CallError::UnknownEntity(entity_id.clone()))?;
        let not_supported = |what: String| CallError::NotSupported(format!("`{entity_id}` {what}"));
        let kind = entity.id.kind();
        if !kind.has_services() {
            return Err(not_supported(format!("is a {kind}; it has no services")));
        }
        let name = if command.action == Command::TOGGLE {
            if !command.data.is_empty() {
                return Err(CallError::NotSupported(format!(
                    "`{kind}.toggle` takes no data"
                )));
            }
            let current = self.commanded.get(entity_id).cloned().or_else(|| {
                self.states
                    .get(entity_id)
                    .and_then(|s| s.state.as_ref())
                    .map(State::primary)
            });
            kind.toggle(current.as_ref())
                .ok_or_else(|| not_supported(format!("is a {kind}; it can't be toggled")))?
        } else {
            ServiceName::of(kind, &command.action).ok_or_else(|| {
                not_supported(format!("is a {kind}; it has no `{}`", command.action))
            })?
        };
        let service = Service::from_data(name, command.data)
            .map_err(|e| CallError::NotSupported(e.to_string()))?;
        entity
            .capabilities
            .supports(&service)
            .map_err(not_supported)?;
        Ok(Resolved {
            protocol: entity.protocol.clone(),
            unique_id: entity.unique_id.clone(),
            service,
        })
    }

    /// Forgets a call that never reached its protocol.
    pub fn forget_call(&mut self, protocol: &ProtocolId, context_id: &ContextId) {
        if let Some(calls) = self.recent_calls.get_mut(protocol) {
            calls.retain(|(id, _)| id != context_id);
        }
    }

    /// Whether a report may name this call in `caused_by`.
    #[cfg(test)]
    pub fn knows_call(&self, protocol: &ProtocolId, context_id: &ContextId) -> bool {
        self.recent_calls
            .get(protocol)
            .is_some_and(|calls| calls.iter().any(|(id, _)| id == context_id))
    }

    /// Forgets what an entity was told to be, e.g. because the call failed.
    pub fn forget_command(&mut self, entity_id: &EntityId) {
        self.commanded.remove(entity_id);
    }

    /// Whether a resolved call still matches the registry: the same entity, still owned by that
    /// protocol under that `unique_id`, and still able to do what's asked.
    pub fn still_dispatchable(&self, entity_id: &EntityId, resolved: &Resolved) -> bool {
        self.entities.get(entity_id).is_some_and(|entity| {
            entity.protocol == resolved.protocol
                && entity.unique_id == resolved.unique_id
                && entity.capabilities.supports(&resolved.service).is_ok()
        })
    }

    /// Remembers what an entity was just told to be, until it reports back.
    pub fn record_command(&mut self, entity_id: &EntityId, service: &Service) {
        match service.asks_for() {
            Some(value) => self.commanded.insert(entity_id.clone(), value),
            None => self.commanded.remove(entity_id),
        };
    }

    /// Remembers a call's context for [`CALL_WINDOW`], so a state report can say it was caused
    /// by it.
    pub fn record_call(&mut self, protocol: &ProtocolId, context_id: ContextId, now: Timestamp) {
        let calls = self.recent_calls.entry(protocol.clone()).or_default();
        while calls
            .front()
            .is_some_and(|(_, at)| !within_call_window(*at, now))
        {
            calls.pop_front();
        }
        if calls.len() == MAX_RECENT_CALLS {
            calls.pop_front();
        }
        calls.push_back((context_id, now));
    }
}

/// Where a device ends up: what a person said, else what the device suggests.
///
/// The three answers are different. A room that was chosen is used if it still exists; a
/// deliberate "no room" is honoured and shuts the suggestion out; and only silence lets the
/// device's own idea of where it is stand in (`docs/specs/config.md` §5).
fn placed(settings: &Settings, chosen: &Placement, suggested: Option<&Name>) -> Option<AreaId> {
    match chosen {
        Placement::In(area) => settings.area(area).map(|area| area.id.clone()),
        Placement::Nowhere => None,
        Placement::Unsaid => suggested
            .and_then(|name| settings.area_named(name))
            .map(|area| area.id.clone()),
    }
}

/// A change Irori made itself, e.g. after a protocol crashed.
fn system_context(stamp: &Stamp) -> Context {
    Context {
        id: stamp.context_id.clone(),
        parent_id: None,
        origin: Origin::System,
    }
}

fn within_call_window(called_at: Timestamp, now: Timestamp) -> bool {
    let age = now.as_jiff().duration_since(called_at.as_jiff());
    let window = jiff::SignedDuration::try_from(CALL_WINDOW).unwrap_or(jiff::SignedDuration::MAX);
    // A clock that steps back (an NTP correction) makes the age negative. Tolerate that by the
    // same window, so a confirmation isn't refused, but nothing older stays valid for ever.
    (-window..=window).contains(&age)
}

/// Whether a change comes from the protocol saying something (it moves `last_reported`
/// even when nothing changed), or from the core, e.g. after a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reported {
    Yes,
    No,
}

fn device_context(protocol: &ProtocolId, stamp: &Stamp, parent: Option<ContextId>) -> Context {
    Context {
        id: stamp.context_id.clone(),
        parent_id: parent,
        origin: Origin::Device {
            protocol: protocol.clone(),
        },
    }
}

/// Whether a reported state fits the entity's kind and capabilities.
fn fits(entity: &Entity, state: &State) -> Result<(), Rejected> {
    entity
        .capabilities
        .fits(state)
        .map_err(|what| Rejected(format!("state report for `{}`: {what}", entity.id)))
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

/// A device's id: its protocol and the protocol's permanent handle for it, as a slug —
/// `esphome_00_11_22_33_44_55`. A pure function of the two, so it's the same after every restart
/// and whatever the device is called (ROADMAP D36).
///
/// A handle too long for an id keeps its beginning and gains a hash of the whole canonical
/// input (protocol plus handle), so two long protocols that share a prefix — or two long
/// handles that start the same — still get different ids.
pub fn device_id_for(protocol: &ProtocolId, unique_id: &UniqueId) -> DeviceId {
    let canonical = format!("{protocol} {unique_id}");
    let mut slug = String::new();
    let mut separate = false;
    for c in canonical.chars() {
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
    // A handle with nothing a slug can keep (`玄関`) would leave just the protocol's name.
    let lossless_enough = slug.len() > protocol.as_str().len();
    if slug.len() > SLUG_MAX_LEN || !lossless_enough {
        let hash = format!("{:016x}", fnv1a(canonical.as_bytes()));
        let keep = SLUG_MAX_LEN - hash.len() - 1;
        slug.truncate(keep);
        let trimmed = slug.trim_end_matches('_');
        slug = format!("{trimmed}_{hash}");
    }
    DeviceId::try_from(slug).expect("built from slug characters, within the length")
}

/// FNV-1a: tiny, stable across builds and platforms, which is all an id suffix needs.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// An id for a new area, from what it's called and what's already there.
///
/// Areas are the one thing a person creates directly, so their ids are made here rather than by
/// a protocol — and by the same rules as every other id, so `Kitchen` becomes `kitchen` and a
/// second `Kitchen` becomes `kitchen_2` instead of an error.
pub fn new_area_id(name: &Name, existing: &[Area]) -> AreaId {
    let id = unique_id_for(&slugify(name.as_str()), "area", |candidate| {
        AreaId::try_from(candidate)
            .is_ok_and(|id| existing.iter().any(|existing| existing.id == id))
    });
    AreaId::try_from(id).expect("unique_id_for returns a slug")
}

/// An id for a new floor, by the same rules as a room's: from its name, never colliding.
pub fn new_floor_id(name: &Name, existing: &[irori_types::Floor]) -> irori_types::FloorId {
    let id = unique_id_for(&slugify(name.as_str()), "floor", |candidate| {
        irori_types::FloorId::try_from(candidate)
            .is_ok_and(|id| existing.iter().any(|existing| existing.id == id))
    });
    irori_types::FloorId::try_from(id).expect("unique_id_for returns a slug")
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
        BinarySensorCapabilities, Capabilities, ColorMode, ColorTempRange, LightCapabilities,
        LightState, LightTurnOn, SensorCapabilities, SensorState, SensorValue, SensorValueType,
        SwitchCapabilities, SwitchState,
    };

    fn protocol() -> ProtocolId {
        ProtocolId::try_from("demo").expect("valid")
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

    const ALL: &[EntityKind] = EntityKind::ALL;

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
            entity_category: None,
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
        home.describe_device(&protocol(), device("lamp", "Desk lamp"))
            .expect("device");
        home.describe_entity(
            &protocol(),
            ALL,
            entity("lamp-light", None, Some("lamp"), dimmable()),
            &stamp(0),
        )
        .expect("entity");
        home
    }

    impl Home {
        /// `apply_settings` at a fixed moment: these tests are about what settings do, not when.
        fn settle(&mut self, settings: Settings) -> Vec<Event> {
            self.apply_settings(settings, &stamp(0))
        }
    }

    fn lamp_id() -> EntityId {
        EntityId::try_from("light.demo_lamp").expect("valid")
    }

    fn area(id: &str, called: &str) -> Area {
        Area {
            id: AreaId::try_from(id).expect("valid"),
            name: name(called),
            floor_id: None,
        }
    }

    /// Device settings are by the device's id, the same id its page and the API use.
    fn key(unique: &str) -> DeviceId {
        device_id_for(&protocol(), &uid(unique))
    }

    fn entity_key(unique: &str) -> SettingsKey {
        SettingsKey::new(protocol(), uid(unique))
    }

    fn called(what: &str) -> irori_types::DeviceSettings {
        irori_types::DeviceSettings {
            added: false,
            name: Some(name(what)),
            description: None,
            area: Placement::Unsaid,
        }
    }

    fn in_room(area: &str) -> irori_types::DeviceSettings {
        irori_types::DeviceSettings {
            added: false,
            name: None,
            description: None,
            area: Placement::In(AreaId::try_from(area).expect("valid")),
        }
    }

    fn nowhere() -> irori_types::DeviceSettings {
        irori_types::DeviceSettings {
            added: false,
            name: None,
            description: None,
            area: Placement::Nowhere,
        }
    }

    fn device_named(home: &Home, id: &str) -> String {
        home.devices[&DeviceId::try_from(id).expect("valid")]
            .name
            .to_string()
    }

    // --- Settings -------------------------------------------------------------------------

    /// The point of the config directory: what a person calls their device beats what its
    /// firmware calls it, and taking the name away gives the firmware's name back rather than
    /// freezing whatever was on screen.
    #[test]
    fn a_name_a_person_chose_wins_and_can_be_taken_back() {
        let mut home = home_with_lamp();
        assert_eq!(device_named(&home, "demo_lamp"), "Desk lamp");

        let events = home.settle(Settings {
            devices: [(key("lamp"), called("Reading lamp"))].into(),
            ..Settings::default()
        });
        assert_eq!(device_named(&home, "demo_lamp"), "Reading lamp");
        assert_eq!(
            home.entities[&lamp_id()].name.as_str(),
            "Reading lamp",
            "a nameless entity follows its device"
        );
        assert_eq!(events.len(), 2, "the device and its entity: {events:?}");

        home.settle(Settings::default());
        assert_eq!(device_named(&home, "demo_lamp"), "Desk lamp");
        assert_eq!(home.entities[&lamp_id()].name.as_str(), "Desk lamp");
    }

    /// The whole of what Home Assistant gets wrong here: a device has one id, and nothing a
    /// person names it can change it — not a rename, not a restart after one, not the firmware
    /// calling it something else. The id is the protocol and its permanent handle.
    #[test]
    fn a_devices_id_never_changes_whatever_it_is_called() {
        let id = key("lamp");
        assert_eq!(id.as_str(), "demo_lamp");

        let mut home = home_with_lamp();
        home.settle(Settings {
            devices: [(id.clone(), called("Reading lamp"))].into(),
            ..Settings::default()
        });
        assert!(home.devices.contains_key(&id));
        assert_eq!(home.entities[&lamp_id()].name.as_str(), "Reading lamp");

        // As after a restart: settings first, then the protocol describes it again, under
        // a name its firmware has since changed.
        let mut restarted = Home::default();
        restarted.settle(Settings {
            devices: [(id.clone(), called("Reading lamp"))].into(),
            ..Settings::default()
        });
        restarted
            .describe_device(&protocol(), device("lamp", "Lamp v2"))
            .expect("device");
        restarted
            .describe_entity(
                &protocol(),
                ALL,
                entity("lamp-light", None, Some("lamp"), dimmable()),
                &stamp(0),
            )
            .expect("entity");
        assert_eq!(restarted.devices.keys().collect::<Vec<_>>(), [&id]);
        assert!(
            restarted.entities.contains_key(&lamp_id()),
            "the entity id didn't move either"
        );
        assert_eq!(device_named(&restarted, "demo_lamp"), "Reading lamp");
    }

    #[test]
    fn device_ids_are_readable_where_they_can_be_and_distinct_where_they_cant() {
        let id = |protocol: &str, unique: &str| {
            device_id_for(
                &ProtocolId::try_from(protocol).expect("valid"),
                &uid(unique),
            )
            .to_string()
        };
        assert_eq!(
            id("esphome", "00:11:22:33:44:55"),
            "esphome_00_11_22_33_44_55"
        );
        assert_eq!(id("mqtt", "0x00158d0001a2b3c4"), "mqtt_0x00158d0001a2b3c4");
        // Nothing a slug can keep: the handle is hashed rather than dropped.
        assert_ne!(id("demo", "玄関"), id("demo", "台所"));
        // Too long for an id: the beginning is kept, and the whole decides the ending.
        let long_a = format!("{}a", "x".repeat(80));
        let long_b = format!("{}b", "x".repeat(80));
        assert_ne!(id("demo", &long_a), id("demo", &long_b));
        assert!(id("demo", &long_a).len() <= SLUG_MAX_LEN);
        // A pure function: asked twice, the same answer.
        assert_eq!(id("demo", &long_a), id("demo", &long_a));
        // Two long protocol ids that share a slug prefix, same handle: the hash covers both,
        // so truncation can't make them collide.
        let prefix = "i".repeat(60);
        let left = format!("{prefix}a");
        let right = format!("{prefix}b");
        assert_ne!(
            id(&left, "same-handle"),
            id(&right, "same-handle"),
            "hash must include the protocol, not only the handle"
        );
    }

    /// Two handles that differ only in case would share an id. Refused with a reason, rather
    /// than numbered in whatever order they happened to be found.
    #[test]
    fn two_devices_whose_ids_would_collide_are_refused_rather_than_renumbered() {
        let mut home = Home::default();
        home.describe_device(&protocol(), device("AB", "One"))
            .expect("first");
        let refused = home
            .describe_device(&protocol(), device("ab", "Two"))
            .expect_err("same id");
        assert!(refused.0.contains("demo_ab"), "{}", refused.0);
    }

    /// What Irori's own config always says: ask before adding, with these devices added.
    fn asking(added: &[&str]) -> Settings {
        Settings {
            ask_before_adding: true,
            devices: added
                .iter()
                .map(|unique| {
                    (
                        key(unique),
                        irori_types::DeviceSettings {
                            added: true,
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            ..Settings::default()
        }
    }

    /// Irori's own device is in the home at once, even when every other device waits for "+ Add
    /// device".
    #[test]
    fn irori_itself_is_never_held() {
        let mut home = Home::default();
        home.settle(asking(&[]));
        let irori: ProtocolId = SYSTEM_PROTOCOL.parse().expect("a protocol id");
        home.describe_device(&irori, device("hub", "Irori"))
            .expect("Irori");
        home.describe_device(&protocol(), device("lamp", "Desk lamp"))
            .expect("lamp");
        let ids: Vec<String> = home.devices().map(|d| d.id.to_string()).collect();
        assert_eq!(ids, ["irori_hub"]);
        assert_eq!(home.held_devices().len(), 1, "the lamp waits");
    }

    /// Removing a device takes it and its entities out of the home, and puts it straight back
    /// among what its protocol has found — without the protocol having to describe it again,
    /// which a device that stays connected never would. What the protocol goes on saying is kept,
    /// not refused, and adding it back brings it in as it is now.
    #[test]
    fn a_removed_device_is_found_again_at_once_and_comes_back_as_it_is_now() {
        let mut home = Home::default();
        home.settle(asking(&["lamp"]));
        home.describe_device(&protocol(), device("lamp", "Desk lamp"))
            .expect("lamp");
        home.describe_entity(
            &protocol(),
            ALL,
            entity("lamp-light", None, Some("lamp"), dimmable()),
            &stamp(0),
        )
        .expect("light");
        home.report_state(
            &protocol(),
            report("lamp-light", Some(light(false, Some(10)))),
            &stamp(1),
        )
        .expect("report");

        // What `Config::forget_device` does: the row goes, then the core is told.
        let events = home.forget_device(&key("lamp")).expect("removed");
        home.settle(asking(&[]));
        assert!(home.devices.is_empty() && home.entities.is_empty() && home.states.is_empty());
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::DeviceRemoved { .. })),
            "{events:?}"
        );
        let found = home.held_devices();
        assert_eq!(found.len(), 1, "listed as found straight away");
        assert_eq!(found[0].name.as_str(), "Desk lamp");
        assert_eq!(found[0].provides, [("light".to_owned(), 1)].into());

        // The protocol carries on as usual, and nothing it says is an error.
        home.describe_device(&protocol(), device("lamp", "Desk lamp v2"))
            .expect("described while found");
        home.report_state(
            &protocol(),
            report("lamp-light", Some(light(true, Some(200)))),
            &stamp(2),
        )
        .expect("reported while found");
        home.set_availability(
            &protocol(),
            AvailabilityTarget::Device(uid("lamp")),
            Availability::Available,
            &stamp(2),
        )
        .expect("availability while found");
        assert!(home.devices.is_empty(), "still out of the home");
        assert!(
            home.resolve(&lamp_id(), Command::toggle()).is_err(),
            "a found device's entity can't be commanded"
        );

        home.settle(asking(&["lamp"]));
        assert!(home.held_devices().is_empty());
        assert_eq!(
            device_named(&home, "demo_lamp"),
            "Desk lamp v2",
            "its latest name"
        );
        assert_eq!(
            home.state(&lamp_id()).and_then(|state| state.state.clone()),
            Some(light(true, Some(200))),
            "its latest reading"
        );
    }

    /// Removing a device that's only found leaves it found; the protocol saying it's gone is
    /// what takes it off the list.
    #[test]
    fn a_found_device_stays_found_until_its_protocol_says_it_is_gone() {
        let mut home = Home::default();
        home.settle(asking(&[]));
        home.describe_device(&protocol(), device("lamp", "Desk lamp"))
            .expect("device");
        assert_eq!(home.held_devices().len(), 1);

        home.forget_device(&key("lamp")).expect("nothing to do");
        assert_eq!(home.held_devices().len(), 1);
        assert!(home.forget_device(&key("nobody")).is_err());

        home.remove_device(&protocol(), &uid("lamp"))
            .expect("removed");
        assert!(home.held_devices().is_empty());
    }

    /// While Irori asks before adding, a new device waits outside the home until a person adds
    /// it, then joins as it is now. One already added joins as soon as it's found.
    #[test]
    fn a_new_device_waits_to_be_added_while_irori_asks() {
        let mut home = Home::default();
        home.settle(asking(&["plug"]));
        home.describe_device(&protocol(), device("lamp", "Desk lamp"))
            .expect("lamp");
        home.describe_entity(
            &protocol(),
            ALL,
            entity("lamp-light", None, Some("lamp"), dimmable()),
            &stamp(0),
        )
        .expect("light");
        home.describe_device(&protocol(), device("plug", "Plug"))
            .expect("plug");

        assert_eq!(
            home.devices
                .keys()
                .map(DeviceId::as_str)
                .collect::<Vec<_>>(),
            ["demo_plug"],
            "the plug was added before it was found"
        );
        let held = home.held_devices();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].id.as_str(), "demo_lamp");
        assert_eq!(
            held[0].model.as_deref(),
            device("lamp", "Desk lamp").model.as_deref()
        );

        home.settle(asking(&["plug", "lamp"]));
        assert!(home.devices.contains_key(&key("lamp")));
        assert!(home.entities.contains_key(&lamp_id()), "with its entities");
        assert!(home.held_devices().is_empty());
    }

    /// A device that already has a `devices.toml` entry (a name, a room) is not new. Asking
    /// written in `irori.toml` before Irori starts must not hold a home someone already arranged.
    #[test]
    fn a_named_device_is_not_new_when_asking_is_already_on() {
        let mut home = Home::default();
        home.settle(Settings {
            ask_before_adding: true,
            devices: [(key("lamp"), called("Reading lamp"))].into(),
            ..Settings::default()
        });
        home.describe_device(&protocol(), device("lamp", "Desk lamp"))
            .expect("lamp");
        assert!(home.devices.contains_key(&key("lamp")));
        assert!(home.held_devices().is_empty());
    }

    #[test]
    fn a_device_has_the_description_a_person_gave_it() {
        let mut home = home_with_lamp();
        home.settle(Settings {
            devices: [(
                key("lamp"),
                irori_types::DeviceSettings {
                    description: Some("On the desk by the window".parse().expect("valid")),
                    ..Default::default()
                },
            )]
            .into(),
            ..Settings::default()
        });
        assert_eq!(
            home.devices[&key("lamp")]
                .description
                .as_ref()
                .map(irori_types::Description::as_str),
            Some("On the desk by the window")
        );
    }

    /// The protocol keeps describing the device while a person's name is in force. Its name
    /// must not win back, and the new manufacturer or firmware version must still land.
    #[test]
    fn a_protocol_that_describes_a_renamed_device_again_doesnt_rename_it_back() {
        let mut home = home_with_lamp();
        home.settle(Settings {
            devices: [(key("lamp"), called("Reading lamp"))].into(),
            ..Settings::default()
        });

        let mut again = device("lamp", "Desk lamp");
        again.sw_version = Some("2026.9.0".to_owned());
        let events = home.describe_device(&protocol(), again).expect("again");

        assert_eq!(device_named(&home, "demo_lamp"), "Reading lamp");
        assert_eq!(
            home.devices[&DeviceId::try_from("demo_lamp").expect("valid")].sw_version,
            Some("2026.9.0".to_owned()),
            "what the protocol is entitled to say still lands"
        );
        assert_eq!(events.len(), 1, "only the device changed: {events:?}");
    }

    /// An entity named by its protocol, or by a person, has a name of its own and keeps it
    /// when the device is renamed.
    #[test]
    fn an_entity_keeps_the_category_its_protocol_gives_it() {
        let mut home = home_with_lamp();
        let signal = || {
            entity(
                "lamp-signal",
                Some("Signal"),
                Some("lamp"),
                Capabilities::Sensor(SensorCapabilities {
                    value_type: SensorValueType::Number,
                    device_class: None,
                    unit: None,
                    state_class: None,
                    options: Vec::new(),
                }),
            )
        };
        let id = EntityId::try_from("sensor.demo_lamp_signal").expect("valid");
        let described = EntityDescription {
            entity_category: Some(irori_types::EntityCategory::Diagnostic),
            ..signal()
        };
        home.describe_entity(&protocol(), ALL, described, &stamp(0))
            .expect("entity");
        assert_eq!(
            home.entities[&id].entity_category,
            Some(irori_types::EntityCategory::Diagnostic)
        );

        // Described again without one, it's an ordinary entity again.
        home.describe_entity(&protocol(), ALL, signal(), &stamp(1))
            .expect("entity");
        assert_eq!(home.entities[&id].entity_category, None);
    }

    #[test]
    fn a_number_is_set_only_within_its_range() {
        let mut home = home_with_lamp();
        home.describe_entity(
            &protocol(),
            ALL,
            entity(
                "lamp-timeout",
                Some("Timeout"),
                Some("lamp"),
                Capabilities::Number(irori_types::NumberCapabilities {
                    min: 5.0,
                    max: 600.0,
                    step: 5.0,
                    unit: Some("s".into()),
                    device_class: None,
                    mode: irori_types::NumberMode::Auto,
                }),
            ),
            &stamp(0),
        )
        .expect("entity");
        let id = EntityId::try_from("number.demo_lamp_timeout").expect("valid");
        let set = |value: f64| {
            home.resolve(
                &id,
                Command::with("set_value", &irori_types::NumberSetValue { value }),
            )
        };
        assert_eq!(
            set(120.0).expect("in range").service,
            Service::NumberSetValue(irori_types::NumberSetValue { value: 120.0 })
        );
        assert_eq!(
            set(900.0).expect_err("out of range").to_string(),
            "`number.demo_lamp_timeout` goes from 5 to 600, not 900"
        );
        assert_eq!(
            home.resolve(&id, Command::toggle())
                .expect_err("not a toggle")
                .to_string(),
            "`number.demo_lamp_timeout` is a number; it can't be toggled"
        );
        // A report outside the range is refused too.
        let report = StateReport {
            unique_id: uid("lamp-timeout"),
            state: Some(State::Number(irori_types::NumberState { value: 1.0 })),
            attributes: Default::default(),
            caused_by: None,
        };
        assert!(home.report_state(&protocol(), report, &stamp(1)).is_err());
    }

    #[test]
    fn only_entities_without_a_name_of_their_own_follow_the_device() {
        let mut home = home_with_lamp();
        home.describe_entity(
            &protocol(),
            ALL,
            entity(
                "lamp-power",
                Some("Power"),
                Some("lamp"),
                Capabilities::Sensor(SensorCapabilities {
                    value_type: SensorValueType::Number,
                    device_class: None,
                    unit: None,
                    state_class: None,
                    options: Vec::new(),
                }),
            ),
            &stamp(0),
        )
        .expect("entity");

        home.settle(Settings {
            floors: Vec::new(),
            devices: [(key("lamp"), called("Reading lamp"))].into(),
            entities: [(
                entity_key("lamp-light"),
                irori_types::EntitySettings {
                    name: Some(name("Reading light")),
                },
            )]
            .into(),
            ..Settings::default()
        });

        assert_eq!(
            home.entities[&lamp_id()].name.as_str(),
            "Reading light",
            "a name a person gave the entity stops it following its device"
        );
        assert_eq!(
            home.entities[&EntityId::try_from("sensor.demo_lamp_power").expect("valid")]
                .name
                .as_str(),
            "Power",
            "a name the protocol gave the entity is its own too"
        );
    }

    #[test]
    fn a_new_rooms_id_comes_from_its_name_and_never_collides() {
        let kitchen = area("kitchen", "Kitchen");
        assert_eq!(new_area_id(&name("Kitchen"), &[]).as_str(), "kitchen");
        assert_eq!(
            new_area_id(&name("Kitchen"), std::slice::from_ref(&kitchen)).as_str(),
            "kitchen_2"
        );
        // A name with nothing a slug can use still has to produce an id.
        assert_eq!(new_area_id(&name("玄関"), &[]).as_str(), "area");
    }

    #[test]
    fn a_device_goes_in_the_room_a_person_puts_it_in() {
        let mut home = home_with_lamp();
        let id = DeviceId::try_from("demo_lamp").expect("valid");
        assert_eq!(home.devices[&id].area_id, None);

        home.settle(Settings {
            areas: vec![area("study", "Study")],
            devices: [(key("lamp"), in_room("study"))].into(),
            ..Settings::default()
        });
        assert_eq!(home.devices[&id].area_id, Some(area("study", "Study").id));
    }

    /// A device that says which room it's in doesn't get to invent one, but it does take its
    /// place the moment that room exists — including when the room is made later.
    #[test]
    fn a_devices_own_suggestion_is_used_only_once_that_room_exists() {
        let mut home = Home::default();
        let mut described = device("lamp", "Desk lamp");
        described.suggested_area = Some(name("Study"));
        home.describe_device(&protocol(), described)
            .expect("device");
        let id = DeviceId::try_from("demo_lamp").expect("valid");
        assert_eq!(home.devices[&id].area_id, None, "no such room yet");

        let events = home.settle(Settings {
            areas: vec![area("study", "Study")],
            ..Settings::default()
        });
        assert_eq!(home.devices[&id].area_id, Some(area("study", "Study").id));
        assert_eq!(events.len(), 1, "{events:?}");
    }

    /// "Not in a room" has to be an answer, not the absence of one. Otherwise saying it to a
    /// device whose firmware suggests a room would clear the setting, let the suggestion back in,
    /// and put the device straight back where it was — the control would be a lie.
    #[test]
    fn a_device_can_be_kept_out_of_the_room_its_firmware_asks_for() {
        let mut home = Home::default();
        let mut described = device("lamp", "Desk lamp");
        described.suggested_area = Some(name("Study"));
        home.describe_device(&protocol(), described)
            .expect("device");
        let id = DeviceId::try_from("demo_lamp").expect("valid");

        home.settle(Settings {
            areas: vec![area("study", "Study")],
            ..Settings::default()
        });
        assert_eq!(
            home.devices[&id].area_id,
            Some(area("study", "Study").id),
            "the suggestion stands in while nobody has said otherwise"
        );

        home.settle(Settings {
            areas: vec![area("study", "Study")],
            devices: [(key("lamp"), nowhere())].into(),
            ..Settings::default()
        });
        assert_eq!(home.devices[&id].area_id, None, "and a person can say no");
    }

    /// Deleting a room shouldn't quietly hand a device back to whatever its firmware suggests:
    /// the person's choice is still written down, so the device is simply unplaced.
    #[test]
    fn a_choice_pointing_at_a_deleted_room_leaves_the_device_unplaced() {
        let mut home = Home::default();
        let mut described = device("lamp", "Desk lamp");
        described.suggested_area = Some(name("Study"));
        home.describe_device(&protocol(), described)
            .expect("device");

        home.settle(Settings {
            areas: vec![area("study", "Study"), area("hall", "Hall")],
            devices: [(key("lamp"), in_room("hall"))].into(),
            ..Settings::default()
        });
        let id = DeviceId::try_from("demo_lamp").expect("valid");
        assert_eq!(home.devices[&id].area_id, Some(area("hall", "Hall").id));

        home.settle(Settings {
            areas: vec![area("study", "Study")],
            devices: [(key("lamp"), in_room("hall"))].into(),
            ..Settings::default()
        });
        assert_eq!(home.devices[&id].area_id, None);
    }

    /// Settings are read before any protocol starts, so the common case is a device arriving
    /// into a home that already has an opinion about it. It should never appear under its old
    /// name, not even for an instant.
    #[test]
    fn a_device_that_arrives_later_is_named_and_placed_as_it_appears() {
        let mut home = Home::default();
        home.settle(Settings {
            ask_before_adding: false,
            floors: Vec::new(),
            areas: vec![area("study", "Study")],
            floorplan: irori_types::Floorplan::default(),
            devices: [(
                key("lamp"),
                irori_types::DeviceSettings {
                    added: false,
                    name: Some(name("Reading lamp")),
                    description: None,
                    area: Placement::In(AreaId::try_from("study").expect("valid")),
                },
            )]
            .into(),
            entities: [(
                entity_key("lamp-light"),
                irori_types::EntitySettings {
                    name: Some(name("Reading light")),
                },
            )]
            .into(),
        });

        let events = home
            .describe_device(&protocol(), device("lamp", "Desk lamp"))
            .expect("device");
        let Some(Event::DeviceAdded { device }) = events.first() else {
            panic!("a device was added: {events:?}");
        };
        assert_eq!(device.name.as_str(), "Reading lamp");
        assert_eq!(device.area_id, Some(area("study", "Study").id));
        assert_eq!(
            device.id.as_str(),
            "demo_lamp",
            "the id is the device's own, whatever it has been named"
        );

        home.describe_entity(
            &protocol(),
            ALL,
            entity("lamp-light", None, Some("lamp"), dimmable()),
            &stamp(0),
        )
        .expect("entity");
        assert_eq!(
            home.entities
                .values()
                .map(|entity| entity.name.to_string())
                .collect::<Vec<_>>(),
            ["Reading light"]
        );
    }

    #[test]
    fn settings_that_change_nothing_change_nothing() {
        let mut home = home_with_lamp();
        let settings = Settings {
            areas: vec![area("study", "Study")],
            devices: [(key("lamp"), called("Reading lamp"))].into(),
            ..Settings::default()
        };

        assert_eq!(home.settle(settings.clone()).len(), 2);
        assert!(home.settle(settings).is_empty(), "applied twice");
    }

    /// Settings are kept for things that aren't here: a device unplugged for a week comes back to
    /// the name it had, rather than to its firmware's.
    #[test]
    fn a_setting_for_something_absent_is_kept_and_waits() {
        let mut home = Home::default();
        home.settle(Settings {
            devices: [(key("nothing_here"), called("Someday"))].into(),
            ..Settings::default()
        });
        assert_eq!(home.settings().devices.len(), 1);
    }

    #[test]
    fn slugs_and_numbered_ids_never_collide() {
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

        // Two devices with the same name are still two ids: names aren't what ids are made of.
        let mut home = Home::default();
        for unique in ["a", "b"] {
            home.describe_device(&protocol(), device(unique, "玄関 light"))
                .expect("device");
        }
        let ids: Vec<_> = home.devices().map(|d| d.id.to_string()).collect();
        assert_eq!(ids, ["demo_a", "demo_b"]);
    }

    /// An entity's id is its device's id and the name its protocol gave it: never a name a
    /// person chose, so no rename can leave an id that says something else.
    #[test]
    fn entities_get_ids_from_their_devices_id_and_their_own_reported_name() {
        let mut home = home_with_lamp();
        home.describe_device(&protocol(), device("sensor", "Hallway sensor"))
            .expect("device");
        home.describe_entity(
            &protocol(),
            ALL,
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
        assert_eq!(ids, ["binary_sensor.demo_sensor_motion", "light.demo_lamp"]);
        // A nameless entity takes its device's name.
        assert_eq!(home.entities[&lamp_id()].name.as_str(), "Desk lamp");
        // New entities start available and unknown, an entry Irori made itself.
        let state = home.state(&lamp_id()).expect("state");
        assert_eq!(state.availability, Availability::Available);
        assert_eq!(state.state, None);
        assert!(matches!(state.context.origin, Origin::System));
    }

    #[test]
    fn describing_again_updates_in_place() {
        let mut home = home_with_lamp();
        let again = home
            .describe_entity(
                &protocol(),
                ALL,
                entity("lamp-light", None, Some("lamp"), dimmable()),
                &stamp(1),
            )
            .expect("same entity");
        assert!(again.is_empty(), "nothing changed, so no events");
        assert_eq!(home.entities().count(), 1);

        let err = home
            .describe_entity(
                &protocol(),
                ALL,
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
                &protocol(),
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
                &protocol(),
                ALL,
                entity("x", None, Some("missing"), dimmable()),
                &stamp(0),
            )
            .expect_err("no device");
        assert!(err.0.contains("describe the device first"), "{err}");

        home.describe_device(&protocol(), device("hub", "Hub"))
            .expect("hub");
        let mut child = device("child", "Child");
        child.via_device_unique_id = Some(uid("hub"));
        home.describe_device(&protocol(), child).expect("child");
        let mut hub = device("hub", "Hub");
        hub.via_device_unique_id = Some(uid("child"));
        let err = home.describe_device(&protocol(), hub).expect_err("loop");
        assert!(err.0.contains("would make a loop"), "{err}");

        // Another protocol can't reach this one's devices.
        let other = ProtocolId::try_from("other").expect("valid");
        let err = home
            .describe_entity(
                &other,
                ALL,
                entity("y", None, Some("hub"), dimmable()),
                &stamp(0),
            )
            .expect_err("foreign device");
        assert!(
            err.0.contains("isn't a device this protocol described"),
            "{err}"
        );
    }

    #[test]
    fn reports_update_timestamps_and_context() {
        let mut home = home_with_lamp();
        let events = home
            .report_state(
                &protocol(),
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
                &protocol(),
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
            &protocol(),
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
                &protocol(),
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
            .report_state(&protocol(), report("lamp-light", Some(rgb)), &stamp(1))
            .expect_err("no rgb");
        assert!(err.0.contains("doesn't support RGB color"), "{err}");

        home.describe_entity(
            &protocol(),
            ALL,
            entity(
                "temp",
                Some("Temperature"),
                None,
                Capabilities::Sensor(SensorCapabilities {
                    value_type: SensorValueType::Number,
                    device_class: None,
                    unit: None,
                    state_class: None,
                    options: Vec::new(),
                }),
            ),
            &stamp(0),
        )
        .expect("sensor");
        let err = home
            .report_state(
                &protocol(),
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
            .report_state(&protocol(), report("nope", None), &stamp(1))
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
            .report_state(&protocol(), report("lamp-light", Some(rgb_mode)), &stamp(1))
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
            &protocol(),
            report("lamp-light", Some(light(true, Some(80)))),
            &stamp(1),
        )
        .expect("fits");
        let not_dimmable = Capabilities::Light(LightCapabilities::default());
        home.describe_entity(
            &protocol(),
            ALL,
            entity("lamp-light", None, Some("lamp"), not_dimmable.clone()),
            &stamp(2),
        )
        .expect("re-described");
        let state = home.state(&lamp_id()).expect("state");
        assert_eq!(state.state, None);
        // Irori forgot it; the device didn't report it.
        assert!(matches!(state.context.origin, Origin::System));

        // A value that still fits is kept.
        home.report_state(
            &protocol(),
            report("lamp-light", Some(light(true, None))),
            &stamp(3),
        )
        .expect("fits");
        home.describe_entity(
            &protocol(),
            ALL,
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
    fn caused_by_must_be_a_call_to_this_protocol() {
        let mut home = home_with_lamp();
        let call = stamp(1).context_id;
        let mut confirmed = report("lamp-light", Some(light(false, None)));
        confirmed.caused_by = Some(call.clone());
        let err = home
            .report_state(&protocol(), confirmed.clone(), &stamp(2))
            .expect_err("unknown call");
        assert!(err.0.contains("in the last 5 minutes"), "{err}");

        home.record_call(&protocol(), call.clone(), stamp(1).now);
        home.report_state(&protocol(), confirmed, &stamp(2))
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
            &protocol(),
            report("lamp-light", Some(light(true, Some(50)))),
            &stamp(1),
        )
        .expect("fits");
        let events = home.mark_unavailable(&protocol(), &stamp(2));
        assert_eq!(events.len(), 1);
        let state = home.state(&lamp_id()).expect("state");
        assert_eq!(state.availability, Availability::Unavailable);
        assert_eq!(state.state, Some(light(true, Some(50))));
        assert!(matches!(state.context.origin, Origin::System));

        home.set_availability(
            &protocol(),
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
    fn names_follow_the_protocol_and_nameless_entities_follow_their_device() {
        let mut home = home_with_lamp();
        let events = home
            .describe_device(&protocol(), device("lamp", "Reading lamp"))
            .expect("renamed");
        assert_eq!(events.len(), 2, "device and its nameless entity updated");
        let lamp = home.entities.get(&lamp_id()).expect("same id");
        assert_eq!(lamp.name.as_str(), "Reading lamp");

        home.describe_entity(
            &protocol(),
            ALL,
            entity("lamp-light", Some("Bulb"), Some("lamp"), dimmable()),
            &stamp(1),
        )
        .expect("named now");
        home.describe_device(&protocol(), device("lamp", "Desk lamp"))
            .expect("renamed");
        assert_eq!(home.entities[&lamp_id()].name.as_str(), "Bulb");
    }

    #[test]
    fn repeated_availability_reports_count_as_reports_but_crashes_dont() {
        let mut home = home_with_lamp();
        let events = home
            .set_availability(
                &protocol(),
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
        home.mark_unavailable(&protocol(), &stamp(6));
        let state = home.state(&lamp_id()).expect("state");
        assert_eq!(state.availability, Availability::Unavailable);
        assert_eq!(state.last_changed, stamp(6).now);
        assert_eq!(state.last_reported, stamp(5).now);
        state
            .validate()
            .expect("valid with last_reported before last_changed");

        // The protocol saying so itself is a report.
        home.set_availability(
            &protocol(),
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
    fn calls_can_be_confirmed_for_five_minutes_even_after_many_others() {
        let mut home = home_with_lamp();
        let call = stamp(100).context_id;
        home.record_call(&protocol(), call.clone(), stamp(100).now);
        for i in 0..200 {
            home.record_call(&protocol(), stamp(101 + i).context_id, stamp(101).now);
        }
        let mut confirmed = report("lamp-light", Some(light(false, None)));
        confirmed.caused_by = Some(call.clone());
        home.report_state(&protocol(), confirmed.clone(), &stamp(100 + 300))
            .expect("within five minutes, after 200 other calls");

        let mut late = confirmed;
        late.state = Some(light(true, None));
        let err = home
            .report_state(&protocol(), late, &stamp(100 + 301))
            .expect_err("too late");
        assert!(err.0.contains("in the last 5 minutes"), "{err}");
    }

    #[test]
    fn the_call_window_survives_a_clock_correction() {
        let mut home = home_with_lamp();
        let call = stamp(1_000).context_id;
        home.record_call(&protocol(), call.clone(), stamp(1_000).now);
        let mut confirmed = report("lamp-light", Some(light(false, None)));
        confirmed.caused_by = Some(call);
        // The clock stepped back a minute between the call and its confirmation.
        home.report_state(&protocol(), confirmed.clone(), &stamp(940))
            .expect("still within the window");
        // A clock a day behind isn't a correction; that call isn't recent.
        let err = home
            .report_state(&protocol(), confirmed, &stamp(1_000 - 86_400))
            .expect_err("far outside the window");
        assert!(err.0.contains("in the last 5 minutes"), "{err}");
    }

    #[test]
    fn toggle_follows_the_last_command_until_the_device_reports() {
        let mut home = home_with_lamp();
        home.report_state(
            &protocol(),
            report("lamp-light", Some(light(false, None))),
            &stamp(1),
        )
        .expect("fits");

        // First toggle: it's off, so turn it on.
        let first = home.resolve(&lamp_id(), Command::toggle()).expect("light");
        assert_eq!(first.service, Service::LightTurnOn(LightTurnOn::default()));
        home.record_command(&lamp_id(), &first.service);

        // Second toggle before the lamp has reported: it must undo the first, not repeat it.
        let second = home.resolve(&lamp_id(), Command::toggle()).expect("light");
        assert_eq!(second.service, Service::LightTurnOff);
        home.record_command(&lamp_id(), &second.service);

        // Once the lamp reports, its own word counts again.
        home.report_state(
            &protocol(),
            report("lamp-light", Some(light(true, None))),
            &stamp(2),
        )
        .expect("fits");
        assert_eq!(
            home.resolve(&lamp_id(), Command::toggle())
                .expect("light")
                .service,
            Service::LightTurnOff
        );
    }

    #[test]
    fn a_call_is_refused_when_the_entity_changed_underneath_it() {
        let mut home = home_with_lamp();
        let resolved = home.resolve(&lamp_id(), Command::toggle()).expect("light");
        assert!(home.still_dispatchable(&lamp_id(), &resolved));

        // The lamp is re-described without dimming, and the brightness it was asked for is gone.
        let bright = home
            .resolve(
                &lamp_id(),
                Command::with(
                    "turn_on",
                    &LightTurnOn {
                        brightness: Some(200),
                        ..LightTurnOn::default()
                    },
                ),
            )
            .expect("dimmable for now");
        home.describe_entity(
            &protocol(),
            ALL,
            entity(
                "lamp-light",
                None,
                Some("lamp"),
                Capabilities::Light(LightCapabilities::default()),
            ),
            &stamp(2),
        )
        .expect("re-described");
        assert!(!home.still_dispatchable(&lamp_id(), &bright));

        // And once it's gone entirely.
        home.remove_entity(&protocol(), &uid("lamp-light"))
            .expect("exists");
        assert!(!home.still_dispatchable(&lamp_id(), &resolved));
    }

    #[test]
    fn describing_an_entity_again_counts_as_hearing_from_it() {
        let mut home = home_with_lamp();
        home.report_state(
            &protocol(),
            report("lamp-light", Some(light(true, None))),
            &stamp(10),
        )
        .expect("fits");
        home.mark_unavailable(&protocol(), &stamp(20));
        assert_eq!(
            home.state(&lamp_id()).expect("state").last_reported,
            stamp(10).now
        );

        // The protocol restarts and describes it again: back online, and heard from.
        home.describe_entity(
            &protocol(),
            ALL,
            entity("lamp-light", None, Some("lamp"), dimmable()),
            &stamp(30),
        )
        .expect("re-described");
        let state = home.state(&lamp_id()).expect("state");
        assert_eq!(state.availability, Availability::Available);
        assert_eq!(state.last_reported, stamp(30).now);
    }

    #[test]
    fn forgotten_calls_cant_be_blamed() {
        let mut home = home_with_lamp();
        let call = stamp(1).context_id;
        home.record_call(&protocol(), call.clone(), stamp(1).now);
        home.forget_call(&protocol(), &call);
        let mut confirmed = report("lamp-light", Some(light(false, None)));
        confirmed.caused_by = Some(call);
        assert!(
            home.report_state(&protocol(), confirmed, &stamp(2))
                .is_err()
        );
    }

    #[test]
    fn removing_a_device_removes_its_entities() {
        let mut home = home_with_lamp();
        let events = home
            .remove_device(&protocol(), &uid("lamp"))
            .expect("exists");
        assert_eq!(events.len(), 2);
        assert_eq!(home.entities().count(), 0);
        assert_eq!(home.states().count(), 0);
        assert!(home.remove_device(&protocol(), &uid("lamp")).is_err());
    }

    #[test]
    fn service_calls_are_checked_and_toggles_resolved() {
        let mut home = home_with_lamp();
        let resolved = home.resolve(&lamp_id(), Command::toggle()).expect("light");
        assert_eq!(
            resolved.service,
            Service::LightTurnOn(LightTurnOn::default())
        );
        assert_eq!(resolved.unique_id, uid("lamp-light"));

        home.report_state(
            &protocol(),
            report("lamp-light", Some(light(true, None))),
            &stamp(1),
        )
        .expect("fits");
        assert_eq!(
            home.resolve(&lamp_id(), Command::toggle())
                .expect("light")
                .service,
            Service::LightTurnOff
        );

        let too_warm = LightTurnOn {
            color_temp_kelvin: Some(2000),
            ..LightTurnOn::default()
        };
        let err = home
            .resolve(&lamp_id(), Command::with("turn_on", &too_warm))
            .expect_err("range");
        assert_eq!(
            err.to_string(),
            "`light.demo_lamp` supports color temperatures from 2700 to 6500 K, not 2000 K"
        );

        let unknown = EntityId::try_from("light.nope").expect("valid");
        assert!(matches!(
            home.resolve(&unknown, Command::turn_off()),
            Err(CallError::UnknownEntity(_))
        ));

        // The action has to be one of the kind's, with the data its service takes.
        let refusal = |command| home.resolve(&lamp_id(), command).expect_err("refused");
        assert_eq!(
            refusal(Command::new("open")).to_string(),
            "`light.demo_lamp` is a light; it has no `open`"
        );
        assert_eq!(
            refusal(Command::with("toggle", &too_warm)).to_string(),
            "`light.toggle` takes no data"
        );
        assert_eq!(
            refusal(Command::with("turn_off", &too_warm)).to_string(),
            "`light.turn_off` takes no data"
        );
    }
}
