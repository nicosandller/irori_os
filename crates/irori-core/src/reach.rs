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

use irori_types::{AreaId, DeviceId, EntityId, FloorId};

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
