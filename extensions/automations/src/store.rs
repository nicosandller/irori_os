//! Where flows and what they leave behind are kept (`docs/specs/flows.md` §2, §5, §6).
//!
//! - **Flows**: `flows/<id>.json` in the config directory, hand-editable and git-friendly. A file
//!   that fails to parse is reported and the last good version of it stays in use; a changed
//!   file is noticed by polling. Writes are atomic.
//! - **Versions, runs, near-misses**: in the extension's private state directory. Runs and
//!   near-misses are JSON Lines, trimmed to the last [`RUNS_KEPT`] / [`NEAR_MISSES_KEPT`] per
//!   flow.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use irori_flow_types::Flow;
use irori_flow_types::api::VersionEntry;
use irori_flow_types::trace::{NearMiss, RunRecord};
use irori_types::{RuleId, Timestamp};
use serde::de::DeserializeOwned;

/// Runs kept per flow.
pub const RUNS_KEPT: usize = 200;
/// Near-misses kept per flow.
pub const NEAR_MISSES_KEPT: usize = 100;
/// A flow's saved test settings, at most.
const TEST_SETTINGS_MAX: usize = 16 * 1024;

/// A flow file that couldn't be read, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileProblem {
    pub file: String,
    pub reason: String,
}

#[derive(Debug)]
pub struct Store {
    flows_dir: PathBuf,
    data_dir: PathBuf,
    /// The last good version of each file, with when it was last seen changed.
    flows: BTreeMap<RuleId, (Flow, Option<SystemTime>)>,
    problems: Vec<FileProblem>,
    /// What each file's modification time was when last read, to notice edits.
    seen: BTreeMap<PathBuf, Option<SystemTime>>,
}

impl Store {
    pub fn open(flows_dir: PathBuf, data_dir: PathBuf) -> Self {
        let mut store = Self {
            flows_dir,
            data_dir,
            flows: BTreeMap::new(),
            problems: Vec::new(),
            seen: BTreeMap::new(),
        };
        store.reload();
        store
    }

    pub fn flows(&self) -> impl Iterator<Item = &Flow> {
        self.flows.values().map(|(flow, _)| flow)
    }

    pub fn flow(&self, id: &RuleId) -> Option<&Flow> {
        self.flows.get(id).map(|(flow, _)| flow)
    }

    pub fn problems(&self) -> &[FileProblem] {
        &self.problems
    }

    /// Whether any flow file changed on disk since the last look; reloads if so.
    pub fn poll(&mut self) -> bool {
        let now = listing(&self.flows_dir);
        if now == self.seen {
            return false;
        }
        self.reload();
        true
    }

    /// Reads every flow file again. A file that fails keeps its last good version.
    pub fn reload(&mut self) {
        self.problems.clear();
        let listing = listing(&self.flows_dir);
        if listing.is_empty()
            && self.flows_dir.exists()
            && std::fs::read_dir(&self.flows_dir).is_err()
        {
            self.problems.push(FileProblem {
                file: self.flows_dir.display().to_string(),
                reason: "can't read this folder; the flows already loaded keep running".into(),
            });
            return;
        }
        let mut next = BTreeMap::new();
        for (path, modified) in &listing {
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let file = format!(
                "flows/{}",
                path.file_name().and_then(|n| n.to_str()).unwrap_or(stem)
            );
            let Ok(id) = RuleId::try_from(stem) else {
                self.problems.push(FileProblem {
                    file,
                    reason: "the file name has to be a flow id: lowercase letters, digits and _"
                        .into(),
                });
                continue;
            };
            match read_flow(path, &id) {
                Ok(flow) => {
                    next.insert(id, (flow, *modified));
                }
                Err(reason) => {
                    self.problems.push(FileProblem { file, reason });
                    // The last good version keeps running.
                    if let Some(old) = self.flows.remove(&id) {
                        next.insert(id, old);
                    }
                }
            }
        }
        self.flows = next;
        self.seen = listing;
    }

    /// Writes a flow to its file, atomically, and keeps a snapshot of its version.
    pub fn save(&mut self, flow: &Flow, now: Timestamp) -> Result<(), String> {
        std::fs::create_dir_all(&self.flows_dir)
            .map_err(|e| format!("couldn't create {}: {e}", self.flows_dir.display()))?;
        let path = self.flows_dir.join(format!("{}.json", flow.id));
        let text = serde_json::to_string_pretty(flow).map_err(|e| e.to_string())?;
        write_atomically(&path, text.as_bytes())?;
        self.keep_version(flow, now)?;
        self.reload();
        Ok(())
    }

    pub fn delete(&mut self, id: &RuleId) -> Result<(), String> {
        let path = self.flows_dir.join(format!("{id}.json"));
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("couldn't delete {}: {e}", path.display())),
        }
        // Its versions and runs stay, to look back on; how it was tested goes with it.
        let _ = std::fs::remove_file(self.tests_file(id));
        self.reload();
        Ok(())
    }

    fn versions_dir(&self, id: &RuleId) -> PathBuf {
        self.data_dir.join("versions").join(id.as_str())
    }

    /// Keeps this definition's snapshot, unless it's already kept.
    pub fn keep_version(&self, flow: &Flow, now: Timestamp) -> Result<(), String> {
        let dir = self.versions_dir(&flow.id);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{}.json", flow.version()));
        if path.exists() {
            return Ok(());
        }
        let entry = VersionEntry {
            version: flow.version(),
            saved_at: now,
            flow: Some(flow.clone()),
        };
        let text = serde_json::to_string(&entry).map_err(|e| e.to_string())?;
        write_atomically(&path, text.as_bytes())
    }

    /// Every kept version of a flow, newest first, without their definitions.
    pub fn versions(&self, id: &RuleId) -> Vec<VersionEntry> {
        let mut entries: Vec<VersionEntry> = std::fs::read_dir(self.versions_dir(id))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| read_json::<VersionEntry>(&entry.path()).ok())
            .map(|mut entry| {
                entry.flow = None;
                entry
            })
            .collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.saved_at));
        entries
    }

    pub fn version(&self, id: &RuleId, version: &str) -> Option<VersionEntry> {
        if !version.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        read_json(&self.versions_dir(id).join(format!("{version}.json"))).ok()
    }

    fn tests_file(&self, id: &RuleId) -> PathBuf {
        self.data_dir.join("tests").join(format!("{id}.json"))
    }

    /// How the page last set up a test of this flow, so it can be run again as it was. What's in
    /// it is the page's business; it's kept as given.
    pub fn test_settings(&self, id: &RuleId) -> Option<serde_json::Value> {
        read_json(&self.tests_file(id)).ok()
    }

    pub fn keep_test_settings(
        &self,
        id: &RuleId,
        settings: &serde_json::Value,
    ) -> Result<(), String> {
        let text = serde_json::to_string(settings).map_err(|e| e.to_string())?;
        if text.len() > TEST_SETTINGS_MAX {
            return Err(format!(
                "test settings are kept up to {} KB",
                TEST_SETTINGS_MAX / 1024
            ));
        }
        let path = self.tests_file(id);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        write_atomically(&path, text.as_bytes())
    }

    fn log(&self, kind: &str, id: &RuleId) -> PathBuf {
        self.data_dir.join(kind).join(format!("{id}.jsonl"))
    }

    pub fn add_run(&self, run: &RunRecord) {
        append(&self.log("runs", &run.flow_id), run, RUNS_KEPT);
    }

    pub fn add_near_miss(&self, miss: &NearMiss) {
        append(
            &self.log("near_misses", &miss.flow_id),
            miss,
            NEAR_MISSES_KEPT,
        );
    }

    /// A flow's kept runs, newest first.
    pub fn runs(&self, id: &RuleId) -> Vec<RunRecord> {
        let mut runs: Vec<RunRecord> = read_lines(&self.log("runs", id));
        runs.reverse();
        runs
    }

    /// A flow's kept near-misses, newest first.
    pub fn near_misses(&self, id: &RuleId) -> Vec<NearMiss> {
        let mut misses: Vec<NearMiss> = read_lines(&self.log("near_misses", id));
        misses.reverse();
        misses
    }
}

/// The flow files in `dir`, with their modification times.
fn listing(dir: &Path) -> BTreeMap<PathBuf, Option<SystemTime>> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .map(|path| {
            let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            (path, modified)
        })
        .collect()
}

fn read_flow(path: &Path, id: &RuleId) -> Result<Flow, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("can't read it: {e}"))?;
    let flow: Flow = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if &flow.id != id {
        return Err(format!(
            "its id is \"{}\"; the file name has to match the id, like flows/{}.json",
            flow.id, flow.id
        ));
    }
    Ok(flow)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn read_lines<T: DeserializeOwned>(path: &Path) -> Vec<T> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Adds one line; when the file holds twice what's kept, rewrites it with the newest `kept`.
fn append<T: serde::Serialize + DeserializeOwned>(path: &Path, value: &T, kept: usize) {
    let Ok(line) = serde_json::to_string(value) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let appended = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| writeln!(file, "{line}"));
    if let Err(error) = appended {
        tracing::warn!(path = %path.display(), %error, "couldn't keep a record");
        return;
    }
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > kept * 2 {
        let newest = lines[lines.len() - kept..].join("\n") + "\n";
        let _ = write_atomically(path, newest.as_bytes());
    }
}

/// Writes beside the target, syncs, and renames over it: a crash leaves the old file or the
/// new one, never half of either.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let writing = path.with_extension("writing");
    let mut file = std::fs::File::create(&writing)
        .map_err(|e| format!("couldn't write {}: {e}", writing.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("couldn't write {}: {e}", writing.display()))?;
    std::fs::rename(&writing, path).map_err(|e| format!("couldn't write {}: {e}", path.display()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn flow(id: &str, name: &str) -> Flow {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": name,
            "nodes": { "start": { "type": "trigger", "trigger": { "type": "startup" } } }
        }))
        .unwrap()
    }

    fn now() -> Timestamp {
        "2026-09-29T20:00:00Z".parse().unwrap()
    }

    #[test]
    fn a_broken_edit_keeps_the_last_good_flow_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("flows"), dir.path().join("data"));
        store.save(&flow("hall", "Hall"), now()).unwrap();
        assert_eq!(store.flows().count(), 1);

        std::fs::write(dir.path().join("flows/hall.json"), "{ not json").unwrap();
        store.reload();
        assert_eq!(
            store.flow(&"hall".parse().unwrap()).unwrap().name.as_str(),
            "Hall"
        );
        assert_eq!(store.problems().len(), 1);
        assert_eq!(store.problems()[0].file, "flows/hall.json");

        std::fs::write(
            dir.path().join("flows/other.json"),
            serde_json::to_string(&flow("hall", "Hall")).unwrap(),
        )
        .unwrap();
        store.reload();
        assert!(
            store
                .problems()
                .iter()
                .any(|p| p.reason.contains("has to match the id"))
        );
    }

    #[test]
    fn versions_are_kept_once_each_and_runs_are_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("flows"), dir.path().join("data"));
        let id: RuleId = "hall".parse().unwrap();
        store.save(&flow("hall", "Hall"), now()).unwrap();
        store.save(&flow("hall", "Hall"), now()).unwrap();
        store.save(&flow("hall", "Hallway"), now()).unwrap();
        assert_eq!(store.versions(&id).len(), 2);
        let version = flow("hall", "Hall").version();
        assert!(store.version(&id, &version).unwrap().flow.is_some());
        assert!(store.version(&id, "../../etc").is_none());
    }

    /// The folder can't be read: what's loaded keeps running, and it says so. Root reads
    /// anything, so this can only be seen as another user (dev/pi runs its tests as root).
    #[test]
    #[cfg(unix)]
    fn an_unreadable_folder_keeps_what_was_loaded() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("flows"), dir.path().join("data"));
        store.save(&flow("hall", "Hall"), now()).unwrap();
        let flows = dir.path().join("flows");
        std::fs::set_permissions(&flows, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read_dir(&flows).is_ok() {
            // Running as root: permissions don't stop us, so there's nothing to see.
            std::fs::set_permissions(&flows, std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        store.reload();
        std::fs::set_permissions(&flows, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(store.flows().count(), 1);
        assert!(
            store.problems()[0]
                .reason
                .contains("can't read this folder")
        );
    }
}
