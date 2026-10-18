//! What one person can see and use (`docs/specs/access.md` §1).
//!
//! The answer for a person held to some rooms is a set of the things in them, worked out when
//! the registry or what was said about it changes, so asking is a lookup. This is the only place
//! that decides it: the page, the command line, the assistant and the engines all ask here.
//!
//! A device is in the room a person **saved** for it. One that only says where it would like to
//! be (`suggested_area`) is placed by name for the owner's view and doesn't count here: a device
//! doesn't join somebody's room by being plugged in. A thing with no device and no room of its
//! own (a helper, a computed value) is in no room.

use std::collections::BTreeSet;
use std::sync::Arc;

use irori_types::{
    Area, AreaId, Device, DeviceId, Entity, EntityId, EntityState, Floor, FloorId, Floorplan,
};

use crate::home::Home;

/// The things in some rooms.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Rooms {
    areas: BTreeSet<AreaId>,
    floors: BTreeSet<FloorId>,
    devices: BTreeSet<DeviceId>,
    entities: BTreeSet<EntityId>,
}

impl Rooms {
    #[cfg(test)]
    pub(crate) fn clone_of(other: &Rooms) -> Rooms {
        Rooms {
            areas: other.areas.clone(),
            floors: other.floors.clone(),
            devices: other.devices.clone(),
            entities: other.entities.clone(),
        }
    }

    /// What is in `wanted` right now. A room that isn't there is skipped: a name in `users.toml`
    /// for a room not made yet means nothing until it is.
    pub(crate) fn of(home: &Home, wanted: &[AreaId]) -> Self {
        let settings = home.settings();
        let areas: BTreeSet<AreaId> = wanted
            .iter()
            .filter(|id| settings.area(id).is_some())
            .cloned()
            .collect();
        let floors = areas
            .iter()
            .filter_map(|id| settings.area(id)?.floor_id.clone())
            .collect();
        let devices: BTreeSet<DeviceId> = home
            .devices()
            .filter(|device| {
                settings
                    .devices
                    .get(&device.id)
                    .and_then(|saved| saved.area.area())
                    .is_some_and(|room| areas.contains(room))
            })
            .map(|device| device.id.clone())
            .collect();
        // An entity a person put in a room of its own is there; any other is where its device is.
        let entities = home
            .entities()
            .filter(|entity| match &entity.area_id {
                Some(room) => areas.contains(room),
                None => entity
                    .device_id
                    .as_ref()
                    .is_some_and(|device| devices.contains(device)),
            })
            .map(|entity| entity.id.clone())
            .collect();
        Self {
            areas,
            floors,
            devices,
            entities,
        }
    }
}

/// How much of the home a person has.
#[derive(Debug, Clone)]
pub enum Reach {
    /// All of it: an owner, a user with no rooms listed, and anybody in an open home.
    Whole,
    /// What is in some rooms, and nothing else.
    Rooms(Arc<Rooms>),
}

impl Reach {
    /// Nothing at all: somebody who isn't anybody. Where a request has no one behind it, this is
    /// what it gets, so a missing answer can't turn into the whole home.
    pub fn nothing() -> Self {
        Self::Rooms(Arc::new(Rooms::default()))
    }

    pub fn is_whole(&self) -> bool {
        matches!(self, Self::Whole)
    }

    pub fn sees_entity(&self, id: &EntityId) -> bool {
        match self {
            Self::Whole => true,
            Self::Rooms(rooms) => rooms.entities.contains(id),
        }
    }

    pub fn sees_device(&self, id: &DeviceId) -> bool {
        match self {
            Self::Whole => true,
            Self::Rooms(rooms) => rooms.devices.contains(id),
        }
    }

    pub fn sees_area(&self, id: &AreaId) -> bool {
        match self {
            Self::Whole => true,
            Self::Rooms(rooms) => rooms.areas.contains(id),
        }
    }

    /// A floor is theirs to see when one of their rooms is on it.
    pub fn sees_floor(&self, id: &FloorId) -> bool {
        match self {
            Self::Whole => true,
            Self::Rooms(rooms) => rooms.floors.contains(id),
        }
    }
}

/// What is shown of a list. Each takes the whole list and gives back what the person has; for
/// [`Reach::Whole`] that is the list itself, untouched, so the owner pays for nothing.
impl Reach {
    pub fn devices(&self, mut devices: Vec<Device>) -> Vec<Device> {
        if self.is_whole() {
            return devices;
        }
        devices.retain(|device| self.sees_device(&device.id));
        for device in &mut devices {
            // What it says about a hub or a room it isn't in would name a thing the person has
            // no room for. They see the device; the rest of the wiring is the owner's.
            if device
                .via_device_id
                .as_ref()
                .is_some_and(|via| !self.sees_device(via))
            {
                device.via_device_id = None;
            }
            device.suggested_area = None;
        }
        devices
    }

    pub fn entities(&self, mut entities: Vec<Entity>) -> Vec<Entity> {
        if !self.is_whole() {
            entities.retain(|entity| self.sees_entity(&entity.id));
        }
        entities
    }

    pub fn states(&self, mut states: Vec<EntityState>) -> Vec<EntityState> {
        if !self.is_whole() {
            states.retain(|state| self.sees_entity(&state.entity_id));
        }
        states
    }

    pub fn areas(&self, mut areas: Vec<Area>) -> Vec<Area> {
        if !self.is_whole() {
            areas.retain(|area| self.sees_area(&area.id));
        }
        areas
    }

    pub fn floors(&self, mut floors: Vec<Floor>) -> Vec<Floor> {
        if !self.is_whole() {
            floors.retain(|floor| self.sees_floor(&floor.id));
        }
        floors
    }

    /// The plan with only their floors, their rooms' shapes, and their things on it. The walls of
    /// a floor they have a room on stay: a wall is structure, not a thing in the home, and a room
    /// drawn with no walls round it would be a label floating on a page.
    pub fn floorplan(&self, plan: Floorplan) -> Floorplan {
        if self.is_whole() {
            return plan;
        }
        let mut floors = plan.floors;
        floors.retain(|id, _| self.sees_floor(id));
        for level in floors.values_mut() {
            level.areas.retain(|placed| self.sees_area(&placed.area));
            level
                .devices
                .retain(|placed| match (&placed.device, &placed.entity) {
                    (Some(device), _) => self.sees_device(device),
                    (None, Some(entity)) => self.sees_entity(entity),
                    (None, None) => false,
                });
        }
        Floorplan { floors }
    }
}
