//! The TOML shape of each file, and the translation to and from [`Settings`].
//!
//! The files are deliberately not a mirror of the Rust types: a settings key is one quoted
//! string in TOML (`"esphome/34:98:7a:2b:09:00"`) and a pair of ids in Rust, and an area's id is
//! the table key rather than a field. Keeping the translation here means the rest of Irori never
//! sees the file format.

use std::collections::BTreeMap;

use irori_types::{
    Area, AreaId, Description, DeviceId, DeviceSettings, EntitySettings, ExtensionId,
    ExtensionSettings, FloorId, Name, Placement, Settings, SettingsKey,
};
use serde::{Deserialize, Serialize};

/// A file this crate reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum File {
    Irori,
    Areas,
    Devices,
    Entities,
    Secrets,
}

impl File {
    /// Every file, in the order they're read.
    pub const ALL: [File; 5] = [
        File::Irori,
        File::Areas,
        File::Devices,
        File::Entities,
        File::Secrets,
    ];

    /// The files that make up [`Settings`], which are saved together.
    pub const SETTINGS: [File; 3] = [File::Areas, File::Devices, File::Entities];

    pub fn name(self) -> &'static str {
        match self {
            File::Irori => "irori.toml",
            File::Areas => "areas.toml",
            File::Devices => "devices.toml",
            File::Entities => "entities.toml",
            File::Secrets => "secrets.toml",
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
    devices: BTreeMap<DeviceId, RawDevice>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDevice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<Description>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    area: Option<RawPlacement>,
}

/// `area = "hall"` for a room, `area = false` for "not in one, and don't ask the device".
///
/// A word like `"none"` would have been friendlier to read, but `none` is a perfectly good area
/// id — somebody's room could be called that — so the two have to be different types rather than
/// different strings.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum RawPlacement {
    In(AreaId),
    Nowhere(bool),
}

impl RawPlacement {
    fn of(placement: &Placement) -> Option<Self> {
        match placement {
            Placement::Unsaid => None,
            Placement::Nowhere => Some(RawPlacement::Nowhere(false)),
            Placement::In(area) => Some(RawPlacement::In(area.clone())),
        }
    }

    /// `area = true` has no meaning — "yes, a room" doesn't say which — so it's an error rather
    /// than a guess.
    fn placement(self) -> Result<Placement, String> {
        match self {
            RawPlacement::In(area) => Ok(Placement::In(area)),
            RawPlacement::Nowhere(false) => Ok(Placement::Nowhere),
            RawPlacement::Nowhere(true) => Err(
                "`area = true` doesn't say which room; use a room's id, or `false` for none"
                    .to_owned(),
            ),
        }
    }
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

pub fn read_devices(text: &str) -> Result<BTreeMap<DeviceId, DeviceSettings>, String> {
    let file: DevicesFile = toml::from_str(text).map_err(|e| e.to_string())?;
    file.devices
        .into_iter()
        .map(|(id, raw)| {
            let area = match raw.area {
                Some(raw) => raw.placement().map_err(|e| format!("`{id}`: {e}"))?,
                None => Placement::Unsaid,
            };
            Ok((
                id,
                DeviceSettings {
                    name: raw.name,
                    description: raw.description,
                    area,
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

/// Settings for Irori itself, from `irori.toml` (`docs/specs/config.md` §3.5).
///
/// Irori only ever reads this file. Nothing it serves can write it, which matters while there's no
/// sign-in: `allow_unauthenticated_lan` lives here, and a page that could set it would let anyone
/// who can reach Irori open it to the whole network.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IroriSettings {
    #[serde(default)]
    pub server: ServerSettings,
    #[serde(default)]
    pub extensions: ExtensionsSection,
}

/// `[server]`: what command-line flags also say. A flag, or its environment variable, wins over
/// the file; the file wins over the default. Read at startup only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSettings {
    pub bind: Option<std::net::SocketAddr>,
    /// Relative to the config directory.
    pub data: Option<std::path::PathBuf>,
    pub allow_unauthenticated_lan: Option<bool>,
    pub log_level: Option<LogLevel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// `[extensions]`. Applied while Irori runs: disabling one stops it, enabling it starts it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionsSection {
    /// Extensions that stay off. Every built-in extension is on unless it's named here.
    #[serde(default)]
    pub disabled: std::collections::BTreeSet<ExtensionId>,
}

pub fn read_irori(text: &str) -> Result<IroriSettings, String> {
    toml::from_str(text).map_err(|e| e.to_string())
}

/// Each extension's table in `secrets.toml`.
///
/// Parse errors say where, never what: TOML's own messages quote the offending line, and in
/// this file that line is somebody's key, on its way to a log.
pub fn read_secrets(text: &str) -> Result<ExtensionSettings, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| {
        let at = e.span().map_or_else(String::new, |span| {
            let line = text[..span.start.min(text.len())].lines().count().max(1);
            format!(" on line {line}")
        });
        format!("isn't valid TOML{at}: {}", e.message())
    })?;
    let mut tables = std::collections::BTreeMap::new();
    for (extension, value) in table {
        let id: ExtensionId = extension
            .parse()
            .map_err(|e| format!("`[{extension}]` isn't an extension id: {e}"))?;
        let toml::Value::Table(inner) = value else {
            return Err(format!(
                "`{extension}` has to be a table (`[{extension}]`), holding that extension's secrets"
            ));
        };
        // TOML's types all have a JSON form apart from dates, which no secret is.
        let serde_json::Value::Object(inner) = serde_json::to_value(inner)
            .map_err(|_| format!("`[{extension}]` holds something that isn't a plain value"))?
        else {
            unreachable!("a TOML table becomes a JSON object");
        };
        tables.insert(id, inner);
    }
    Ok(ExtensionSettings::new(tables))
}

/// `secrets.toml`'s text, ready to write.
pub fn write_secrets(secrets: &ExtensionSettings) -> String {
    let body = toml::to_string_pretty(secrets.tables()).unwrap_or_default();
    format!(
        "# Secrets for extensions: encryption keys, passwords, tokens. One table per extension.\n\
         #\n\
         # Keep this file out of git, and out of anywhere else it could be read. Irori creates it\n\
         # readable by its own user only.\n\
         # Written by Irori, and yours to edit: changes are picked up within a couple of seconds.\n\
         # See docs/specs/config.md.\n\n{body}"
    )
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
                .map(|(id, device)| {
                    (
                        id.clone(),
                        RawDevice {
                            name: device.name.clone(),
                            description: device.description.clone(),
                            area: RawPlacement::of(&device.area),
                        },
                    )
                })
                .collect(),
        }),
        File::Secrets => unreachable!("secrets are written by `write_secrets`"),
        File::Irori => unreachable!("irori.toml is only ever read"),
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
            "What you've said about your devices: what each is called, what it's for, and which\n\
             # room it's in. Each key is the device's id, the same one its page and the API use."
        }
        File::Entities => {
            "What you've said about individual entities.\n\
             # Each key is `<integration>/<the integration's own id for the entity>`."
        }
        File::Secrets => unreachable!("secrets have their own preamble"),
        File::Irori => unreachable!("irori.toml is only ever read"),
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
    fn irori_toml_reads_the_shape_the_spec_shows() {
        let settings = read_irori(
            "[server]\nbind = \"0.0.0.0:8480\"\nlog_level = \"debug\"\n\
             allow_unauthenticated_lan = true\ndata = \"/var/lib/irori\"\n\n\
             [extensions]\ndisabled = [\"demo\"]\n",
        )
        .expect("valid");
        assert_eq!(
            settings.server.bind,
            Some("0.0.0.0:8480".parse().expect("valid"))
        );
        assert_eq!(settings.server.log_level, Some(LogLevel::Debug));
        assert!(
            settings
                .extensions
                .disabled
                .contains(&"demo".parse().expect("valid"))
        );
        assert_eq!(read_irori("").expect("empty"), IroriSettings::default());
        assert!(read_irori("[server]\nlog_level = \"loud\"\n").is_err());
        assert!(read_irori("[server]\nport = 80\n").is_err());
    }

    #[test]
    fn secrets_are_one_table_per_extension() {
        let secrets =
            read_secrets("[esphome.keys]\n\"30:83:98:CA:6A:08\" = \"c2VjcmV0\"\n").expect("valid");
        assert_eq!(
            secrets.of(&"esphome".parse().expect("a valid id")),
            serde_json::json!({"keys": {"30:83:98:CA:6A:08": "c2VjcmV0"}})
        );
        assert_eq!(
            read_secrets(&write_secrets(&secrets)).expect("valid"),
            secrets
        );
    }

    #[test]
    fn a_secret_outside_a_table_or_under_a_bad_id_is_refused() {
        assert!(read_secrets("key = \"abc\"\n").is_err());
        assert!(read_secrets("[Not_An_Id]\nkey = \"abc\"\n").is_err());
    }

    /// TOML's errors quote the line they're about, and in this file that line is a secret.
    #[test]
    fn a_broken_secrets_file_is_described_without_quoting_it() {
        let error = read_secrets("[esphome.keys]\nmac = \"c2VjcmV0LWtleQ\n").expect_err("broken");
        assert!(!error.contains("c2VjcmV0"), "{error}");
        assert!(error.contains("line 2"), "{error}");
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
            "[devices.esphome_34_98_7a_2b_09_00]\nname = \"Hallway radar\"\ndescription = \"By the door\"\narea = \"hall\"\n",
        )
        .expect("valid");
        let settings = devices
            .get(
                &"esphome_34_98_7a_2b_09_00"
                    .parse::<DeviceId>()
                    .expect("valid"),
            )
            .expect("the device");
        assert_eq!(
            settings.name.as_ref().map(Name::as_str),
            Some("Hallway radar")
        );
        assert_eq!(settings.area.area().map(AreaId::as_str), Some("hall"));
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

    /// A device is written under its id, which is a slug; the old `<integration>/<handle>` form
    /// isn't one, and says so rather than being read as a device nobody has.
    #[test]
    fn a_device_key_that_isnt_a_device_id_names_itself_in_the_error() {
        let error = read_devices("[devices.\"esphome/34:98:7a\"]\nname = \"Lamp\"\n")
            .expect_err("not a device id");
        assert!(error.contains("esphome/34:98:7a"), "{error}");
        let error =
            read_entities("[entities.lamp]\nname = \"Lamp\"\n").expect_err("no integration");
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
                "esphome_34_98_7a_2b_09_00"
                    .parse::<DeviceId>()
                    .expect("valid"),
                DeviceSettings {
                    name: Some(name("Hallway radar")),
                    description: Some("By the door".parse().expect("valid")),
                    area: Placement::In(area_id("hall")),
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
                (
                    "demo_lamp".parse::<DeviceId>().expect("valid"),
                    DeviceSettings::default(),
                ),
                (
                    "demo_plug".parse::<DeviceId>().expect("valid"),
                    DeviceSettings {
                        name: Some(name("Plug")),
                        description: None,
                        area: Placement::Unsaid,
                    },
                ),
            ]
            .into(),
            ..Settings::default()
        };
        let written = write(File::Devices, &settings);
        assert!(!written.contains("demo_lamp"), "{written}");
        assert!(written.contains("demo_plug"), "{written}");
    }
}
