//! How the Devices and Entities lists are grouped: by the protocol a thing came through, the
//! area it's in, who made it, the device it's part of, or the kind of thing it is.
//!
//! All of it is what a thing *is*, never what it's reporting, so two homes that differ only in
//! readings group identically — which is what lets the lists hold still while readings arrive.

use std::collections::BTreeMap;

use irori_types::{Device, Entity, EntityKind};

use crate::api::Home;

/// What the device table is grouped by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevicesBy {
    /// The protocol it came through. The default: it's how devices arrive.
    Protocol,
    Area,
    Make,
}

impl DevicesBy {
    pub const ALL: [DevicesBy; 3] = [DevicesBy::Protocol, DevicesBy::Area, DevicesBy::Make];

    pub fn key(self) -> &'static str {
        match self {
            DevicesBy::Protocol => "protocol",
            DevicesBy::Area => "area",
            DevicesBy::Make => "make",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DevicesBy::Protocol => "Protocol",
            DevicesBy::Area => "Area",
            DevicesBy::Make => "Make",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|by| by.key() == key)
    }
}

/// What the entity table is grouped by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntitiesBy {
    /// The device it's part of. The default: it's where a person looks for it.
    Device,
    Area,
    Kind,
    Protocol,
}

impl EntitiesBy {
    pub const ALL: [EntitiesBy; 4] = [
        EntitiesBy::Device,
        EntitiesBy::Area,
        EntitiesBy::Kind,
        EntitiesBy::Protocol,
    ];

    pub fn key(self) -> &'static str {
        match self {
            EntitiesBy::Device => "device",
            EntitiesBy::Area => "area",
            EntitiesBy::Kind => "kind",
            EntitiesBy::Protocol => "protocol",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EntitiesBy::Device => "Device",
            EntitiesBy::Area => "Area",
            EntitiesBy::Kind => "Kind",
            EntitiesBy::Protocol => "Protocol",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|by| by.key() == key)
    }
}

/// One group of a list: its heading and the rows under it.
#[derive(Debug, Clone, PartialEq)]
pub struct Section<Row> {
    /// What remembers this group being folded. Says which grouping it belongs to
    /// (`area:kitchen`), so a protocol and an area that share an id don't fold together.
    pub key: String,
    pub label: String,
    /// What goes after the label, quieter: an area's floor, a device's make and model.
    pub note: Option<String>,
    /// The protocol whose icon heads the group, where a group has one.
    pub icon: Option<ProtocolIcon>,
    pub rows: Vec<Row>,
}

/// A protocol and whether its extension ships an icon, which is all `devices::icon` needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolIcon {
    pub protocol: String,
    pub has_icon: bool,
}

/// A device as its row shows it, apart from its battery, which is a reading.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceLine {
    pub device: Device,
    pub area: Option<String>,
    pub icon: ProtocolIcon,
    pub entities: usize,
}

/// An entity as its row shows it, apart from its state.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityLine {
    pub entity: Entity,
    /// The device it's part of: its id, for the way there, and its name.
    pub device: Option<(String, String)>,
}

/// The device table: every device matching `needle`, grouped by `by`. Groups go by name with
/// the "none of these" group last, and devices by name inside each.
pub fn devices(home: &Home, needle: &str, by: DevicesBy) -> Vec<Section<DeviceLine>> {
    let needle = needle.trim().to_lowercase();
    let mut sections: BTreeMap<Place, Section<DeviceLine>> = BTreeMap::new();
    for device in home
        .devices
        .iter()
        .filter(|device| super::matches_device(home, device, &needle))
    {
        let heading = match by {
            DevicesBy::Protocol => protocol_heading(home, device.protocol.as_str()),
            DevicesBy::Area => area_heading(home, device.area_id.as_ref().map(|id| id.as_str())),
            DevicesBy::Make => make_heading(device.manufacturer.as_deref()),
        };
        let line = DeviceLine {
            device: device.clone(),
            area: home.area_of(device),
            icon: protocol_icon(home, device.protocol.as_str()),
            entities: home
                .entities
                .iter()
                .filter(|entity| entity.device_id.as_ref() == Some(&device.id))
                .count(),
        };
        heading.into_section(&mut sections).rows.push(line);
    }
    sections
        .into_values()
        .map(|mut section| {
            section.rows.sort_by(|a, b| {
                (&a.device.name, &a.device.id).cmp(&(&b.device.name, &b.device.id))
            });
            section
        })
        .collect()
}

/// The entity table: every entity matching `needle`, grouped by `by`. Inside a group, what a
/// device is for comes first, then its settings and diagnostics.
pub fn entities(home: &Home, needle: &str, by: EntitiesBy) -> Vec<Section<EntityLine>> {
    let needle = needle.trim().to_lowercase();
    let devices: BTreeMap<_, _> = home.devices.iter().map(|d| (&d.id, d)).collect();
    let mut sections: BTreeMap<Place, Section<EntityLine>> = BTreeMap::new();
    for entity in &home.entities {
        let device = entity.device_id.as_ref().and_then(|id| devices.get(id));
        // Its own area if it has one, else its device's: a sensor on a long lead can be in a
        // different room from the box it's wired to.
        let area = entity
            .area_id
            .as_ref()
            .or_else(|| device.and_then(|device| device.area_id.as_ref()));
        let area_name = area
            .and_then(|id| home.area(id))
            .map(|area| area.name.to_string())
            .unwrap_or_default();
        // A device's name is part of what its entities are called in conversation ("the lamp in
        // the hallway sensor"), so typing it keeps the whole device. Area and make too: the
        // same search box is used on the Devices table.
        let matches = needle.is_empty()
            || entity.id.to_string().to_lowercase().contains(&needle)
            || entity.name.as_str().to_lowercase().contains(&needle)
            || area_name.to_lowercase().contains(&needle)
            || device.is_some_and(|device| super::matches_device(home, device, &needle));
        if !matches {
            continue;
        }
        let heading = match by {
            EntitiesBy::Device => device_heading(home, device.copied()),
            EntitiesBy::Area => area_heading(home, area.map(|id| id.as_str())),
            EntitiesBy::Kind => kind_heading(entity.capabilities.kind()),
            EntitiesBy::Protocol => protocol_heading(home, entity.protocol.as_str()),
        };
        let line = EntityLine {
            entity: entity.clone(),
            device: device.map(|device| (device.id.to_string(), device.name.to_string())),
        };
        heading.into_section(&mut sections).rows.push(line);
    }
    sections
        .into_values()
        .map(|mut section| {
            section.rows.sort_by(|a, b| {
                let (a, b) = (&a.entity, &b.entity);
                (a.entity_category, &a.name, &a.id).cmp(&(b.entity_category, &b.name, &b.id))
            });
            section
        })
        .collect()
}

/// How many rows the sections hold between them.
pub fn count<Row>(sections: &[Section<Row>]) -> usize {
    sections.iter().map(|section| section.rows.len()).sum()
}

/// Where a group sorts: real groups by name, then by key so two that share a name keep a
/// stable order, and the "none of these" group after all of them.
type Place = (bool, String, String);

/// A group's heading before it has any rows.
struct Heading {
    /// `None` for the group of things that have no protocol, area, make or device.
    id: Option<String>,
    kind: &'static str,
    label: String,
    note: Option<String>,
    icon: Option<ProtocolIcon>,
}

impl Heading {
    fn into_section<Row>(self, sections: &mut BTreeMap<Place, Section<Row>>) -> &mut Section<Row> {
        let key = format!("{}:{}", self.kind, self.id.as_deref().unwrap_or_default());
        let place = (self.id.is_none(), self.label.to_lowercase(), key.clone());
        sections.entry(place).or_insert_with(|| Section {
            key,
            label: self.label,
            note: self.note,
            icon: self.icon,
            rows: Vec::new(),
        })
    }
}

fn protocol_icon(home: &Home, protocol: &str) -> ProtocolIcon {
    ProtocolIcon {
        protocol: protocol.to_owned(),
        has_icon: home
            .extensions
            .iter()
            .any(|(id, extension)| id.as_str() == protocol && extension.has_icon),
    }
}

fn protocol_heading(home: &Home, protocol: &str) -> Heading {
    let name = home
        .extensions
        .iter()
        .find(|(id, _)| id.as_str() == protocol)
        .map(|(_, extension)| extension.name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| protocol.to_owned());
    Heading {
        id: Some(protocol.to_owned()),
        kind: "protocol",
        label: name,
        note: None,
        icon: Some(protocol_icon(home, protocol)),
    }
}

fn area_heading(home: &Home, area: Option<&str>) -> Heading {
    let found = area.and_then(|id| home.areas.iter().find(|area| area.id.as_str() == id));
    match found {
        Some(area) => Heading {
            id: Some(area.id.to_string()),
            kind: "area",
            label: area.name.to_string(),
            note: area.floor_id.as_ref().and_then(|floor| {
                home.floors
                    .iter()
                    .find(|known| &known.id == floor)
                    .map(|floor| floor.name.to_string())
            }),
            icon: None,
        },
        // In no area, or in one that has since been removed.
        None => Heading {
            id: None,
            kind: "area",
            label: "Unassigned".to_owned(),
            note: None,
            icon: None,
        },
    }
}

/// Makes are whatever the device said, so "IKEA", "Ikea " and "ikea" are one group, called by
/// whichever spelling turned up first.
fn make_heading(make: Option<&str>) -> Heading {
    match make.map(str::trim).filter(|make| !make.is_empty()) {
        Some(make) => Heading {
            id: Some(make.to_lowercase()),
            kind: "make",
            label: make.to_owned(),
            note: None,
            icon: None,
        },
        None => Heading {
            id: None,
            kind: "make",
            label: "Unknown make".to_owned(),
            note: None,
            icon: None,
        },
    }
}

fn device_heading(home: &Home, device: Option<&Device>) -> Heading {
    match device {
        Some(device) => {
            let model = [device.manufacturer.as_deref(), device.model.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
            Heading {
                id: Some(device.id.to_string()),
                kind: "device",
                label: device.name.to_string(),
                note: (!model.is_empty()).then_some(model),
                icon: Some(protocol_icon(home, device.protocol.as_str())),
            }
        }
        None => Heading {
            id: None,
            kind: "device",
            label: "Not on a device".to_owned(),
            note: None,
            icon: None,
        },
    }
}

fn kind_heading(kind: EntityKind) -> Heading {
    Heading {
        id: Some(format!("{kind:?}").to_lowercase()),
        kind: "kind",
        label: kind_label(kind).to_owned(),
        note: None,
        icon: None,
    }
}

/// A kind, as the heading of everything of that kind.
fn kind_label(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Light => "Lights",
        EntityKind::Switch => "Switches",
        EntityKind::Sensor => "Sensors",
        EntityKind::BinarySensor => "Binary sensors",
        EntityKind::Number => "Numbers",
        EntityKind::Select => "Selects",
        EntityKind::Text => "Text",
        EntityKind::Button => "Buttons",
        EntityKind::Event => "Events",
        EntityKind::Cover => "Covers",
        EntityKind::Lock => "Locks",
        EntityKind::Fan => "Fans",
        EntityKind::Valve => "Valves",
        EntityKind::Siren => "Sirens",
        EntityKind::Climate => "Climate",
        EntityKind::WaterHeater => "Water heaters",
        EntityKind::Humidifier => "Humidifiers",
        EntityKind::MediaPlayer => "Media players",
    }
}

/// The folded groups as they were saved. Keys from before groups could be anything but
/// protocols have no grouping in front; they were protocols.
pub fn folded_from(saved: &str) -> std::collections::BTreeSet<String> {
    saved
        .split(',')
        .filter(|key| !key.is_empty())
        .map(|key| {
            if key.contains(':') {
                key.to_owned()
            } else {
                format!("protocol:{key}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use irori_types::{Area, BinarySensorCapabilities, Capabilities, Floor};

    use super::*;

    fn device(id: &str, protocol: &str, make: Option<&str>, area: Option<&str>) -> Device {
        Device {
            id: id.parse().expect("valid"),
            protocol: protocol.parse().expect("valid"),
            unique_id: id.parse().expect("valid"),
            name: id.parse().expect("valid"),
            description: None,
            manufacturer: make.map(str::to_owned),
            model: None,
            sw_version: None,
            hw_version: None,
            area_id: area.map(|area| area.parse().expect("valid")),
            suggested_area: None,
            via_device_id: None,
        }
    }

    fn entity(id: &str, device: Option<&str>, area: Option<&str>) -> Entity {
        Entity {
            id: id.parse().expect("valid"),
            protocol: "zigbee".parse().expect("valid"),
            unique_id: id.replace('.', "-").parse().expect("valid"),
            name: id.replace('.', " ").parse().expect("valid"),
            device_id: device.map(|device| device.parse().expect("valid")),
            area_id: area.map(|area| area.parse().expect("valid")),
            capabilities: Capabilities::BinarySensor(BinarySensorCapabilities {
                device_class: None,
            }),
            entity_category: None,
        }
    }

    fn home() -> Home {
        Home {
            devices: vec![
                device("lamp", "zigbee", Some("IKEA"), Some("hall")),
                device("bulb", "zigbee", Some("ikea "), None),
                device("radar", "esphome", None, Some("hall")),
            ],
            entities: vec![
                entity("binary_sensor.lamp_on", Some("lamp"), None),
                entity("binary_sensor.radar_far", Some("radar"), Some("attic")),
                entity("binary_sensor.loose", None, None),
            ],
            areas: vec![
                Area {
                    id: "hall".parse().expect("valid"),
                    name: "Hall".parse().expect("valid"),
                    floor_id: Some("ground".parse().expect("valid")),
                },
                Area {
                    id: "attic".parse().expect("valid"),
                    name: "Attic".parse().expect("valid"),
                    floor_id: None,
                },
            ],
            floors: vec![Floor {
                id: "ground".parse().expect("valid"),
                name: "Ground floor".parse().expect("valid"),
                level: 0,
            }],
            ..Home::default()
        }
    }

    fn names<Row>(sections: &[Section<Row>]) -> Vec<(&str, usize)> {
        sections
            .iter()
            .map(|section| (section.label.as_str(), section.rows.len()))
            .collect()
    }

    #[test]
    fn devices_group_by_protocol_area_and_make() {
        let home = home();
        assert_eq!(
            names(&devices(&home, "", DevicesBy::Protocol)),
            [("esphome", 1), ("zigbee", 2)]
        );
        let by_area = devices(&home, "", DevicesBy::Area);
        assert_eq!(names(&by_area), [("Hall", 2), ("Unassigned", 1)]);
        assert_eq!(by_area[0].note.as_deref(), Some("Ground floor"));
        assert_eq!(by_area[0].key, "area:hall");
        // One make however it was spelled, and the devices that don't say come last.
        assert_eq!(
            names(&devices(&home, "", DevicesBy::Make)),
            [("IKEA", 2), ("Unknown make", 1)]
        );
    }

    #[test]
    fn a_device_row_counts_its_entities_and_names_its_area() {
        let by_protocol = devices(&home(), "lamp", DevicesBy::Protocol);
        let lamp = &by_protocol[0].rows[0];
        assert_eq!(lamp.entities, 1);
        assert_eq!(lamp.area.as_deref(), Some("Hall"));
    }

    #[test]
    fn entities_group_by_device_area_kind_and_protocol() {
        let home = home();
        assert_eq!(
            names(&entities(&home, "", EntitiesBy::Device)),
            [("lamp", 1), ("radar", 1), ("Not on a device", 1)]
        );
        // The radar's far sensor says its own area; the lamp's takes its device's.
        assert_eq!(
            names(&entities(&home, "", EntitiesBy::Area)),
            [("Attic", 1), ("Hall", 1), ("Unassigned", 1)]
        );
        assert_eq!(
            names(&entities(&home, "", EntitiesBy::Kind)),
            [("Binary sensors", 3)]
        );
        assert_eq!(
            names(&entities(&home, "", EntitiesBy::Protocol)),
            [("zigbee", 3)]
        );
    }

    /// The Devices and Entities views share one search box. Typing an area or a make has to
    /// keep the entity, not look like a broken filter, and something that isn't there finds
    /// nothing.
    #[test]
    fn filtering_entities_matches_area_and_make() {
        let home = home();
        let found = |needle| count(&entities(&home, needle, EntitiesBy::Device));
        assert_eq!(found("ikea"), 1);
        // The device's area, and the entity's own.
        assert_eq!(found("hall"), 2);
        assert_eq!(found("attic"), 1);
        assert_eq!(found("nowhere"), 0);
    }

    #[test]
    fn folds_saved_before_groupings_were_protocols() {
        let folded = folded_from("zigbee,area:hall,,make:ikea");
        assert_eq!(
            folded.into_iter().collect::<Vec<_>>(),
            ["area:hall", "make:ikea", "protocol:zigbee"]
        );
    }
}
