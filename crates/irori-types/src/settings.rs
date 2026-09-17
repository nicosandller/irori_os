//! What a person has said about their home, as opposed to what the integrations report.
//!
//! These are the contents of the config directory once it has been read and checked
//! (`docs/specs/config.md`). They live here, rather than in `irori-config`, so the core can be
//! told about them without depending on a file format — and so a tool that only wants to read
//! config doesn't have to pull in the core.

use std::collections::BTreeMap;

use crate::{
    Area, AreaId, Description, DeviceId, ExtensionId, Floor, FloorId, IdError, IntegrationId, Name,
    UniqueId,
};

/// Everything the config directory says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// The levels of the home, ordered by id.
    pub floors: Vec<Floor>,
    /// The rooms of the home, ordered by id.
    pub areas: Vec<Area>,
    /// By the device's id, which never changes (ROADMAP D36).
    pub devices: BTreeMap<DeviceId, DeviceSettings>,
    pub entities: BTreeMap<SettingsKey, EntitySettings>,
    /// Whether a device Irori hasn't been told to add waits to be added, rather than joining the
    /// home as soon as it's found (`irori.toml`, `[devices] new = "ask"`).
    pub ask_before_adding: bool,
}

impl Settings {
    pub fn floor(&self, id: &FloorId) -> Option<&Floor> {
        self.floors.iter().find(|floor| &floor.id == id)
    }

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
    /// What it's for.
    pub description: Option<Description>,
    /// Which room it's in, if that's been decided.
    pub area: Placement,
    /// Kept out of the home: not listed, not controllable, nothing it reports is recorded. The
    /// integration may still talk to it; Irori just doesn't let it in.
    pub ignored: bool,
    /// A person added it. Only matters while Irori asks before adding new devices.
    pub added: bool,
}

impl DeviceSettings {
    /// Whether this says anything at all. An entry that says nothing is not written out.
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.description.is_none()
            && self.area.is_unsaid()
            && !self.ignored
            && !self.added
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

/// Each extension's settings, as the config directory has them: today, its table in
/// `secrets.toml` (`docs/specs/config.md` §3.4). An extension gets exactly its own table, as JSON,
/// and checks it against its own config type when it starts.
///
/// These are secrets. `Debug` names the extensions and nothing else, so a stray `{:?}` in a log
/// line can't print a key; nothing that serializes this type exists, on purpose.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ExtensionSettings(BTreeMap<ExtensionId, serde_json::Map<String, serde_json::Value>>);

impl std::fmt::Debug for ExtensionSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.0.keys()).finish_non_exhaustive()
    }
}

/// Why a secret couldn't be put where it was asked to go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretError(pub String);

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SecretError {}

impl ExtensionSettings {
    pub fn new(tables: BTreeMap<ExtensionId, serde_json::Map<String, serde_json::Value>>) -> Self {
        Self(tables)
    }

    /// One extension's settings: its table, or an empty one when the file says nothing about it.
    pub fn of(&self, extension: &ExtensionId) -> serde_json::Value {
        serde_json::Value::Object(self.0.get(extension).cloned().unwrap_or_default())
    }

    pub fn tables(&self) -> &BTreeMap<ExtensionId, serde_json::Map<String, serde_json::Value>> {
        &self.0
    }

    /// Puts a secret at `path` inside the extension's table, making the tables on the way.
    ///
    /// Refuses rather than overwrites when something that isn't a table is in the way: a path
    /// that runs through a value is a mistake in whoever asked, not an instruction to delete it.
    pub fn set(
        &mut self,
        extension: &ExtensionId,
        path: &[String],
        value: String,
    ) -> Result<(), SecretError> {
        let (last, tables) = path.split_last().ok_or_else(|| {
            SecretError("a secret needs somewhere to go: the path is empty".into())
        })?;
        if path.iter().any(|key| key.is_empty()) {
            return Err(SecretError(
                "a secret's path can't have an empty key in it".into(),
            ));
        }
        let mut table = self.0.entry(extension.clone()).or_default();
        for key in tables {
            table = match table
                .entry(key.clone())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
            {
                serde_json::Value::Object(inner) => inner,
                _ => {
                    return Err(SecretError(format!(
                        "`{key}` in `{extension}`'s secrets is a value, not a table, so nothing \
                         can go inside it"
                    )));
                }
            };
        }
        table.insert(last.clone(), serde_json::Value::String(value));
        Ok(())
    }

    /// Takes a secret away. Tables it leaves empty go too, so the file doesn't fill up with
    /// headings that hold nothing. Says whether there was anything to remove.
    pub fn remove(&mut self, extension: &ExtensionId, path: &[String]) -> bool {
        fn remove_in(
            table: &mut serde_json::Map<String, serde_json::Value>,
            path: &[String],
        ) -> bool {
            match path {
                [] => false,
                [last] => table.remove(last).is_some(),
                [first, rest @ ..] => {
                    let Some(serde_json::Value::Object(inner)) = table.get_mut(first) else {
                        return false;
                    };
                    let removed = remove_in(inner, rest);
                    if inner.is_empty() {
                        table.remove(first);
                    }
                    removed
                }
            }
        }
        let Some(table) = self.0.get_mut(extension) else {
            return false;
        };
        let removed = remove_in(table, path);
        if table.is_empty() {
            self.0.remove(extension);
        }
        removed
    }
}

/// What a setting is attached to: an integration and its own permanent handle for the thing
/// (`docs/specs/config.md` §4).
///
/// Not a `DeviceId` or `EntityId`. Those are also made from the integration and permanent handle
/// (ROADMAP D36), not from names; settings still key on the handle itself so they stay attached
/// if the user-facing id format ever changes.
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

    fn extension() -> ExtensionId {
        "esphome".parse().expect("a valid extension id")
    }

    fn path(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|key| (*key).to_owned()).collect()
    }

    /// A secret lands where the extension asked, and an extension sees only its own table.
    #[test]
    fn a_secret_goes_where_its_path_says_and_nowhere_else() {
        let mut settings = ExtensionSettings::default();
        settings
            .set(
                &extension(),
                &path(&["keys", "00:11:22:33:44:55"]),
                "k".into(),
            )
            .expect("set");
        assert_eq!(
            settings.of(&extension()),
            serde_json::json!({"keys": {"00:11:22:33:44:55": "k"}})
        );
        assert_eq!(
            settings.of(&"demo".parse().expect("a valid extension id")),
            serde_json::json!({})
        );
    }

    #[test]
    fn a_secret_wont_overwrite_a_value_that_stands_in_its_way() {
        let mut settings = ExtensionSettings::default();
        settings
            .set(&extension(), &path(&["keys"]), "not a table".into())
            .expect("set");
        assert!(
            settings
                .set(&extension(), &path(&["keys", "mac"]), "k".into())
                .is_err()
        );
        assert!(settings.set(&extension(), &[], "k".into()).is_err());
        assert!(
            settings
                .set(&extension(), &path(&["keys", ""]), "k".into())
                .is_err()
        );
    }

    /// Removing the last secret leaves no empty headings behind.
    #[test]
    fn removing_a_secret_tidies_away_what_it_leaves_empty() {
        let mut settings = ExtensionSettings::default();
        settings
            .set(&extension(), &path(&["keys", "a"]), "k".into())
            .expect("set");
        assert!(settings.remove(&extension(), &path(&["keys", "a"])));
        assert_eq!(settings, ExtensionSettings::default());
        assert!(
            !settings.remove(&extension(), &path(&["keys", "a"])),
            "nothing left to remove"
        );
    }

    /// The whole reason `Debug` is written by hand.
    #[test]
    fn debugging_settings_never_prints_a_secret() {
        let mut settings = ExtensionSettings::default();
        settings
            .set(
                &extension(),
                &path(&["keys", "mac"]),
                "c2VjcmV0LWtleQ==".into(),
            )
            .expect("set");
        let shown = format!("{settings:?}");
        assert!(!shown.contains("c2VjcmV0"), "{shown}");
        assert!(shown.contains("esphome"), "{shown}");
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
