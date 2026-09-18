//! The home: the registry (what exists) and live state (what it's doing), changed one operation at
//! a time. Pure and synchronous, so every rule of the contract is easy to test. The spec's "what
//! the core checks" (`docs/specs/integrations.md` §8) lives here.

use std::collections::{BTreeMap, HashMap, VecDeque};

use irori_integration::{AvailabilityTarget, Rejected};
use irori_types::{
    Area, AreaId, Availability, Capabilities, ColorMode, Context, ContextId, Device,
    DeviceDescription, DeviceId, Entity, EntityDescription, EntityId, EntityKind, EntityState,
    IntegrationId, LightCapabilities, LightTurnOn, Name, Origin, Placement, SLUG_MAX_LEN,
    SensorValue, SensorValueType, Service, Settings, SettingsKey, State, StateReport, Timestamp,
    UniqueId,
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

/// How long a delivered service call can be named in `caused_by`: the window for a device to
/// confirm a command, even when its integration is busy.
const CALL_WINDOW: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// A memory bound per integration: past this many calls within the window, the oldest are
/// forgotten. Far more than any home sends to one integration in five minutes.
const MAX_RECENT_CALLS: usize = 1024;

#[derive(Debug, Default)]
pub(crate) struct Home {
    devices: BTreeMap<DeviceId, Device>,
    device_keys: HashMap<Key, DeviceId>,
    entities: BTreeMap<EntityId, Entity>,
    entity_keys: HashMap<Key, EntityId>,
    states: BTreeMap<EntityId, EntityState>,
    /// What each integration calls its devices, kept even while a person's name is being shown,
    /// so removing that name restores the original rather than freezing it.
    reported_device_names: HashMap<DeviceId, Name>,
    /// The same for entities. An entity that is *absent* here was described without a name, and
    /// uses (and follows) its device's name.
    reported_entity_names: HashMap<EntityId, Name>,
    /// What a person has said about all this (`docs/specs/config.md`). Overrides what the
    /// integrations report.
    settings: Settings,
    /// What each integration last said about its devices and entities, as it said it. Kept so
    /// a device can be taken out of the home and put back without asking the integration again.
    device_descriptions: HashMap<DeviceId, DeviceDescription>,
    entity_descriptions: HashMap<EntityId, (Vec<EntityKind>, EntityDescription)>,
    /// Devices a person has ignored: out of the registry, but remembered.
    ignored: BTreeMap<DeviceId, Ignored>,
    /// Which ignored device each of their entities belongs to, to recognise what arrives for them.
    ignored_entities: HashMap<Key, DeviceId>,
    /// What the core last told an entity to be, until the device reports back. `Toggle` uses it,
    /// so two toggles in a row don't both see the old value while the first is still in flight.
    commanded: HashMap<EntityId, bool>,
    recent_calls: HashMap<IntegrationId, VecDeque<(ContextId, Timestamp)>>,
}

/// A device a person has chosen to keep out of the home (`docs/specs/config.md` §3.2).
///
/// Everything its integration says about it lands here instead of in the registry: nothing is
/// listed, nothing can be switched, nothing is recorded. The latest of it is kept, so stopping
/// ignoring the device puts it back as it is now, not as it was.
#[derive(Debug, Clone)]
struct Ignored {
    integration: IntegrationId,
    description: DeviceDescription,
    entities: BTreeMap<UniqueId, (Vec<EntityKind>, EntityDescription)>,
    reports: BTreeMap<UniqueId, StateReport>,
    /// What the integration last said about whether the device can be reached.
    available: bool,
}

/// A device found but kept out of the home, as the page lists it: enough to recognise it and
/// let it in.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HeldDevice {
    pub id: DeviceId,
    pub integration: IntegrationId,
    pub name: Name,
    pub why: Held,
}

/// Why a device is kept out of the home.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Held {
    /// A person ignored it.
    Ignored,
    /// It's new, and Irori asks before adding new devices.
    New,
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

    // --- Settings -------------------------------------------------------------------------

    pub fn areas(&self) -> &[Area] {
        &self.settings.areas
    }

    pub fn floors(&self) -> &[irori_types::Floor] {
        &self.settings.floors
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Whether a device is kept out of the home, and why. Ignoring wins over being new: an
    /// ignored device stays ignored whatever Irori asks about new ones. Any other `devices.toml`
    /// entry means it isn't new — a name or a room is as much a decision as `added = true`, so
    /// turning asking on in `irori.toml` and restarting doesn't hold a home that was already named.
    fn held(&self, id: &DeviceId) -> Option<Held> {
        let settings = self.settings.devices.get(id);
        if settings.is_some_and(|settings| settings.ignored) {
            return Some(Held::Ignored);
        }
        let known = settings.is_some();
        (self.settings.ask_before_adding && !known).then_some(Held::New)
    }

    fn is_held(&self, id: &DeviceId) -> bool {
        self.held(id).is_some()
    }

    pub fn held_devices(&self) -> Vec<HeldDevice> {
        self.ignored
            .iter()
            .filter_map(|(id, held)| {
                Some(HeldDevice {
                    id: id.clone(),
                    integration: held.integration.clone(),
                    name: self.name_for_device(id, &held.description.name),
                    why: self.held(id)?,
                })
            })
            .collect()
    }

    /// Takes a device out of the home, keeping what its integration said so it can come back.
    fn ignore(&mut self, id: &DeviceId, _stamp: &Stamp) -> Vec<Event> {
        let (Some(device), Some(description)) = (
            self.devices.get(id).cloned(),
            self.device_descriptions.get(id).cloned(),
        ) else {
            return vec![];
        };
        let mut ignored = Ignored {
            integration: device.integration.clone(),
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
                ignored
                    .entities
                    .insert(entity.unique_id.clone(), described.clone());
            }
            if let Some(state) = self.states.get(&entity.id) {
                if state.availability == Availability::Unavailable {
                    ignored.available = false;
                }
                ignored.reports.insert(
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
        // Removed first, then recorded as ignored: the other way round, removal would take the
        // entities for already-ignored ones and leave them in the registry without their device.
        let events = self
            .remove_device(&device.integration, &device.unique_id)
            .unwrap_or_default();
        for entity in &owned {
            self.ignored_entities.insert(
                (device.integration.clone(), entity.unique_id.clone()),
                id.clone(),
            );
        }
        self.ignored.insert(id.clone(), ignored);
        events
    }

    /// Puts an ignored device back, as its integration last described it.
    fn restore(&mut self, id: &DeviceId, stamp: &Stamp) -> Vec<Event> {
        let Some(ignored) = self.ignored.remove(id) else {
            return vec![];
        };
        let integration = ignored.integration;
        for unique_id in ignored.entities.keys() {
            self.ignored_entities
                .remove(&(integration.clone(), unique_id.clone()));
        }
        let mut events = Vec::new();
        // Each step is what the integration said and the core accepted before; if one is
        // refused now (the registry changed meanwhile), the rest still go back.
        events.extend(
            self.describe_device(&integration, ignored.description.clone())
                .unwrap_or_default(),
        );
        for (kinds, description) in ignored.entities.into_values() {
            events.extend(
                self.describe_entity(&integration, &kinds, description, stamp)
                    .unwrap_or_default(),
            );
        }
        for report in ignored.reports.into_values() {
            events.extend(
                self.report_state(&integration, report, stamp)
                    .unwrap_or_default(),
            );
        }
        if !ignored.available {
            events.extend(
                self.set_availability(
                    &integration,
                    AvailabilityTarget::Device(ignored.description.unique_id),
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
            entity.integration.clone(),
            entity.unique_id.clone(),
        ))
    }

    /// What a device is called: the name a person gave it, else the one its integration reports
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

    /// What an entity is called: the name a person gave it, else the one its integration
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

        // Ignoring first, so the renames below only touch what's still in the home.
        let now_ignored: Vec<DeviceId> = self
            .devices
            .keys()
            .filter(|id| self.is_held(id))
            .cloned()
            .collect();
        for id in now_ignored {
            events.extend(self.ignore(&id, stamp));
        }
        let no_longer_ignored: Vec<DeviceId> = self
            .ignored
            .keys()
            .filter(|id| !self.is_held(id))
            .cloned()
            .collect();
        for id in no_longer_ignored {
            events.extend(self.restore(&id, stamp));
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
        integration: &IntegrationId,
        description: DeviceDescription,
    ) -> Result<Vec<Event>, Rejected> {
        description
            .validate()
            .map_err(|e| Rejected(e.to_string()))?;
        let unique_id = &description.unique_id;
        let ignored_id = device_id_for(integration, unique_id);
        if self.is_held(&ignored_id) {
            match self.ignored.get_mut(&ignored_id) {
                Some(ignored) => ignored.description = description,
                None => {
                    self.ignored.insert(
                        ignored_id,
                        Ignored {
                            integration: integration.clone(),
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
        // Reached through a device that's ignored: as far as the home knows, it's reached directly.
        let via_unique_id = description
            .via_device_unique_id
            .clone()
            .filter(|via| !self.ignored.contains_key(&device_id_for(integration, via)));
        let via = match &via_unique_id {
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
                    // own, from the integration or from a person, keeps it.
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
        let id = device_id_for(integration, &description.unique_id);
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
            integration: integration.clone(),
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
        if let Some(device) = &description.device_unique_id {
            let device_id = device_id_for(integration, device);
            if let Some(ignored) = self.ignored.get_mut(&device_id) {
                self.ignored_entities
                    .insert((integration.clone(), unique_id.clone()), device_id);
                ignored
                    .entities
                    .insert(unique_id.clone(), (kinds.to_vec(), description));
                return Ok(vec![]);
            }
        }
        let described = (kinds.to_vec(), description.clone());
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
            // Being described means the integration is back in touch with it (e.g. after a
            // restart). If the device is still offline, the integration says so next.
            let context = device_context(integration, stamp, None);
            // Describing an entity is the integration telling Irori about it, so it counts as
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
                integration.clone(),
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
        // The id is built from the device's id and the name the integration gave the entity —
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
            // Irori creates this "nothing reported yet" entry; the device hasn't said anything.
            context: system_context(stamp),
        };
        if let Some(reported) = description.name {
            self.reported_entity_names.insert(id.clone(), reported);
        }
        self.entity_descriptions.insert(id.clone(), described);
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
        if let Some(device) = self.ignored_entities.remove(&key) {
            if let Some(ignored) = self.ignored.get_mut(&device) {
                ignored.entities.remove(unique_id);
                ignored.reports.remove(unique_id);
            }
            return Ok(vec![]);
        }
        let id = self.entity_keys.remove(&key).ok_or_else(|| {
            Rejected(format!(
                "can't remove entity `{unique_id}`: this integration has no such entity"
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
        integration: &IntegrationId,
        unique_id: &UniqueId,
    ) -> Result<Vec<Event>, Rejected> {
        let key = (integration.clone(), unique_id.clone());
        if let Some(ignored) = self.ignored.remove(&device_id_for(integration, unique_id)) {
            for entity in ignored.entities.keys() {
                self.ignored_entities
                    .remove(&(integration.clone(), entity.clone()));
            }
            return Ok(vec![]);
        }
        let id = self.device_keys.remove(&key).ok_or_else(|| {
            Rejected(format!(
                "can't remove device `{unique_id}`: this integration has no such device"
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
        if let Some(device) = self
            .ignored_entities
            .get(&(integration.clone(), unique_id.clone()))
        {
            if let Some(ignored) = self.ignored.get_mut(device) {
                // Kept without its cause: by the time it's put back, no call is waiting on it.
                let mut report = report;
                report.caused_by = None;
                ignored.reports.insert(report.unique_id.clone(), report);
            }
            return Ok(vec![]);
        }
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
                if self.recent_calls.get(integration).is_some_and(|calls| {
                    calls
                        .iter()
                        .any(|(id, at)| id == caused_by && within_call_window(*at, stamp.now))
                }) =>
            {
                Some(caused_by.clone())
            }
            Some(caused_by) => {
                return Err(Rejected(format!(
                    "state report for `{unique_id}`: caused_by `{caused_by}` isn't a service call sent to this integration in the last 5 minutes"
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
        let target = match target {
            AvailabilityTarget::Device(device) => {
                if let Some(ignored) = self.ignored.get_mut(&device_id_for(integration, &device)) {
                    ignored.available = availability == Availability::Available;
                    return Ok(vec![]);
                }
                AvailabilityTarget::Device(device)
            }
            AvailabilityTarget::Entities(unique_ids) => {
                let kept: Vec<UniqueId> = unique_ids
                    .into_iter()
                    .filter(|u| {
                        !self
                            .ignored_entities
                            .contains_key(&(integration.clone(), u.clone()))
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
        let is_on = || match self.commanded.get(entity_id) {
            Some(on) => *on,
            None => match self.states.get(entity_id).and_then(|s| s.state.as_ref()) {
                Some(State::Light(light)) => light.on,
                Some(State::Switch(switch)) => switch.on,
                _ => false,
            },
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
            calls.retain(|(id, _)| id != context_id);
        }
    }

    /// Whether a report may name this call in `caused_by`.
    #[cfg(test)]
    pub fn knows_call(&self, integration: &IntegrationId, context_id: &ContextId) -> bool {
        self.recent_calls
            .get(integration)
            .is_some_and(|calls| calls.iter().any(|(id, _)| id == context_id))
    }

    /// Forgets what an entity was told to be, e.g. because the call failed.
    pub fn forget_command(&mut self, entity_id: &EntityId) {
        self.commanded.remove(entity_id);
    }

    /// Whether a resolved call still matches the registry: the same entity, still owned by that
    /// integration under that `unique_id`, and still able to do what's asked.
    pub fn still_dispatchable(&self, entity_id: &EntityId, resolved: &Resolved) -> bool {
        self.entities.get(entity_id).is_some_and(|entity| {
            entity.integration == resolved.integration
                && entity.unique_id == resolved.unique_id
                && match (&entity.capabilities, &resolved.service) {
                    (Capabilities::Light(caps), Service::LightTurnOn(data)) => {
                        light_supports(caps, data).is_ok()
                    }
                    (Capabilities::Light(_), Service::LightTurnOff)
                    | (Capabilities::Switch(_), Service::SwitchTurnOn | Service::SwitchTurnOff) => {
                        true
                    }
                    _ => false,
                }
        })
    }

    /// Remembers what an entity was just told to be, until it reports back.
    pub fn record_command(&mut self, entity_id: &EntityId, service: &Service) {
        let on = match service {
            Service::LightTurnOn(_) | Service::SwitchTurnOn => true,
            Service::LightTurnOff | Service::SwitchTurnOff => false,
        };
        self.commanded.insert(entity_id.clone(), on);
    }

    /// Remembers a call's context for [`CALL_WINDOW`], so a state report can say it was caused
    /// by it.
    pub fn record_call(
        &mut self,
        integration: &IntegrationId,
        context_id: ContextId,
        now: Timestamp,
    ) {
        let calls = self.recent_calls.entry(integration.clone()).or_default();
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

/// A change Irori made itself, e.g. after an integration crashed.
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

/// A device's id: its integration and the integration's permanent handle for it, as a slug —
/// `esphome_30_83_98_ca_6a_08`. A pure function of the two, so it's the same after every restart
/// and whatever the device is called (ROADMAP D36).
///
/// A handle too long for an id keeps its beginning and gains a hash of the whole canonical
/// input (integration plus handle), so two long integrations that share a prefix — or two long
/// handles that start the same — still get different ids.
pub fn device_id_for(integration: &IntegrationId, unique_id: &UniqueId) -> DeviceId {
    let canonical = format!("{integration} {unique_id}");
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
    // A handle with nothing a slug can keep (`玄関`) would leave just the integration's name.
    let lossless_enough = slug.len() > integration.as_str().len();
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
/// an integration — and by the same rules as every other id, so `Kitchen` becomes `kitchen` and a
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
        device_id_for(&integration(), &uid(unique))
    }

    fn entity_key(unique: &str) -> SettingsKey {
        SettingsKey::new(integration(), uid(unique))
    }

    fn called(what: &str) -> irori_types::DeviceSettings {
        irori_types::DeviceSettings {
            added: false,
            name: Some(name(what)),
            description: None,
            area: Placement::Unsaid,
            ignored: false,
        }
    }

    fn in_room(area: &str) -> irori_types::DeviceSettings {
        irori_types::DeviceSettings {
            added: false,
            name: None,
            description: None,
            area: Placement::In(AreaId::try_from(area).expect("valid")),
            ignored: false,
        }
    }

    fn nowhere() -> irori_types::DeviceSettings {
        irori_types::DeviceSettings {
            added: false,
            name: None,
            description: None,
            area: Placement::Nowhere,
            ignored: false,
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
    /// calling it something else. The id is the integration and its permanent handle.
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

        // As after a restart: settings first, then the integration describes it again, under
        // a name its firmware has since changed.
        let mut restarted = Home::default();
        restarted.settle(Settings {
            devices: [(id.clone(), called("Reading lamp"))].into(),
            ..Settings::default()
        });
        restarted
            .describe_device(&integration(), device("lamp", "Lamp v2"))
            .expect("device");
        restarted
            .describe_entity(
                &integration(),
                &ALL,
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
        let id = |integration: &str, unique: &str| {
            device_id_for(
                &IntegrationId::try_from(integration).expect("valid"),
                &uid(unique),
            )
            .to_string()
        };
        assert_eq!(
            id("esphome", "30:83:98:CA:6A:08"),
            "esphome_30_83_98_ca_6a_08"
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
        // Two long integration ids that share a slug prefix, same handle: the hash covers both,
        // so truncation can't make them collide.
        let prefix = "i".repeat(60);
        let left = format!("{prefix}a");
        let right = format!("{prefix}b");
        assert_ne!(
            id(&left, "same-handle"),
            id(&right, "same-handle"),
            "hash must include the integration, not only the handle"
        );
    }

    /// Two handles that differ only in case would share an id. Refused with a reason, rather
    /// than numbered in whatever order they happened to be found.
    #[test]
    fn two_devices_whose_ids_would_collide_are_refused_rather_than_renumbered() {
        let mut home = Home::default();
        home.describe_device(&integration(), device("AB", "One"))
            .expect("first");
        let refused = home
            .describe_device(&integration(), device("ab", "Two"))
            .expect_err("same id");
        assert!(refused.0.contains("demo_ab"), "{}", refused.0);
    }

    /// One description, set by a person, like the name.
    fn ignoring(unique: &str) -> Settings {
        Settings {
            devices: [(
                key(unique),
                irori_types::DeviceSettings {
                    ignored: true,
                    ..Default::default()
                },
            )]
            .into(),
            ..Settings::default()
        }
    }

    /// Ignoring a device takes it and its entities out of the home. What its integration goes
    /// on saying is kept, not refused — so nothing is counted as a rejected report — and letting
    /// it back in restores it as it is now, not as it was when it left.
    #[test]
    fn an_ignored_device_leaves_the_home_and_comes_back_as_it_is_now() {
        let mut home = home_with_lamp();
        home.report_state(
            &integration(),
            report("lamp-light", Some(light(false, Some(10)))),
            &stamp(1),
        )
        .expect("report");

        let events = home.settle(ignoring("lamp"));
        assert!(home.devices.is_empty() && home.entities.is_empty() && home.states.is_empty());
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::DeviceRemoved { .. })),
            "{events:?}"
        );
        assert_eq!(home.held_devices().len(), 1);
        assert_eq!(home.held_devices()[0].name.as_str(), "Desk lamp");

        // The integration carries on as usual, and nothing it says is an error.
        home.describe_device(&integration(), device("lamp", "Desk lamp v2"))
            .expect("described while ignored");
        home.report_state(
            &integration(),
            report("lamp-light", Some(light(true, Some(200)))),
            &stamp(2),
        )
        .expect("reported while ignored");
        home.set_availability(
            &integration(),
            AvailabilityTarget::Device(uid("lamp")),
            Availability::Available,
            &stamp(2),
        )
        .expect("availability while ignored");
        assert!(home.devices.is_empty(), "still out of the home");
        assert!(
            home.resolve(&lamp_id(), Command::Toggle).is_err(),
            "an ignored entity can't be commanded"
        );

        home.settle(Settings::default());
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

    /// A device that's ignored before it's ever seen never enters the home at all.
    #[test]
    fn a_device_ignored_in_advance_never_arrives() {
        let mut home = Home::default();
        home.settle(ignoring("lamp"));
        home.describe_device(&integration(), device("lamp", "Desk lamp"))
            .expect("device");
        home.describe_entity(
            &integration(),
            &ALL,
            entity("lamp-light", None, Some("lamp"), dimmable()),
            &stamp(0),
        )
        .expect("entity");
        assert!(home.devices.is_empty() && home.entities.is_empty());
        assert_eq!(home.held_devices().len(), 1);

        // And an integration that removes it while ignored is taken at its word.
        home.remove_device(&integration(), &uid("lamp"))
            .expect("removed");
        assert!(home.held_devices().is_empty());
    }

    /// While Irori asks before adding, a new device waits outside the home until a person adds
    /// it, then joins as it is now. One already added joins as soon as it's found.
    #[test]
    fn a_new_device_waits_to_be_added_while_irori_asks() {
        let asking = |added: &[&str]| Settings {
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
        };
        let mut home = Home::default();
        home.settle(asking(&["plug"]));
        home.describe_device(&integration(), device("lamp", "Desk lamp"))
            .expect("lamp");
        home.describe_entity(
            &integration(),
            &ALL,
            entity("lamp-light", None, Some("lamp"), dimmable()),
            &stamp(0),
        )
        .expect("light");
        home.describe_device(&integration(), device("plug", "Plug"))
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
        assert_eq!((held[0].id.as_str(), held[0].why), ("demo_lamp", Held::New));

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
        home.describe_device(&integration(), device("lamp", "Desk lamp"))
            .expect("lamp");
        assert!(home.devices.contains_key(&key("lamp")));
        assert!(home.held_devices().is_empty());
    }

    /// Ignored stays ignored whether or not Irori asks; stopping asking lets every new device in.
    #[test]
    fn stopping_asking_lets_new_devices_in_but_not_ignored_ones() {
        let mut home = Home::default();
        home.settle(Settings {
            ask_before_adding: true,
            devices: [(
                key("plug"),
                irori_types::DeviceSettings {
                    ignored: true,
                    ..Default::default()
                },
            )]
            .into(),
            ..Settings::default()
        });
        home.describe_device(&integration(), device("lamp", "Desk lamp"))
            .expect("lamp");
        home.describe_device(&integration(), device("plug", "Plug"))
            .expect("plug");
        let mut why: Vec<_> = home
            .held_devices()
            .into_iter()
            .map(|held| held.why)
            .collect();
        why.sort_by_key(|why| format!("{why:?}"));
        assert_eq!(why, [Held::Ignored, Held::New]);

        home.settle(Settings {
            devices: [(
                key("plug"),
                irori_types::DeviceSettings {
                    ignored: true,
                    ..Default::default()
                },
            )]
            .into(),
            ..Settings::default()
        });
        assert!(home.devices.contains_key(&key("lamp")));
        assert_eq!(home.held_devices().len(), 1);
        assert_eq!(home.held_devices()[0].why, Held::Ignored);
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

    /// The integration keeps describing the device while a person's name is in force. Its name
    /// must not win back, and the new manufacturer or firmware version must still land.
    #[test]
    fn an_integration_that_describes_a_renamed_device_again_doesnt_rename_it_back() {
        let mut home = home_with_lamp();
        home.settle(Settings {
            devices: [(key("lamp"), called("Reading lamp"))].into(),
            ..Settings::default()
        });

        let mut again = device("lamp", "Desk lamp");
        again.sw_version = Some("2026.9.0".to_owned());
        let events = home.describe_device(&integration(), again).expect("again");

        assert_eq!(device_named(&home, "demo_lamp"), "Reading lamp");
        assert_eq!(
            home.devices[&DeviceId::try_from("demo_lamp").expect("valid")].sw_version,
            Some("2026.9.0".to_owned()),
            "what the integration is entitled to say still lands"
        );
        assert_eq!(events.len(), 1, "only the device changed: {events:?}");
    }

    /// An entity named by its integration, or by a person, has a name of its own and keeps it
    /// when the device is renamed.
    #[test]
    fn only_entities_without_a_name_of_their_own_follow_the_device() {
        let mut home = home_with_lamp();
        home.describe_entity(
            &integration(),
            &ALL,
            entity(
                "lamp-power",
                Some("Power"),
                Some("lamp"),
                Capabilities::Sensor(SensorCapabilities {
                    value_type: SensorValueType::Number,
                    device_class: None,
                    unit: None,
                    state_class: None,
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
            "a name the integration gave the entity is its own too"
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
        home.describe_device(&integration(), described)
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
        home.describe_device(&integration(), described)
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
        home.describe_device(&integration(), described)
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

    /// Settings are read before any integration starts, so the common case is a device arriving
    /// into a home that already has an opinion about it. It should never appear under its old
    /// name, not even for an instant.
    #[test]
    fn a_device_that_arrives_later_is_named_and_placed_as_it_appears() {
        let mut home = Home::default();
        home.settle(Settings {
            ask_before_adding: false,
            floors: Vec::new(),
            areas: vec![area("study", "Study")],
            devices: [(
                key("lamp"),
                irori_types::DeviceSettings {
                    added: false,
                    name: Some(name("Reading lamp")),
                    description: None,
                    area: Placement::In(AreaId::try_from("study").expect("valid")),
                    ignored: false,
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
            .describe_device(&integration(), device("lamp", "Desk lamp"))
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
            &integration(),
            &ALL,
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
            home.describe_device(&integration(), device(unique, "玄関 light"))
                .expect("device");
        }
        let ids: Vec<_> = home.devices().map(|d| d.id.to_string()).collect();
        assert_eq!(ids, ["demo_a", "demo_b"]);
    }

    /// An entity's id is its device's id and the name its integration gave it: never a name a
    /// person chose, so no rename can leave an id that says something else.
    #[test]
    fn entities_get_ids_from_their_devices_id_and_their_own_reported_name() {
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
        let state = home.state(&lamp_id()).expect("state");
        assert_eq!(state.state, None);
        // Irori forgot it; the device didn't report it.
        assert!(matches!(state.context.origin, Origin::System));

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
        assert!(err.0.contains("in the last 5 minutes"), "{err}");

        home.record_call(&integration(), call.clone(), stamp(1).now);
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
    fn calls_can_be_confirmed_for_five_minutes_even_after_many_others() {
        let mut home = home_with_lamp();
        let call = stamp(100).context_id;
        home.record_call(&integration(), call.clone(), stamp(100).now);
        for i in 0..200 {
            home.record_call(&integration(), stamp(101 + i).context_id, stamp(101).now);
        }
        let mut confirmed = report("lamp-light", Some(light(false, None)));
        confirmed.caused_by = Some(call.clone());
        home.report_state(&integration(), confirmed.clone(), &stamp(100 + 300))
            .expect("within five minutes, after 200 other calls");

        let mut late = confirmed;
        late.state = Some(light(true, None));
        let err = home
            .report_state(&integration(), late, &stamp(100 + 301))
            .expect_err("too late");
        assert!(err.0.contains("in the last 5 minutes"), "{err}");
    }

    #[test]
    fn the_call_window_survives_a_clock_correction() {
        let mut home = home_with_lamp();
        let call = stamp(1_000).context_id;
        home.record_call(&integration(), call.clone(), stamp(1_000).now);
        let mut confirmed = report("lamp-light", Some(light(false, None)));
        confirmed.caused_by = Some(call);
        // The clock stepped back a minute between the call and its confirmation.
        home.report_state(&integration(), confirmed.clone(), &stamp(940))
            .expect("still within the window");
        // A clock a day behind isn't a correction; that call isn't recent.
        let err = home
            .report_state(&integration(), confirmed, &stamp(1_000 - 86_400))
            .expect_err("far outside the window");
        assert!(err.0.contains("in the last 5 minutes"), "{err}");
    }

    #[test]
    fn toggle_follows_the_last_command_until_the_device_reports() {
        let mut home = home_with_lamp();
        home.report_state(
            &integration(),
            report("lamp-light", Some(light(false, None))),
            &stamp(1),
        )
        .expect("fits");

        // First toggle: it's off, so turn it on.
        let first = home.resolve(&lamp_id(), Command::Toggle).expect("light");
        assert_eq!(first.service, Service::LightTurnOn(LightTurnOn::default()));
        home.record_command(&lamp_id(), &first.service);

        // Second toggle before the lamp has reported: it must undo the first, not repeat it.
        let second = home.resolve(&lamp_id(), Command::Toggle).expect("light");
        assert_eq!(second.service, Service::LightTurnOff);
        home.record_command(&lamp_id(), &second.service);

        // Once the lamp reports, its own word counts again.
        home.report_state(
            &integration(),
            report("lamp-light", Some(light(true, None))),
            &stamp(2),
        )
        .expect("fits");
        assert_eq!(
            home.resolve(&lamp_id(), Command::Toggle)
                .expect("light")
                .service,
            Service::LightTurnOff
        );
    }

    #[test]
    fn a_call_is_refused_when_the_entity_changed_underneath_it() {
        let mut home = home_with_lamp();
        let resolved = home.resolve(&lamp_id(), Command::Toggle).expect("light");
        assert!(home.still_dispatchable(&lamp_id(), &resolved));

        // The lamp is re-described without dimming, and the brightness it was asked for is gone.
        let bright = home
            .resolve(
                &lamp_id(),
                Command::TurnOn(LightTurnOn {
                    brightness: Some(200),
                    ..LightTurnOn::default()
                }),
            )
            .expect("dimmable for now");
        home.describe_entity(
            &integration(),
            &ALL,
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
        home.remove_entity(&integration(), &uid("lamp-light"))
            .expect("exists");
        assert!(!home.still_dispatchable(&lamp_id(), &resolved));
    }

    #[test]
    fn describing_an_entity_again_counts_as_hearing_from_it() {
        let mut home = home_with_lamp();
        home.report_state(
            &integration(),
            report("lamp-light", Some(light(true, None))),
            &stamp(10),
        )
        .expect("fits");
        home.mark_unavailable(&integration(), &stamp(20));
        assert_eq!(
            home.state(&lamp_id()).expect("state").last_reported,
            stamp(10).now
        );

        // The integration restarts and describes it again: back online, and heard from.
        home.describe_entity(
            &integration(),
            &ALL,
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
        home.record_call(&integration(), call.clone(), stamp(1).now);
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
            "`light.demo_lamp` supports color temperatures from 2700 to 6500 K, not 2000 K"
        );

        let unknown = EntityId::try_from("light.nope").expect("valid");
        assert!(matches!(
            home.resolve(&unknown, Command::TurnOff),
            Err(CallError::UnknownEntity(_))
        ));
    }
}
