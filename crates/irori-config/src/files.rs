//! The TOML shape of each file, and the translation to and from [`Settings`].
//!
//! The files are deliberately not a mirror of the Rust types: a settings key is one quoted
//! string in TOML (`"esphome/34:98:7a:2b:09:00"`) and a pair of ids in Rust, and an area's id is
//! the table key rather than a field. Keeping the translation here means the rest of Irori never
//! sees the file format.

use std::collections::BTreeMap;

use irori_types::{
    Area, AreaId, DeviceSettings, EntitySettings, FloorId, Name, Settings, SettingsKey,
};
use serde::{Deserialize, Serialize};

/// A file this crate reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum File {
    Areas,
    Devices,
    Entities,
}

impl File {
    pub const ALL: [File; 3] = [File::Areas, File::Devices, File::Entities];

    pub fn name(self) -> &'static str {
        match self {
            File::Areas => "areas.toml",
            File::Devices => "devices.toml",
            File::Entities => "entities.toml",
        }
    }
}

impl std::fmt::Display for File {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AreasFile {
    #[serde(default)]
    areas: BTreeMap<AreaId, RawArea>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawArea {
    name: Name,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    floor: Option<FloorId>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DevicesFile {
    #[serde(default)]
    devices: BTreeMap<String, RawDevice>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDevice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    area: Option<AreaId>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntitiesFile {
    #[serde(default)]
    entities: BTreeMap<String, RawEntity>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<Name>,
}

/// The areas in `areas.toml`, ordered by id.
pub fn read_areas(text: &str) -> Result<Vec<Area>, String> {
    let file: AreasFile = toml::from_str(text).map_err(|e| e.to_string())?;
    Ok(file
        .areas
        .into_iter()
        .map(|(id, raw)| Area {
            id,
            name: raw.name,
            floor_id: raw.floor,
        })
        .collect())
}

pub fn read_devices(text: &str) -> Result<BTreeMap<SettingsKey, DeviceSettings>, String> {
    let file: DevicesFile = toml::from_str(text).map_err(|e| e.to_string())?;
    file.devices
        .into_iter()
        .map(|(key, raw)| {
            let key: SettingsKey = key.parse().map_err(|e| format!("`{key}`: {e}"))?;
            Ok((
                key,
                DeviceSettings {
                    name: raw.name,
                    area: raw.area,
                },
            ))
        })
        .collect()
}

pub fn read_entities(text: &str) -> Result<BTreeMap<SettingsKey, EntitySettings>, String> {
    let file: EntitiesFile = toml::from_str(text).map_err(|e| e.to_string())?;
    file.entities
        .into_iter()
        .map(|(key, raw)| {
            let key: SettingsKey = key.parse().map_err(|e| format!("`{key}`: {e}"))?;
            Ok((key, EntitySettings { name: raw.name }))
        })
        .collect()
}

/// One file's text, ready to write. Entries that say nothing are left out rather than written as
/// empty tables.
pub fn write(file: File, settings: &Settings) -> String {
    let body = match file {
        File::Areas => toml::to_string_pretty(&AreasFile {
            areas: settings
                .areas
                .iter()
                .map(|area| {
                    (
                        area.id.clone(),
                        RawArea {
                            name: area.name.clone(),
                            floor: area.floor_id.clone(),
                        },
                    )
                })
                .collect(),
        }),
        File::Devices => toml::to_string_pretty(&DevicesFile {
            devices: settings
                .devices
                .iter()
                .filter(|(_, device)| !device.is_empty())
                .map(|(key, device)| {
                    (
                        key.to_string(),
                        RawDevice {
                            name: device.name.clone(),
                            area: device.area.clone(),
                        },
                    )
                })
                .collect(),
        }),
        File::Entities => toml::to_string_pretty(&EntitiesFile {
            entities: settings
                .entities
                .iter()
                .filter(|(_, entity)| !entity.is_empty())
                .map(|(key, entity)| {
                    (
                        key.to_string(),
                        RawEntity {
                            name: entity.name.clone(),
                        },
                    )
                })
                .collect(),
        }),
    };
    // These types are maps of plain strings and options; serialization can't fail. An empty
    // string rather than a panic if it somehow did: losing one file beats taking the server down.
    format!("{}{}", preamble(file), body.unwrap_or_default())
}

fn preamble(file: File) -> String {
    let what = match file {
        File::Areas => "The rooms of your home.",
        File::Devices => {
            "What you've said about your devices: what to call one, and which room it's in.\n\
             # Each key is `<integration>/<the integration's own id for the device>`."
        }
        File::Entities => {
            "What you've said about individual entities.\n\
             # Each key is `<integration>/<the integration's own id for the entity>`."
        }
    };
    format!(
        "# {what}\n\
         #\n\
         # Written by Irori, and yours to edit: changes are picked up within a couple of seconds.\n\
         # Comments and ordering don't survive a rewrite. See docs/specs/config.md.\n\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> SettingsKey {
        s.parse().expect("a valid settings key")
    }

    fn name(s: &str) -> Name {
        s.parse().expect("a valid name")
    }

    fn area_id(s: &str) -> AreaId {
        s.parse().expect("a valid area id")
    }

    #[test]
    fn areas_read_from_the_shape_the_spec_shows() {
        let areas =
            read_areas("[areas.hall]\nname = \"Hall\"\n\n[areas.kitchen]\nname = \"Kitchen\"\n")
                .expect("valid");
        assert_eq!(areas.len(), 2);
        assert_eq!(areas[0].id.as_str(), "hall");
        assert_eq!(areas[1].name.as_str(), "Kitchen");
    }

    #[test]
    fn devices_read_from_the_shape_the_spec_shows() {
        let devices = read_devices(
            "[devices.\"esphome/34:98:7a:2b:09:00\"]\nname = \"Hallway radar\"\narea = \"hall\"\n",
        )
        .expect("valid");
        let settings = devices
            .get(&key("esphome/34:98:7a:2b:09:00"))
            .expect("the device");
        assert_eq!(
            settings.name.as_ref().map(Name::as_str),
            Some("Hallway radar")
        );
        assert_eq!(settings.area.as_ref().map(AreaId::as_str), Some("hall"));
    }

    #[test]
    fn a_missing_file_and_an_empty_one_say_the_same_thing() {
        assert!(read_areas("").expect("valid").is_empty());
        assert!(read_devices("").expect("valid").is_empty());
        assert!(read_entities("").expect("valid").is_empty());
    }

    /// A typo shouldn't be read as "you said nothing": `deny_unknown_fields` is what makes
    /// `nme = "Hall"` an error a person can see rather than a silent no-op.
    #[test]
    fn a_misspelled_field_is_an_error_rather_than_silence() {
        assert!(read_areas("[areas.hall]\nnme = \"Hall\"\n").is_err());
        assert!(read_devices("[devices.\"demo/lamp\"]\nroom = \"hall\"\n").is_err());
        assert!(read_devices("[device.\"demo/lamp\"]\nname = \"Lamp\"\n").is_err());
    }

    #[test]
    fn a_key_that_isnt_a_settings_key_names_itself_in_the_error() {
        let error = read_devices("[devices.lamp]\nname = \"Lamp\"\n").expect_err("no integration");
        assert!(error.contains("lamp"), "{error}");
    }

    #[test]
    fn what_is_written_reads_back_the_same() {
        let settings = Settings {
            areas: vec![Area {
                id: area_id("hall"),
                name: name("Hall"),
                floor_id: None,
            }],
            devices: [(
                key("esphome/34:98:7a:2b:09:00"),
                DeviceSettings {
                    name: Some(name("Hallway radar")),
                    area: Some(area_id("hall")),
                },
            )]
            .into(),
            entities: [(
                key("esphome/34:98:7a:2b:09:00-binary_sensor-1594977085"),
                EntitySettings {
                    name: Some(name("Hallway occupancy")),
                },
            )]
            .into(),
        };

        assert_eq!(
            read_areas(&write(File::Areas, &settings)).expect("valid"),
            settings.areas
        );
        assert_eq!(
            read_devices(&write(File::Devices, &settings)).expect("valid"),
            settings.devices
        );
        assert_eq!(
            read_entities(&write(File::Entities, &settings)).expect("valid"),
            settings.entities
        );
    }

    /// An entry left with nothing in it is the shape a rename-then-undo leaves behind. Writing
    /// it out would grow the file with lines that mean nothing.
    #[test]
    fn an_entry_that_says_nothing_is_not_written() {
        let settings = Settings {
            devices: [
                (key("demo/lamp"), DeviceSettings::default()),
                (
                    key("demo/plug"),
                    DeviceSettings {
                        name: Some(name("Plug")),
                        area: None,
                    },
                ),
            ]
            .into(),
            ..Settings::default()
        };
        let written = write(File::Devices, &settings);
        assert!(!written.contains("demo/lamp"), "{written}");
        assert!(written.contains("demo/plug"), "{written}");
    }
}
