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
    Area, DeviceId, DeviceSettings, EntitySettings, ExtensionSettings, Floor, Settings, SettingsKey,
};

pub use files::{ExtensionsSection, File, IroriSettings, LogLevel, ServerSettings};

/// Something wrong with one file, to be logged and shown. Never fatal: the file keeps whatever it
/// last held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub file: File,
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
    devices: Part<BTreeMap<DeviceId, DeviceSettings>>,
    entities: Part<BTreeMap<SettingsKey, EntitySettings>>,
    secrets: Part<ExtensionSettings>,
}

impl Store {
    /// Points at a directory without reading it. A directory that doesn't exist is not an error:
    /// it means nothing has been configured, and it's created on the first write.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            irori: Part::default(),
            areas: Part::default(),
            devices: Part::default(),
            entities: Part::default(),
            secrets: Part::default(),
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
            devices: self.devices.value.clone(),
            entities: self.entities.value.clone(),
        }
    }

    /// Settings for Irori itself, from `irori.toml`.
    pub fn irori(&self) -> IroriSettings {
        self.irori.value.clone()
    }

    /// Each extension's settings: its table in `secrets.toml`.
    pub fn extension_settings(&self) -> ExtensionSettings {
        self.secrets.value.clone()
    }

    /// Re-reads whatever has changed on disk since the last call, and says what went wrong.
    ///
    /// The first call reads everything. After that an untouched file costs one `stat`, which is
    /// what makes polling every couple of seconds reasonable.
    pub fn reload(&mut self) -> Vec<Problem> {
        let mut problems = Vec::new();
        for file in File::ALL {
            if let Err(reason) = self.reload_one(file) {
                problems.push(Problem { file, reason });
            }
        }
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
            File::Devices => self.devices.seen.as_ref(),
            File::Entities => self.entities.seen.as_ref(),
            File::Secrets => self.secrets.seen.as_ref(),
        }
    }

    fn seen_mut(&mut self, file: File) -> &mut Option<Seen> {
        match file {
            File::Irori => &mut self.irori.seen,
            File::Areas => &mut self.areas.seen,
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

    let temporary = path.with_extension("toml.writing");
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
            floors: Vec::new(),
            areas: vec![area("hall", "Hall")],
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
            [File::Areas, File::Devices, File::Entities]
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
        assert_eq!(problems[0].file, File::Areas);
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
            floors: Vec::new(),
            areas: vec![area("kitchen", "Kitchen")],
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
                &["keys".to_owned(), "30:83:98:CA:6A:08".to_owned()],
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
            "[esphome.keys]\n\"30:83:98:CA:6A:08\" = \"a2V5\"\n",
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
        assert_eq!(left, ["areas.toml", "devices.toml", "entities.toml"]);
    }
}
