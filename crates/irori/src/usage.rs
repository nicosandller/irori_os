//! What is using the machine, for the meters in Settings: which processes hold the memory and
//! the processor, and what Irori's own data directory is made of.
//!
//! Asked for only while a meter is open. Unlike `host_info`, this does list every process —
//! that is the question — and it takes two looks a moment apart, because how busy a process is
//! is only ever the difference between two readings.

use std::path::Path;

use serde::Serialize;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

/// How many of each are worth a row. Past these it's a list nobody reads.
const PROCESSES: usize = 8;
const FOLDERS: usize = 8;

/// The most files a folder is counted through. A directory tree this size is somebody's
/// mistake, and Settings shouldn't hang on it.
const FILES_COUNTED: usize = 500_000;

#[derive(Debug, Serialize)]
pub struct UsageView {
    /// The processes using the most memory and the most processor, each once.
    pub processes: Vec<ProcessView>,
    /// What's in the data directory, biggest first.
    pub storage: Vec<StorageView>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProcessView {
    pub name: String,
    pub pid: u32,
    /// Memory it holds, in bytes.
    pub memory: u64,
    /// Its share of the whole machine's processor, 0–100, over the moment it was watched.
    pub cpu: f32,
    /// Whether it is Irori itself.
    pub own: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StorageView {
    pub name: String,
    pub bytes: u64,
}

/// Looks at the machine twice, a moment apart. Blocks for that moment: call it off the
/// async runtime.
pub fn read(database: &Path) -> UsageView {
    let what = ProcessRefreshKind::nothing()
        .with_memory()
        .with_cpu()
        .without_tasks();
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, what);
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, what);

    let cores = std::thread::available_parallelism().map_or(1.0, |cores| cores.get() as f32);
    let own = sysinfo::get_current_pid().ok();
    let all: Vec<ProcessView> = system
        .processes()
        .iter()
        .map(|(pid, process)| ProcessView {
            name: process.name().to_string_lossy().into_owned(),
            pid: pid.as_u32(),
            memory: process.memory(),
            // sysinfo counts one busy core as 100.
            cpu: process.cpu_usage() / cores,
            own: Some(*pid) == own,
        })
        .collect();

    UsageView {
        processes: heaviest(all, PROCESSES),
        storage: database
            .parent()
            .map(|data| storage(data, FOLDERS))
            .unwrap_or_default(),
    }
}

/// The `most` biggest by memory and the `most` busiest, each process once, biggest first.
fn heaviest(mut all: Vec<ProcessView>, most: usize) -> Vec<ProcessView> {
    all.sort_by(|a, b| b.memory.cmp(&a.memory).then(a.pid.cmp(&b.pid)));
    let mut kept: Vec<ProcessView> = all.iter().take(most).cloned().collect();
    all.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(a.pid.cmp(&b.pid)));
    for process in all.into_iter().take(most) {
        // One that is doing nothing isn't among the busiest, however short the list.
        if process.cpu > 0.0 && !kept.iter().any(|known| known.pid == process.pid) {
            kept.push(process);
        }
    }
    kept
}

/// What a data directory is made of: each thing in it with its size, biggest first, the
/// database's files as one, and whatever is past `most` rows added up as the last.
fn storage(data: &Path, most: usize) -> Vec<StorageView> {
    let Ok(entries) = std::fs::read_dir(data) else {
        return Vec::new();
    };
    let mut left = FILES_COUNTED;
    let mut database = 0;
    let mut things = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let bytes = size(&entry.path(), &mut left);
        if name.starts_with("irori.db") {
            database += bytes;
        } else if bytes > 0 {
            things.push(StorageView { name, bytes });
        }
    }
    if database > 0 {
        things.push(StorageView {
            name: "Database".to_owned(),
            bytes: database,
        });
    }
    things.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.name.cmp(&b.name)));
    if things.len() > most {
        let rest: u64 = things.drain(most..).map(|thing| thing.bytes).sum();
        things.push(StorageView {
            name: "Everything else".to_owned(),
            bytes: rest,
        });
    }
    things
}

/// A file's size, or everything under a directory added up. Links aren't followed, so a link
/// to somewhere big isn't counted as being here, and a loop can't be walked forever.
fn size(path: &Path, left: &mut usize) -> u64 {
    if *left == 0 {
        return 0;
    }
    *left -= 1;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| size(&entry.path(), left))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, memory: u64, cpu: f32) -> ProcessView {
        ProcessView {
            name: format!("p{pid}"),
            pid,
            memory,
            cpu,
            own: false,
        }
    }

    #[test]
    fn the_heaviest_are_the_biggest_and_the_busiest_each_once() {
        let all = vec![
            process(1, 900, 0.0),
            process(2, 800, 50.0),
            process(3, 10, 90.0),
            process(4, 5, 0.0),
        ];
        let kept: Vec<u32> = heaviest(all, 2).into_iter().map(|p| p.pid).collect();
        // The two biggest, then the busiest that wasn't already there. The idle small one is
        // neither.
        assert_eq!(kept, [1, 2, 3]);
    }

    #[test]
    fn a_data_directory_is_weighed_thing_by_thing() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let data = dir.path();
        std::fs::write(data.join("irori.db"), [0; 100])?;
        std::fs::write(data.join("irori.db-wal"), [0; 50])?;
        std::fs::create_dir_all(data.join("extensions/demo"))?;
        std::fs::write(data.join("extensions/demo/bin"), [0; 400])?;
        std::fs::write(data.join("extensions/readme"), [0; 20])?;
        std::fs::write(data.join("ollama.log"), [0; 30])?;
        std::fs::write(data.join("empty"), [])?;
        let all = storage(data, 8);
        let said: Vec<(&str, u64)> = all.iter().map(|s| (s.name.as_str(), s.bytes)).collect();
        assert_eq!(
            said,
            [("extensions", 420), ("Database", 150), ("ollama.log", 30)]
        );
        // Past the rows there's room for, the rest is one line.
        let short = storage(data, 1);
        let said: Vec<(&str, u64)> = short.iter().map(|s| (s.name.as_str(), s.bytes)).collect();
        assert_eq!(said, [("extensions", 420), ("Everything else", 180)]);
        Ok(())
    }

    #[test]
    fn this_process_is_among_what_is_running() {
        let usage = read(Path::new("./irori.db"));
        assert!(!usage.processes.is_empty());
        assert!(usage.processes.iter().all(|process| process.cpu >= 0.0));
    }
}
