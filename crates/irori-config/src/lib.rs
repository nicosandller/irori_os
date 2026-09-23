//! Load, validate, and hot-reload the plain-text config directory.
//!
//! What a person has said about their home — what to call a device, which room it's in — lives in
//! a directory of TOML files rather than in the database, so it can be read, edited, diffed and
//! kept in git. See `docs/specs/config.md`.
//!
//! The rules that make this safe to leave running:
//!
//! - **A broken file loses only itself.** Its last good contents stay live, and the other files
//!   are unaffected.
//! - **Writes are atomic.** A temporary file in the same directory, renamed over the target, so
//!   nobody reads half a file.
//! - **Nothing is written that wasn't asked for.** Files are rewritten only when their contents
//!   would actually change.

mod files;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use irori_types::{
    Area, AreaId, DeviceId, DeviceSettings, EntitySettings, ExtensionId, ExtensionSettings, Floor,
    FloorId, Floorplan, Settings, SettingsKey,
};

pub use files::{
    DevicesSection, ExtensionsSection, File, IroriSettings, LogLevel, NewDevices, ServerSettings,
};

/// Something wrong with one file, to be logged and shown. Never fatal: the file keeps whatever it
/// last held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// The file's path inside the config directory, e.g. `areas.toml` or
    /// `extensions/helpers.toml`.
    pub file: String,
    pub reason: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.file, self.reason)
    }
}

/// What a file looked like last time, so an unchanged file isn't parsed again.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    modified: Option<SystemTime>,
    len: u64,
}

/// One file's last good contents, and what the file looked like when they were read.
#[derive(Debug)]
struct Part<T> {
    value: T,
    seen: Option<Seen>,
}

impl<T: Default> Default for Part<T> {
    fn default() -> Self {
        Self {
            value: T::default(),
            seen: None,
        }
    }
}

/// The config directory: what it says, and whether it has changed.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    irori: Part<IroriSettings>,
    areas: Part<(Vec<Floor>, Vec<Area>)>,
    floorplan: Part<Floorplan>,
    devices: Part<BTreeMap<DeviceId, DeviceSettings>>,
    entities: Part<BTreeMap<SettingsKey, EntitySettings>>,
    secrets: Part<ExtensionSettings>,
    /// `extensions/<id>.toml`, one per extension that has one.
    extensions: BTreeMap<ExtensionId, Part<serde_json::Map<String, serde_json::Value>>>,
    /// Keys set in both an extension's file and its secrets, as last reported: said once when
    /// they appear, not on every two-second check.
    clashes: std::collections::BTreeSet<(ExtensionId, String)>,
    /// Rooms naming a floor that isn't there, as last reported: said once when they appear.
    missing_floors: std::collections::BTreeSet<(AreaId, FloorId)>,
}

impl Store {
    /// Points at a directory without reading it. A directory that doesn't exist is not an error:
    /// it means nothing has been configured, and it's created on the first write.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            irori: Part::default(),
            areas: Part::default(),
            floorplan: Part::default(),
            devices: Part::default(),
            entities: Part::default(),
            secrets: Part::default(),
            extensions: BTreeMap::new(),
            clashes: std::collections::BTreeSet::new(),
            missing_floors: std::collections::BTreeSet::new(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, file: File) -> PathBuf {
        self.dir.join(file.name())
    }

    /// Everything the directory currently says.
    pub fn settings(&self) -> Settings {
        Settings {
            floors: self.areas.value.0.clone(),
            areas: self.areas.value.1.clone(),
            floorplan: self.floorplan.value.clone(),
            devices: self.devices.value.clone(),
            entities: self.entities.value.clone(),
            ask_before_adding: self.irori.value.devices.new == NewDevices::Ask,
        }
    }

    /// Settings for Irori itself, from `irori.toml`.
    pub fn irori(&self) -> IroriSettings {
        self.irori.value.clone()
    }

    /// Each extension's settings: `extensions/<id>.toml` joined with its table in `secrets.toml`
    /// (`docs/specs/config.md` §3.4). A key in both is a mistake [`Store::reload`] reports; the
    /// secret is the one used.
    pub fn extension_settings(&self) -> ExtensionSettings {
        let mut tables = self.secrets.value.tables().clone();
        for (id, part) in &self.extensions {
            let joined = tables.entry(id.clone()).or_default();
            for (key, value) in &part.value {
                joined.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        ExtensionSettings::new(tables)
    }

    /// `secrets.toml` only, not joined with `extensions/<id>.toml`. Writes that go back to
    /// `secrets.toml` must start from this, or non-secret settings would be copied into it.
    pub fn secrets(&self) -> ExtensionSettings {
        self.secrets.value.clone()
    }

    /// One extension's settings file as it stands, for changing and saving back.
    pub fn extension_file(
        &self,
        extension: &ExtensionId,
    ) -> serde_json::Map<String, serde_json::Value> {
        self.extensions
            .get(extension)
            .map(|part| part.value.clone())
            .unwrap_or_default()
    }

    /// Replaces `extensions/<id>.toml`, atomically, if its contents would change.
    pub fn save_extension(
        &mut self,
        extension: &ExtensionId,
        settings: &serde_json::Map<String, serde_json::Value>,
    ) -> std::io::Result<bool> {
        let dir = self.dir.join("extensions");
        let path = dir.join(format!("{extension}.toml"));
        let text = files::write_extension(extension, settings)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let changed = !std::fs::read_to_string(&path).is_ok_and(|current| current == text);
        if changed {
            std::fs::create_dir_all(&dir)?;
            let temporary = write_beside(&path, &text, Readable::ByAnyone)?;
            if let Err(e) = std::fs::rename(&temporary, &path) {
                let _ = std::fs::remove_file(&temporary);
                return Err(e);
            }
        }
        self.extensions.insert(
            extension.clone(),
            Part {
                value: settings.clone(),
                seen: look(&path),
            },
        );
        Ok(changed)
    }

    /// Re-reads whatever has changed on disk since the last call, and says what went wrong.
    ///
    /// The first call reads everything. After that an untouched file costs one `stat`, which is
    /// what makes polling every couple of seconds reasonable.
    pub fn reload(&mut self) -> Vec<Problem> {
        let mut problems = Vec::new();
        for file in File::ALL {
            if let Err(reason) = self.reload_one(file) {
                problems.push(Problem {
                    file: file.name().to_owned(),
                    reason,
                });
            }
        }
        problems.extend(self.reload_extensions());
        // A key set in both places is ambiguous; the secret wins. Said when it first appears.
        let mut clashes = std::collections::BTreeSet::new();
        for (id, part) in &self.extensions {
            if let Some(secret) = self.secrets.value.tables().get(id) {
                for key in part.value.keys().filter(|key| secret.contains_key(*key)) {
                    clashes.insert((id.clone(), key.clone()));
                }
            }
        }
        for (id, key) in clashes.difference(&self.clashes) {
            problems.push(Problem {
                file: format!("extensions/{id}.toml"),
                reason: format!(
                    "`{key}` is also in secrets.toml under [{id}]; the one in secrets.toml is used"
                ),
            });
        }
        self.clashes = clashes;

        // A room naming a missing floor is kept (config.md §3.1 / §6). Warn once when it appears.
        let floor_ids: std::collections::BTreeSet<_> = self
            .areas
            .value
            .0
            .iter()
            .map(|floor| floor.id.clone())
            .collect();
        let mut missing_floors = std::collections::BTreeSet::new();
        for area in &self.areas.value.1 {
            if let Some(floor) = &area.floor_id
                && !floor_ids.contains(floor)
            {
                missing_floors.insert((area.id.clone(), floor.clone()));
            }
        }
        for (room, floor) in missing_floors.difference(&self.missing_floors) {
            tracing::warn!(
                room = %room,
                floor = %floor,
                "room names a floor that isn't there; it's listed without a floor"
            );
        }
        self.missing_floors = missing_floors;

        problems
    }

    /// Reads `extensions/*.toml`: new files, changed ones, and ones that have gone.
    fn reload_extensions(&mut self) -> Vec<Problem> {
        let dir = self.dir.join("extensions");
        let mut problems = Vec::new();
        let mut present = std::collections::BTreeSet::new();
        // A missing directory means nothing is configured. Any other listing error (permissions,
        // a transient I/O fault) must not look like every file was deleted: last-good stays.
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => {
                let mut collected = Vec::new();
                for entry in entries {
                    match entry {
                        Ok(entry) => collected.push(entry),
                        Err(e) => {
                            problems.push(Problem {
                                file: "extensions".to_owned(),
                                reason: format!("can't list it: {e}"),
                            });
                            return problems;
                        }
                    }
                }
                collected
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                problems.push(Problem {
                    file: "extensions".to_owned(),
                    reason: format!("can't read it: {e}"),
                });
                return problems;
            }
        };
        for entry in entries {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let file = format!("extensions/{stem}.toml");
            let Ok(id) = stem.parse::<ExtensionId>() else {
                problems.push(Problem {
                    file,
                    reason: "the file name has to be an extension id, like `helpers.toml`"
                        .to_owned(),
                });
                continue;
            };
            present.insert(id.clone());
            let now = look(&path);
            if self
                .extensions
                .get(&id)
                .is_some_and(|part| part.seen.is_some() && part.seen == now)
            {
                continue;
            }
            let parsed = std::fs::read_to_string(&path)
                .map_err(|e| format!("can't read it: {e}"))
                .and_then(|text| files::read_extension(&text));
            match parsed {
                Ok(value) => {
                    self.extensions.insert(id, Part { value, seen: now });
                }
                // The last good version stays, as for every other file.
                Err(reason) => problems.push(Problem { file, reason }),
            }
        }
        self.extensions.retain(|id, _| present.contains(id));
        problems
    }

    fn reload_one(&mut self, file: File) -> Result<(), String> {
        let path = self.path(file);
        let now = look(&path);
        let unchanged = match (&now, self.seen(file)) {
            // Both absent, or both the same size and mtime: nothing to do.
            (None, None) => true,
            (Some(now), Some(before)) => now == before,
            _ => false,
        };
        if unchanged {
            return Ok(());
        }
        // Absent and unreadable are different: a file that was deleted says "nothing configured",
        // but one that exists and can't be read is a problem worth showing.
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("can't read {}: {e}", path.display())),
        };
        match file {
            File::Irori => self.irori.value = files::read_irori(&text)?,
            File::Areas => self.areas.value = files::read_areas(&text)?,
            File::Floorplan => self.floorplan.value = files::read_floorplan(&text)?,
            File::Devices => self.devices.value = files::read_devices(&text)?,
            File::Entities => self.entities.value = files::read_entities(&text)?,
            File::Secrets => self.secrets.value = files::read_secrets(&text)?,
        }
        // Only once it parsed: a file that is being edited and saved half-written should be
        // re-read on the next tick, not remembered as good.
        *self.seen_mut(file) = now;
        Ok(())
    }

    fn seen(&self, file: File) -> Option<&Seen> {
        match file {
            File::Irori => self.irori.seen.as_ref(),
            File::Areas => self.areas.seen.as_ref(),
            File::Floorplan => self.floorplan.seen.as_ref(),
            File::Devices => self.devices.seen.as_ref(),
            File::Entities => self.entities.seen.as_ref(),
            File::Secrets => self.secrets.seen.as_ref(),
        }
    }

    fn seen_mut(&mut self, file: File) -> &mut Option<Seen> {
        match file {
            File::Irori => &mut self.irori.seen,
            File::Areas => &mut self.areas.seen,
            File::Floorplan => &mut self.floorplan.seen,
            File::Devices => &mut self.devices.seen,
            File::Entities => &mut self.entities.seen,
            File::Secrets => &mut self.secrets.seen,
        }
    }

    /// Replaces what the directory says, writing only the files whose contents would change.
    ///
    /// Returns the files it wrote, which is empty when the settings match what's already there —
    /// so saving the same thing twice touches nothing.
    ///
    /// Every file is written to a temporary one first, and only then are they renamed into place.
    /// Renaming a file that already exists on the same filesystem is about as close to "cannot
    /// fail" as a filesystem offers, while writing the bytes is where a full disk or a read-only
    /// mount shows up — so doing all the writing first means a failure leaves the directory
    /// exactly as it was, rather than half of a change that the next reload would then adopt.
    pub fn save(&mut self, settings: &Settings) -> std::io::Result<Vec<File>> {
        let mut prepared = Vec::new();
        for file in File::SETTINGS {
            let text = files::write(file, settings);
            let path = self.path(file);
            if std::fs::read_to_string(&path).is_ok_and(|current| current == text) {
                continue;
            }
            std::fs::create_dir_all(&self.dir)?;
            match write_beside(&path, &text, Readable::ByAnyone) {
                Ok(temporary) => prepared.push((file, path, temporary)),
                Err(e) => {
                    discard(&prepared);
                    return Err(e);
                }
            }
        }

        let mut written: Vec<File> = Vec::new();
        for (file, path, temporary) in &prepared {
            if let Err(e) = std::fs::rename(temporary, path) {
                // A rename failing here is close to unheard of, and there is nothing sensible
                // left to try — so the error says which files did land, because that, not the
                // errno, is what someone needs in order to put the directory right by hand.
                let landed = match written.as_slice() {
                    [] => "no files were changed".to_owned(),
                    landed => format!(
                        "these were already changed: {}",
                        landed
                            .iter()
                            .map(|file| file.name())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                };
                let kind = e.kind();
                discard(&prepared);
                return Err(std::io::Error::new(
                    kind,
                    format!("couldn't put {file} in place ({landed}): {e}"),
                ));
            }
            written.push(*file);
        }
        self.areas.value = (settings.floors.clone(), settings.areas.clone());
        self.floorplan.value = settings.floorplan.clone();
        self.devices.value = settings.devices.clone();
        self.entities.value = settings.entities.clone();
        // The files on disk are now these settings, so the next reload must not treat Irori's
        // own write as an outside edit and parse them again.
        for file in File::SETTINGS {
            *self.seen_mut(file) = look(&self.dir.join(file.name()));
        }
        Ok(written)
    }

    /// Replaces `secrets.toml`. Written only if its contents change, atomically, and — when Irori
    /// creates it — readable by Irori's own user and nobody else.
    ///
    /// Returns whether it wrote anything.
    pub fn save_secrets(&mut self, secrets: &ExtensionSettings) -> std::io::Result<bool> {
        let text = files::write_secrets(secrets);
        let path = self.path(File::Secrets);
        let changed = !std::fs::read_to_string(&path).is_ok_and(|current| current == text);
        if changed {
            std::fs::create_dir_all(&self.dir)?;
            // The rest of this directory is meant for git; this file mustn't end up there by way
            // of a `git add .`. Only when there's no `.gitignore` at all: one a person wrote is
            // theirs, and they may have their reasons.
            let ignore = self.dir.join(".gitignore");
            if !ignore.exists() {
                std::fs::write(
                    &ignore,
                    "# Written by Irori: secrets.toml holds keys and must never be committed.\n\
                     secrets.toml\n*.toml.writing\n",
                )?;
            }
            let temporary = write_beside(&path, &text, Readable::ByOwnerOnly)?;
            if let Err(e) = std::fs::rename(&temporary, &path) {
                let _ = std::fs::remove_file(&temporary);
                return Err(e);
            }
        }
        self.secrets.value = secrets.clone();
        *self.seen_mut(File::Secrets) = look(&path);
        Ok(changed)
    }
}

/// Who a file Irori writes may be read by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Readable {
    /// Whatever the umask allows, like any other file.
    ByAnyone,
    /// Irori's own user. Set on the file before anything is written into it, so there's no
    /// moment when a secret sits in a file others can open.
    ByOwnerOnly,
}

/// A file's size and modification time, or `None` if it isn't there.
fn look(path: &Path) -> Option<Seen> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(Seen {
        modified: metadata.modified().ok(),
        len: metadata.len(),
    })
}

/// Writes `text` to a temporary file next to `path`, ready to be renamed over it.
///
/// Beside it, not in `/tmp`, on purpose: `rename` is only atomic within one filesystem.
fn write_beside(path: &Path, text: &str, readable: Readable) -> std::io::Result<PathBuf> {
    use std::io::Write as _;

    let temporary = {
        let mut name = path.as_os_str().to_os_string();
        name.push(".writing");
        PathBuf::from(name)
    };
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if readable == Readable::ByOwnerOnly {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = readable;
    let mut file = options.open(&temporary)?;
    // `mode` only applies when the file is created. One left behind by a crash could already
    // exist with wider permissions, so say it again rather than trust whatever was there.
    #[cfg(unix)]
    if readable == Readable::ByOwnerOnly {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    let written = file
        .write_all(text.as_bytes())
        // Rename is atomic, but without this the rename can land before the contents do and a
        // power cut leaves an empty file where the old good one was.
        .and_then(|()| file.sync_all());
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(e);
    }
    Ok(temporary)
}

/// Throws away files that were prepared but never put in place.
fn discard(prepared: &[(File, PathBuf, PathBuf)]) {
    for (_, _, temporary) in prepared {
        // Already renamed, or never created: either way there's nothing to clean up, and a
        // failure to tidy isn't worth reporting over whatever went wrong first.
        let _ = std::fs::remove_file(temporary);
    }
}

#[cfg(test)]
mod tests {
    use irori_types::{AreaId, Name};

    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("a temporary directory")
    }

    fn device(s: &str) -> DeviceId {
        s.parse().expect("a valid device id")
    }

    fn key(s: &str) -> SettingsKey {
        s.parse().expect("a valid settings key")
    }

    fn name(s: &str) -> Name {
        s.parse().expect("a valid name")
    }

    fn area(id: &str, called: &str) -> Area {
        Area {
            id: id.parse::<AreaId>().expect("a valid area id"),
            name: name(called),
            floor_id: None,
        }
    }

    fn named(what: &str) -> DeviceSettings {
        DeviceSettings {
            added: false,
            name: Some(name(what)),
            description: None,
            area: irori_types::Placement::Unsaid,
            ignored: false,
        }
    }

    #[test]
    fn a_directory_that_isnt_there_means_nothing_is_configured() {
        let mut store = Store::new(dir().path().join("never-created"));
        assert!(store.reload().is_empty());
        assert_eq!(store.settings(), Settings::default());
    }

    #[test]
    fn what_is_saved_is_what_is_loaded_again() {
        let home = dir();
        let settings = Settings {
            ask_before_adding: false,
            floors: Vec::new(),
            areas: vec![area("hall", "Hall")],
            floorplan: irori_types::Floorplan::default(),
            devices: [(device("demo_lamp"), named("Reading lamp"))].into(),
            entities: BTreeMap::new(),
        };

        let mut store = Store::new(home.path());
        store.save(&settings).expect("saved");

        let mut fresh = Store::new(home.path());
        assert!(fresh.reload().is_empty());
        assert_eq!(fresh.settings(), settings);
    }

    /// The first save lays out the whole directory, empty files included, so that someone who
    /// opens it can see what they're allowed to write. Saving the same thing again is a no-op:
    /// nothing is rewritten, so nothing looks edited to the reload check or to git.
    #[test]
    fn saving_the_same_settings_twice_touches_nothing() {
        let home = dir();
        let settings = Settings {
            areas: vec![area("hall", "Hall")],
            ..Settings::default()
        };
        let mut store = Store::new(home.path());

        assert_eq!(
            store.save(&settings).expect("saved"),
            [File::Areas, File::Floorplan, File::Devices, File::Entities]
        );
        assert_eq!(store.save(&settings).expect("saved"), Vec::<File>::new());
    }

    /// The point of the whole crate: an edit made in a text editor reaches the running server.
    #[test]
    fn an_edit_on_disk_is_picked_up() {
        let home = dir();
        let mut store = Store::new(home.path());
        store.reload();

        std::fs::write(
            home.path().join("areas.toml"),
            "[areas.kitchen]\nname = \"Kitchen\"\n",
        )
        .expect("written");
        assert!(store.reload().is_empty());
        assert_eq!(store.settings().areas, vec![area("kitchen", "Kitchen")]);
    }

    /// A file that stops parsing halfway through an edit must not empty the home.
    #[test]
    fn a_broken_file_keeps_its_last_good_contents_and_loses_nothing_else() {
        let home = dir();
        let mut store = Store::new(home.path());
        store
            .save(&Settings {
                areas: vec![area("hall", "Hall")],
                devices: [(device("demo_lamp"), named("Reading lamp"))].into(),
                ..Settings::default()
            })
            .expect("saved");

        std::fs::write(home.path().join("areas.toml"), "[areas.hall\nname = ").expect("written");
        let problems = store.reload();

        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].file, File::Areas.name());
        assert_eq!(
            store.settings().areas,
            vec![area("hall", "Hall")],
            "the last good areas stay live"
        );
        assert_eq!(
            store.settings().devices[&device("demo_lamp")],
            named("Reading lamp"),
            "a broken areas.toml says nothing about devices.toml"
        );
    }

    /// Being broken isn't remembered as "read": the fix has to be noticed on the next tick even
    /// if the file ends up the same size (a one-character typo, corrected).
    #[test]
    fn a_broken_file_is_read_again_once_it_is_fixed() {
        let home = dir();
        let areas = home.path().join("areas.toml");
        std::fs::create_dir_all(home.path()).expect("created");
        let mut store = Store::new(home.path());

        std::fs::write(&areas, "[areas.hall]\nname = Hall\n").expect("written");
        assert_eq!(store.reload().len(), 1);

        std::fs::write(&areas, "[areas.hall]\nname = \"Hal\"\n").expect("written");
        assert!(store.reload().is_empty());
        assert_eq!(store.settings().areas, vec![area("hall", "Hal")]);
    }

    #[test]
    fn deleting_a_file_clears_what_it_said() {
        let home = dir();
        let mut store = Store::new(home.path());
        store
            .save(&Settings {
                areas: vec![area("hall", "Hall")],
                ..Settings::default()
            })
            .expect("saved");

        std::fs::remove_file(home.path().join("areas.toml")).expect("removed");
        assert!(store.reload().is_empty());
        assert!(store.settings().areas.is_empty());
    }

    /// A save that can't be finished must change nothing, or the watcher would find half of it
    /// on the next tick and adopt a state nobody asked for.
    ///
    /// Provoked by putting a directory where the last file's working file needs to go, so that
    /// two of the three are prepared and the third can't be. It stands in for the real reasons a
    /// write fails part-way — a full disk, a read-only mount — which a test can't arrange.
    #[test]
    fn a_save_that_cant_finish_leaves_the_directory_as_it_was() {
        let home = dir();
        let mut store = Store::new(home.path());
        store
            .save(&Settings {
                areas: vec![area("hall", "Hall")],
                ..Settings::default()
            })
            .expect("saved");

        // `entities.toml` is the last of the three, so a failure on it is exactly the case where
        // the two before it would already have been changed.
        std::fs::create_dir(home.path().join("entities.toml.writing")).expect("in the way");
        let before: Vec<String> = File::SETTINGS
            .iter()
            .map(|file| std::fs::read_to_string(home.path().join(file.name())).expect("readable"))
            .collect();

        let refused = store.save(&Settings {
            ask_before_adding: false,
            floors: Vec::new(),
            areas: vec![area("kitchen", "Kitchen")],
            floorplan: irori_types::Floorplan::default(),
            devices: [(device("demo_lamp"), named("Reading lamp"))].into(),
            entities: [(
                key("demo/lamp-light"),
                EntitySettings {
                    name: Some(name("Reading light")),
                },
            )]
            .into(),
        });

        assert!(refused.is_err(), "the save should have failed");
        for (file, was) in File::SETTINGS.iter().zip(before) {
            assert_eq!(
                std::fs::read_to_string(home.path().join(file.name())).expect("readable"),
                was,
                "{file} changed despite the save failing"
            );
        }
        assert!(
            !home.path().join("areas.toml.writing").exists(),
            "a prepared file was left behind"
        );
    }

    fn esphome_key(value: &str) -> ExtensionSettings {
        let mut secrets = ExtensionSettings::default();
        secrets
            .set(
                &"esphome".parse().expect("a valid id"),
                &["keys".to_owned(), "00:11:22:33:44:55".to_owned()],
                value.to_owned(),
            )
            .expect("set");
        secrets
    }

    #[test]
    fn secrets_are_saved_and_read_back_like_everything_else() {
        let home = dir();
        let mut store = Store::new(home.path());
        assert!(store.save_secrets(&esphome_key("a2V5")).expect("saved"));
        assert!(
            !store.save_secrets(&esphome_key("a2V5")).expect("saved"),
            "unchanged"
        );

        let mut fresh = Store::new(home.path());
        assert!(fresh.reload().is_empty());
        assert_eq!(fresh.extension_settings(), esphome_key("a2V5"));
    }

    /// A config directory is meant to be kept in git, and a `git add .` there must not take the
    /// keys with it. A `.gitignore` somebody already wrote is left exactly as it is.
    #[test]
    fn secrets_are_kept_out_of_git_without_overriding_a_persons_own_gitignore() {
        let home = dir();
        let mut store = Store::new(home.path());
        store.save_secrets(&esphome_key("a2V5")).expect("saved");
        let ignore = std::fs::read_to_string(home.path().join(".gitignore")).expect("written");
        assert!(
            ignore.lines().any(|line| line == "secrets.toml"),
            "{ignore}"
        );

        let theirs = dir();
        std::fs::write(theirs.path().join(".gitignore"), "mine\n").expect("written");
        let mut store = Store::new(theirs.path());
        store.save_secrets(&esphome_key("a2V5")).expect("saved");
        assert_eq!(
            std::fs::read_to_string(theirs.path().join(".gitignore")).expect("readable"),
            "mine\n"
        );
    }

    /// Somebody else on the machine shouldn't be able to read a key just because the umask is
    /// generous.
    #[cfg(unix)]
    #[test]
    fn a_secrets_file_irori_writes_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let home = dir();
        let mut store = Store::new(home.path());
        store.save_secrets(&esphome_key("a2V5")).expect("saved");
        let mode = std::fs::metadata(home.path().join("secrets.toml"))
            .expect("written")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    }

    /// A key edited by hand reaches the running server; a broken edit keeps the last good keys.
    #[test]
    fn a_secrets_file_edited_by_hand_is_picked_up_and_a_broken_one_is_survived() {
        let home = dir();
        let mut store = Store::new(home.path());
        store.reload();
        std::fs::write(
            home.path().join("secrets.toml"),
            "[esphome.keys]\n\"00:11:22:33:44:55\" = \"a2V5\"\n",
        )
        .expect("written");
        assert!(store.reload().is_empty());
        assert_eq!(store.extension_settings(), esphome_key("a2V5"));

        std::fs::write(home.path().join("secrets.toml"), "[esphome.keys\n").expect("written");
        assert_eq!(store.reload().len(), 1);
        assert_eq!(store.extension_settings(), esphome_key("a2V5"));
    }

    /// A rename must not leave the working file behind: the directory a person opens should have
    /// exactly the files the spec describes.
    #[test]
    fn writing_leaves_no_temporary_files_behind() {
        let home = dir();
        let mut store = Store::new(home.path());
        store
            .save(&Settings {
                areas: vec![area("hall", "Hall")],
                ..Settings::default()
            })
            .expect("saved");

        let mut left: Vec<String> = std::fs::read_dir(home.path())
            .expect("readable")
            .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "areas.toml",
                "devices.toml",
                "entities.toml",
                "floorplan.toml"
            ]
        );
    }

    /// An extension's settings come from its own file joined with its secrets; editing the file
    /// by hand is picked up, and deleting it takes its settings away.
    #[test]
    fn an_extensions_file_is_joined_with_its_secrets() {
        let home = dir();
        let helpers: ExtensionId = "helpers".parse().expect("valid");
        let mut store = Store::new(home.path());
        let mut file = serde_json::Map::new();
        file.insert(
            "toggles".into(),
            serde_json::json!({"guests": {"name": "Guests"}}),
        );
        store.save_extension(&helpers, &file).expect("saved");
        std::fs::write(
            home.path().join("secrets.toml"),
            "[helpers]\ntoken = \"s3cret\"\n",
        )
        .expect("written");

        let mut fresh = Store::new(home.path());
        assert!(fresh.reload().is_empty());
        assert_eq!(
            fresh.extension_settings().of(&helpers),
            serde_json::json!({"toggles": {"guests": {"name": "Guests"}}, "token": "s3cret"})
        );

        std::fs::write(
            home.path().join("extensions/helpers.toml"),
            "token = \"clash\"\n",
        )
        .expect("written");
        let problems = fresh.reload();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].reason.contains("secrets.toml"),
            "{}",
            problems[0].reason
        );
        assert_eq!(fresh.extension_settings().of(&helpers)["token"], "s3cret");
        assert!(fresh.reload().is_empty(), "said once, not on every check");

        std::fs::remove_file(home.path().join("extensions/helpers.toml")).expect("removed");
        fresh.reload();
        assert_eq!(
            fresh.extension_settings().of(&helpers),
            serde_json::json!({"token": "s3cret"})
        );
    }

    /// A listing error is not "the directory is empty": last-good helper definitions must stay,
    /// or a permission blip would restart the extension with nothing.
    ///
    /// The directory is replaced by an ordinary file rather than made unreadable, which is the
    /// other way `read_dir` fails with something that isn't `NotFound` — the branch under test.
    /// Taking the read permission away would be the more obvious setup and doesn't work
    /// everywhere: **root ignores permission bits**, so that version of this test passes on a
    /// laptop and fails in a container, where tests run as root (`dev/pi check`).
    #[test]
    fn an_unlistable_extensions_dir_keeps_the_last_good_settings() {
        let home = dir();
        let helpers: ExtensionId = "helpers".parse().expect("valid");
        let mut store = Store::new(home.path());
        let mut file = serde_json::Map::new();
        file.insert(
            "toggles".into(),
            serde_json::json!({"guests": {"name": "Guests"}}),
        );
        store.save_extension(&helpers, &file).expect("saved");

        let dir = home.path().join("extensions");
        std::fs::remove_dir_all(&dir).expect("removed");
        std::fs::write(&dir, "not a directory").expect("written");

        let problems = store.reload();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].file, "extensions");
        assert_eq!(
            store.extension_settings().of(&helpers)["toggles"]["guests"]["name"],
            "Guests"
        );
    }

    /// areas.toml with a dangling floor reference still loads (the warning is in reload).
    #[test]
    fn a_room_on_a_missing_floor_reloads() {
        let home = dir();
        std::fs::write(
            home.path().join("areas.toml"),
            "[areas.hall]\nname = \"Hall\"\nfloor = \"upstairs\"\n",
        )
        .expect("written");
        let mut store = Store::new(home.path());
        assert!(store.reload().is_empty());
        assert_eq!(store.settings().areas.len(), 1);
        assert_eq!(
            store.settings().areas[0]
                .floor_id
                .as_ref()
                .map(|id| id.as_str()),
            Some("upstairs")
        );
        assert!(store.settings().floors.is_empty());
    }
}
