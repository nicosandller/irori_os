//! The machine running the instance, for the System menu in Settings.
//!
//! Read once, each time the Settings page asks: nothing here is kept, so the page always gets the
//! machine as it is. The values come from `sysinfo`, which reads the same places a system monitor
//! does — `sysctl` on macOS, `/proc` on Linux — and deliberately asks for no more than the page
//! shows, so a slow bit of the OS is never pulled in for the sake of a row nobody is looking at.

use std::path::Path;

use serde::Serialize;
use sysinfo::{CpuRefreshKind, Disks, MemoryRefreshKind, RefreshKind, System};

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
    /// RAM, in bytes.
    pub memory_total: u64,
    pub memory_used: u64,
    /// How long the machine has been up. Measured in what the OS reports, not Irori's own uptime,
    /// which is the "Uptime" row on the Instance card.
    pub uptime_secs: u64,
    /// The filesystem that holds the instance's data: the volume the data directory is on.
    pub disk: DiskView,
}

/// One disk: enough for "is the volume getting full?" without implying anything finer.
#[derive(Debug, Serialize)]
pub struct DiskView {
    /// Where the volume is mounted, e.g. "/" or "/System/Volumes/Data".
    pub mount: String,
    pub total: u64,
    pub available: u64,
    pub used: u64,
}

/// Reads the machine, given where the instance keeps its data so the right volume is reported.
pub fn read(data: &Path) -> HostView {
    let system = System::new_with_specifics(
        RefreshKind::nothing()
            .with_memory(MemoryRefreshKind::everything())
            .with_cpu(CpuRefreshKind::everything()),
    );
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
    let memory_total = system.total_memory();
    let memory_used = system.used_memory();

    HostView {
        host: System::host_name(),
        os: System::name().unwrap_or_else(|| std::env::consts::OS.to_owned()),
        os_version: System::long_os_version().unwrap_or_default(),
        kernel: System::kernel_version().unwrap_or_default(),
        arch: std::env::consts::ARCH,
        cpu,
        cpu_cores,
        memory_total,
        memory_used,
        uptime_secs: System::uptime(),
        disk: data_disk(data).unwrap_or(DiskView {
            // Without a disk to answer, the row says so rather than guessing at a number.
            mount: String::new(),
            total: 0,
            available: 0,
            used: 0,
        }),
    }
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
fn data_disk(data: &Path) -> Option<DiskView> {
    let disks = Disks::new_with_refreshed_list();

    // The id is the honest answer: macOS's `/Users` is a firmlink into `/System/Volumes/Data`,
    // and not even `canonicalize` resolves firmlinks, so a path under `/Users` still lexically
    // sits inside "/" — the wrong volume to answer "how full?" with. The writable Data volume
    // answers it, and only its filesystem id says so.
    #[cfg(unix)]
    let by_id = fsid(data).and_then(|want| {
        disks
            .iter()
            .find(|disk| fsid(disk.mount_point()) == Some(want))
    });
    #[cfg(unix)]
    if let Some(disk) = by_id {
        return Some(disk_view(disk));
    }

    // Fall back to mounts by path: "/" is every volume's ancestor, but the one the data is
    // actually on answers the honest "how full?".
    let resolved = data
        .canonicalize()
        .or_else(|_| std::path::absolute(data))
        .unwrap_or_else(|_| data.to_path_buf());
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
    }
}
