//! The machine running the instance, for the System menu in Settings.
//!
//! Read once, each time the Settings page asks: nothing here is kept, so the page always gets the
//! machine as it is. The values come from `sysinfo`, which reads the same places a system monitor
//! does — `sysctl` on macOS, `/proc` on Linux — and deliberately asks for no more than the page
//! shows, so a slow bit of the OS is never pulled in for the sake of a row nobody is looking at.

use std::path::Path;

use serde::Serialize;
use sysinfo::{
    CpuRefreshKind, Disks, MemoryRefreshKind, ProcessRefreshKind, ProcessesToUpdate, RefreshKind,
    System,
};

/// What the System menu in Settings shows about the machine running Irori.
#[derive(Debug, Serialize)]
pub struct HostView {
    /// The host's name on the network, e.g. "studio". `None` when the OS won't say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The operating system, e.g. "macOS" or "Debian GNU/Linux".
    pub os: String,
    /// The OS's own words for its version, e.g. "15.1.1".
    pub os_version: String,
    /// The kernel the OS is running, e.g. "24.3.0".
    pub kernel: String,
    /// The architecture this build was made for, e.g. "aarch64".
    pub arch: &'static str,
    /// A CPU model name. Vendors load these with clock speeds, so the `@ 3.20GHz` tail is dropped.
    pub cpu: String,
    /// The number of physical cores.
    pub cpu_cores: usize,
    /// Every core the OS schedules on, hyperthreads included: what a load average is read against.
    pub cpu_logical: usize,
    /// How many processes wanted a core, averaged over the last one, five and fifteen minutes.
    /// All zero where the OS keeps no such count.
    pub load_average: [f64; 3],
    /// RAM, in bytes.
    pub memory_total: u64,
    pub memory_used: u64,
    /// What Irori's own process holds of it.
    pub process_memory: u64,
    /// Swap: memory moved out to disk to make room. Zero total on a machine without any.
    pub swap_total: u64,
    pub swap_used: u64,
    /// How long the machine has been up. Measured in what the OS reports, not Irori's own uptime,
    /// which is the "Uptime" row on the Instance card.
    pub uptime_secs: u64,
    /// The filesystem that holds the instance's data: the volume the data directory is on.
    pub disk: DiskView,
    /// Every volume mounted on the machine, the data's among them, by mount point.
    pub disks: Vec<DiskView>,
    /// Where the instance keeps its data.
    pub data_dir: String,
    /// The database on disk, with what it has yet to fold in (its write-ahead log).
    pub database_bytes: u64,
}

/// One disk: enough for "is the volume getting full?" without implying anything finer.
#[derive(Debug, Clone, Serialize)]
pub struct DiskView {
    /// Where the volume is mounted, e.g. "/" or "/System/Volumes/Data".
    pub mount: String,
    pub total: u64,
    pub available: u64,
    pub used: u64,
    /// The device it's on, as the OS names it, for telling one pool of space from another.
    #[serde(skip)]
    device: String,
}

/// Reads the machine, given where the instance keeps its data so the right volume is reported.
pub fn read(data: &Path) -> HostView {
    let mut system = System::new_with_specifics(
        RefreshKind::nothing()
            .with_memory(MemoryRefreshKind::everything())
            .with_cpu(CpuRefreshKind::everything()),
    );
    // Just this process, and just its memory: listing every process is the slow bit of the OS
    // this module stays away from.
    let process_memory = sysinfo::get_current_pid().map_or(0, |pid| {
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            false,
            ProcessRefreshKind::nothing().with_memory(),
        );
        system.process(pid).map_or(0, sysinfo::Process::memory)
    });
    let load = System::load_average();
    let cpu = system.cpus().first().map_or_else(String::new, |cpu| {
        let brand = cpu.brand().trim();
        // "Intel(R) Core(TM) i7-8700 CPU @ 3.20GHz" keeps more than the model itself: the clock
        // changes per machine and the model is what a person was choosing between. Everything
        // after " @ " is that tail.
        brand
            .split_once(" @ ")
            .map_or(brand, |(model, _)| model)
            .trim()
            .to_owned()
    });
    let cpu_cores = System::physical_core_count().unwrap_or(0);
    let (memory_total, memory_used) = memory(&system);
    let disks = Disks::new_with_refreshed_list();
    let on = data_disk(&disks, data);

    HostView {
        host: System::host_name(),
        os: System::name().unwrap_or_else(|| std::env::consts::OS.to_owned()),
        os_version: System::long_os_version().unwrap_or_default(),
        kernel: System::kernel_version().unwrap_or_default(),
        arch: std::env::consts::ARCH,
        cpu,
        cpu_cores,
        cpu_logical: system.cpus().len(),
        load_average: [load.one, load.five, load.fifteen],
        memory_total,
        memory_used,
        process_memory,
        swap_total: system.total_swap(),
        swap_used: system.used_swap(),
        uptime_secs: System::uptime(),
        disks: volumes(&disks, on.clone()),
        data_dir: data.parent().unwrap_or(data).to_string_lossy().into_owned(),
        database_bytes: database_bytes(data),
        disk: on.unwrap_or_else(no_disk),
    }
}

/// The memory there is and how much of it is in use. In a container the machine's memory is not
/// what Irori has: the container's own limit is, and that is the number to show, to chart and
/// to plan a model against.
pub fn memory(system: &System) -> (u64, u64) {
    match allowance(Path::new("/sys/fs/cgroup")) {
        Some((limit, used)) if limit < system.total_memory() => (limit, used),
        _ => (system.total_memory(), system.used_memory()),
    }
}

/// The database file and the two SQLite keeps beside it in WAL mode. A file that isn't there
/// counts for nothing.
fn database_bytes(database: &Path) -> u64 {
    let beside = |suffix: &str| {
        let mut name = database.as_os_str().to_owned();
        name.push(suffix);
        std::path::PathBuf::from(name)
    };
    [database.to_path_buf(), beside("-wal"), beside("-shm")]
        .iter()
        .filter_map(|file| std::fs::metadata(file).ok())
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len())
        .sum()
}

/// The volume the data is on, and nothing else: what Irori's own disk reading is made from,
/// every half minute, without asking the machine everything Settings shows.
pub fn disk(data: &Path) -> DiskView {
    data_disk(&Disks::new_with_refreshed_list(), data).unwrap_or_else(no_disk)
}

/// Without a disk to answer, the row says so rather than guessing at a number.
fn no_disk() -> DiskView {
    DiskView {
        mount: String::new(),
        total: 0,
        available: 0,
        used: 0,
        device: String::new(),
    }
}

/// A device's name without the part that says which slice of it: `/dev/disk3s5` and
/// `/dev/disk3s1` are both `/dev/disk3`, `/dev/nvme0n1p2` is `/dev/nvme0n1`, `/dev/sda1` is
/// `/dev/sda`. A name with no number on the end (`overlay`, `tmpfs`) is itself.
fn family(device: &str) -> &str {
    let whole = device.trim_end_matches(|c: char| c.is_ascii_digit());
    if whole.len() == device.len() {
        return device;
    }
    match whole.strip_suffix(['s', 'p']) {
        Some(disk) if disk.ends_with(|c: char| c.is_ascii_digit()) => disk,
        _ => whole,
    }
}

/// The volumes worth showing: ones with a size, mounted on a directory, each filesystem once
/// however many places it is mounted. The data's volume stands for its filesystem under the
/// mount it was found by; any other goes by its shortest mount.
fn volumes(disks: &Disks, data: Option<DiskView>) -> Vec<DiskView> {
    let mut views: Vec<DiskView> = disks
        .iter()
        .filter(|disk| disk.total_space() > 0)
        .filter(|disk| std::fs::metadata(disk.mount_point()).is_ok_and(|meta| meta.is_dir()))
        .map(disk_view)
        .collect();
    views.sort_by(|a, b| (a.mount.len(), &a.mount).cmp(&(b.mount.len(), &b.mount)));
    if let Some(data) = data {
        views.insert(0, data);
    }
    // One pool of space, however it's mounted: the same device (or volumes carved from one,
    // which share its free space) reporting the same numbers. Two disks that merely happen to
    // be as full as each other are different devices, and both stay.
    let mut seen = Vec::new();
    views.retain(|view| {
        let same = (family(&view.device).to_owned(), view.total, view.available);
        if seen.contains(&same) {
            return false;
        }
        seen.push(same);
        true
    });
    views.sort_by(|a, b| a.mount.cmp(&b.mount));
    views
}

/// This process group's memory limit and how much of it is in use, when it has a limit
/// (cgroup v2). Files the kernel is only caching are not counted as used: it gives them up
/// when memory is asked for.
fn allowance(cgroup: &Path) -> Option<(u64, u64)> {
    let read = |name: &str| std::fs::read_to_string(cgroup.join(name)).ok();
    let limit: u64 = read("memory.max")?.trim().parse().ok()?;
    let current: u64 = read("memory.current")?.trim().parse().ok()?;
    let cached = read("memory.stat")
        .and_then(|stat| {
            stat.lines()
                .find_map(|line| line.strip_prefix("file "))
                .and_then(|bytes| bytes.trim().parse::<u64>().ok())
        })
        .unwrap_or(0);
    Some((limit, current.saturating_sub(cached).min(limit)))
}

/// The id of the filesystem a path is on, as the OS numbers them. Equal ids mean the same
/// volume, even when the same data is reachable through several mount paths.
#[cfg(unix)]
fn fsid(path: &Path) -> Option<u64> {
    rustix::fs::statvfs(path).ok().map(|stat| stat.f_fsid)
}

/// The volume the data directory is on, as "how full is it?": by filesystem id when one
/// matches, otherwise by the longest mount the (canonicalized) path falls under. `None` when
/// nothing matches, so the caller can say so rather than guess.
fn data_disk(disks: &Disks, data: &Path) -> Option<DiskView> {
    // The id is the honest answer: macOS's `/Users` is a firmlink into `/System/Volumes/Data`,
    // and not even `canonicalize` resolves firmlinks, so a path under `/Users` still lexically
    // sits inside "/" — the wrong volume to answer "how full?" with. The writable Data volume
    // answers it, and only its filesystem id says so.
    let resolved = data
        .canonicalize()
        .or_else(|_| std::path::absolute(data))
        .unwrap_or_else(|_| data.to_path_buf());

    #[cfg(unix)]
    if let Some(want) = fsid(data) {
        // A filesystem id can cover several mounts (a container's named volume and Docker's
        // small bind-mounted files share the host overlay's id), so scope it to volume-looking
        // ones first: mounts that hold directories, then ones the data path falls under, and
        // only then the longest mount. A bind-mounted file like /etc/resolv.conf never wins.
        let same = disks.iter().filter(|disk| {
            fsid(disk.mount_point()) == Some(want)
                && std::fs::metadata(disk.mount_point()).is_ok_and(|meta| meta.is_dir())
        });
        let disk = same
            .clone()
            .filter(|disk| resolved.starts_with(disk.mount_point()))
            .max_by_key(|disk| disk.mount_point().as_os_str().len())
            .or_else(|| same.max_by_key(|disk| disk.mount_point().as_os_str().len()));
        if let Some(disk) = disk {
            return Some(disk_view(disk));
        }
    }

    // Fall back to mounts by path: "/" is every volume's ancestor, but the one the data is
    // actually on answers the honest "how full?".
    disks
        .iter()
        .filter(|disk| resolved.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .map(disk_view)
}

fn disk_view(disk: &sysinfo::Disk) -> DiskView {
    let total = disk.total_space();
    let available = disk.available_space();
    DiskView {
        mount: disk.mount_point().to_string_lossy().into_owned(),
        total,
        available,
        used: total.saturating_sub(available),
        device: disk.name().to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_containers_limit_is_the_memory_there_is() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        assert_eq!(allowance(dir.path()), None);
        std::fs::write(dir.path().join("memory.max"), "1073741824\n")?;
        std::fs::write(dir.path().join("memory.current"), "900000000\n")?;
        std::fs::write(dir.path().join("memory.stat"), "anon 1\nfile 800000000\n")?;
        assert_eq!(allowance(dir.path()), Some((1_073_741_824, 100_000_000)));
        // No limit set reads "max", and then the machine's own memory is the answer.
        std::fs::write(dir.path().join("memory.max"), "max\n")?;
        assert_eq!(allowance(dir.path()), None);
        Ok(())
    }

    #[test]
    fn the_database_is_weighed_with_its_write_ahead_log() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let database = dir.path().join("irori.db");
        assert_eq!(database_bytes(&database), 0);
        std::fs::write(&database, [0; 100])?;
        std::fs::write(dir.path().join("irori.db-wal"), [0; 40])?;
        std::fs::write(dir.path().join("irori.db-shm"), [0; 2])?;
        assert_eq!(database_bytes(&database), 142);
        Ok(())
    }

    #[test]
    fn the_machine_says_what_else_is_using_it() {
        let host = read(Path::new("."));
        assert!(host.cpu_logical >= host.cpu_cores);
        assert!(host.process_memory > 0, "this process holds some memory");
        assert!(
            host.disks.iter().all(|disk| disk.total > 0),
            "{:?}",
            host.disks
        );
    }

    #[test]
    fn slices_of_one_device_are_one_family() {
        assert_eq!(family("/dev/disk3s5"), "/dev/disk3");
        assert_eq!(family("/dev/disk3s1"), "/dev/disk3");
        assert_eq!(family("/dev/nvme0n1p2"), "/dev/nvme0n1");
        assert_eq!(family("/dev/sda1"), "/dev/sda");
        assert_ne!(family("/dev/sda1"), family("/dev/sdb1"));
        assert_eq!(family("overlay"), "overlay");
    }
}
