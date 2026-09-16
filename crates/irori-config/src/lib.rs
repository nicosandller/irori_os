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

use irori_types::{Area, DeviceSettings, EntitySettings, Settings, SettingsKey};

pub use files::File;

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
    areas: Part<Vec<Area>>,
    devices: Part<BTreeMap<SettingsKey, DeviceSettings>>,
    entities: Part<BTreeMap<SettingsKey, EntitySettings>>,
}

impl Store {
    /// Points at a directory without reading it. A directory that doesn't exist is not an error:
    /// it means nothing has been configured, and it's created on the first write.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            areas: Part::default(),
            devices: Part::default(),
            entities: Part::default(),
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
            areas: self.areas.value.clone(),
            devices: self.devices.value.clone(),
            entities: self.entities.value.clone(),
        }
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
            File::Areas => self.areas.value = files::read_areas(&text)?,
            File::Devices => self.devices.value = files::read_devices(&text)?,
            File::Entities => self.entities.value = files::read_entities(&text)?,
        }
        // Only once it parsed: a file that is being edited and saved half-written should be
        // re-read on the next tick, not remembered as good.
        *self.seen_mut(file) = now;
        Ok(())
    }

    fn seen(&self, file: File) -> Option<&Seen> {
        match file {
            File::Areas => self.areas.seen.as_ref(),
            File::Devices => self.devices.seen.as_ref(),
            File::Entities => self.entities.seen.as_ref(),
        }
    }

    fn seen_mut(&mut self, file: File) -> &mut Option<Seen> {
        match file {
            File::Areas => &mut self.areas.seen,
            File::Devices => &mut self.devices.seen,
            File::Entities => &mut self.entities.seen,
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
        for file in File::ALL {
            let text = files::write(file, settings);
            let path = self.path(file);
            if std::fs::read_to_string(&path).is_ok_and(|current| current == text) {
                continue;
            }
            std::fs::create_dir_all(&self.dir)?;
            match write_beside(&path, &text) {
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
        self.areas.value = settings.areas.clone();
        self.devices.value = settings.devices.clone();
        self.entities.value = settings.entities.clone();
        // The files on disk are now these settings, so the next reload must not treat Irori's
        // own write as an outside edit and parse them again.
        for file in File::ALL {
            *self.seen_mut(file) = look(&self.dir.join(file.name()));
        }
        Ok(written)
    }
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
fn write_beside(path: &Path, text: &str) -> std::io::Result<PathBuf> {
    use std::io::Write as _;

    let temporary = path.with_extension("toml.writing");
    let mut file = std::fs::File::create(&temporary)?;
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
            area: irori_types::Placement::Unsaid,
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
            areas: vec![area("hall", "Hall")],
            devices: [(key("demo/lamp"), named("Reading lamp"))].into(),
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
                devices: [(key("demo/lamp"), named("Reading lamp"))].into(),
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
            store.settings().devices[&key("demo/lamp")],
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
        let before: Vec<String> = File::ALL
            .iter()
            .map(|file| std::fs::read_to_string(home.path().join(file.name())).expect("readable"))
            .collect();

        let refused = store.save(&Settings {
            areas: vec![area("kitchen", "Kitchen")],
            devices: [(key("demo/lamp"), named("Reading lamp"))].into(),
            entities: [(
                key("demo/lamp-light"),
                EntitySettings {
                    name: Some(name("Reading light")),
                },
            )]
            .into(),
        });

        assert!(refused.is_err(), "the save should have failed");
        for (file, was) in File::ALL.iter().zip(before) {
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
