//! What a person has said about their home, as opposed to what the integrations report.
//!
//! These are the contents of the config directory once it has been read and checked
//! (`docs/specs/config.md`). They live here, rather than in `irori-config`, so the core can be
//! told about them without depending on a file format — and so a tool that only wants to read
//! config doesn't have to pull in the core.

use std::collections::BTreeMap;

use crate::{Area, AreaId, IdError, IntegrationId, Name, UniqueId};

/// Everything the config directory says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// The rooms of the home, ordered by id.
    pub areas: Vec<Area>,
    pub devices: BTreeMap<SettingsKey, DeviceSettings>,
    pub entities: BTreeMap<SettingsKey, EntitySettings>,
}

impl Settings {
    pub fn area(&self, id: &AreaId) -> Option<&Area> {
        self.areas.iter().find(|area| &area.id == id)
    }

    /// The area with this name, ignoring case and surrounding space — how a `suggested_area`
    /// finds its room (`docs/specs/config.md` §5).
    pub fn area_named(&self, name: &Name) -> Option<&Area> {
        let wanted = name.as_str().trim().to_lowercase();
        self.areas
            .iter()
            .find(|area| area.name.as_str().trim().to_lowercase() == wanted)
    }
}

/// Where a device is, as far as a person has said.
///
/// Three states, not two, because "nobody has said" and "it isn't in a room" are different
/// answers. A device whose firmware suggests a room needs its owner to be able to say *no* —
/// otherwise choosing "not in a room" would only clear the setting, let the suggestion back in,
/// and put the device straight back where it was.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Placement {
    /// Nobody has said. The device's own `suggested_area` may stand in.
    #[default]
    Unsaid,
    /// Deliberately in no room, whatever the device suggests.
    Nowhere,
    In(AreaId),
}

impl Placement {
    pub fn area(&self) -> Option<&AreaId> {
        match self {
            Placement::In(area) => Some(area),
            _ => None,
        }
    }

    /// Whether a person has answered at all.
    pub fn is_unsaid(&self) -> bool {
        matches!(self, Placement::Unsaid)
    }
}

/// What a person has said about one device.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceSettings {
    /// What to call it instead of the name its integration reports.
    pub name: Option<Name>,
    /// Which room it's in, if that's been decided.
    pub area: Placement,
}

impl DeviceSettings {
    /// Whether this says anything at all. An entry that says nothing is not written out.
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.area.is_unsaid()
    }
}

/// What a person has said about one entity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntitySettings {
    /// What to call it instead of the name its integration reports — or instead of following
    /// its device's name, for an entity that was described without one.
    pub name: Option<Name>,
}

impl EntitySettings {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
    }
}

/// What a setting is attached to: an integration and its own permanent handle for the thing
/// (`docs/specs/config.md` §4).
///
/// Not a `DeviceId` or `EntityId`, which are derived from names — keying on those would mean a
/// rename could lose the setting that caused it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SettingsKey {
    pub integration: IntegrationId,
    pub unique_id: UniqueId,
}

impl SettingsKey {
    pub fn new(integration: IntegrationId, unique_id: UniqueId) -> Self {
        Self {
            integration,
            unique_id,
        }
    }
}

/// `<integration>/<unique_id>`, which is how it is written in the config files.
impl std::fmt::Display for SettingsKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.integration, self.unique_id)
    }
}

impl std::str::FromStr for SettingsKey {
    type Err = IdError;

    /// Splits at the *first* `/`: an integration id is a slug and can't contain one, but a
    /// unique id is whatever the integration chose and often can.
    fn from_str(value: &str) -> Result<Self, IdError> {
        let (integration, unique_id) = value.split_once('/').unwrap_or((value, ""));
        Ok(Self {
            integration: integration.parse()?,
            unique_id: unique_id.parse()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> SettingsKey {
        s.parse().expect("a valid settings key")
    }

    fn area(id: &str, name: &str) -> Area {
        Area {
            id: id.parse().expect("a valid area id"),
            name: name.parse().expect("a valid name"),
            floor_id: None,
        }
    }

    #[test]
    fn a_key_survives_a_round_trip_through_its_written_form() {
        for written in [
            "esphome/34:98:7a:2b:09:00",
            "esphome/34:98:7a:2b:09:00-binary_sensor-1594977085",
            "demo/lamp",
            // A unique id with slashes of its own: MQTT topics look like this.
            "mqtt/home/kitchen/lamp",
        ] {
            assert_eq!(key(written).to_string(), written);
        }
    }

    #[test]
    fn a_key_without_both_halves_is_refused() {
        for bad in ["esphome", "esphome/", "/lamp", "", "Esphome/lamp"] {
            assert!(bad.parse::<SettingsKey>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_suggested_area_finds_its_room_however_it_is_capitalised() {
        let settings = Settings {
            areas: vec![area("kitchen", "Kitchen"), area("hall", "Hall")],
            ..Settings::default()
        };
        let named = |name: &str| {
            settings
                .area_named(&name.parse().expect("a valid name"))
                .map(|area| area.id.to_string())
        };

        assert_eq!(named("Kitchen").as_deref(), Some("kitchen"));
        assert_eq!(named("kitchen").as_deref(), Some("kitchen"));
        assert_eq!(named("KITCHEN").as_deref(), Some("kitchen"));
        assert_eq!(named("Pantry"), None);
    }
}
