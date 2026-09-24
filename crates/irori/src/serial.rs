//! What serial devices are plugged in right now — for a settings field the schema marks
//! `"format": "serial-port"` (a Zigbee dongle, say) to offer as a live-updated list of
//! candidates, on top of the plain text box a device path always was.
//!
//! Best-effort and host-specific: a platform or a `/dev` layout this doesn't recognise just
//! means an empty list, never an error — the field stays free text either way, and the whole
//! point of settling for a heuristic here is that guessing wrong costs nothing a person can't
//! fix by typing the path themselves.

use std::path::Path;

/// Prefixes, under `/dev`, of entries worth offering: `ttyUSB`/`ttyACM` (Linux, wired up on
/// enumeration in kernel order, which can move on replug), and `cu.usbserial`/`cu.usbmodem`
/// (macOS — for developing off the target hardware, not for a Pi itself).
const DEV_PREFIXES: &[&str] = &["ttyUSB", "ttyACM", "cu.usbserial", "cu.usbmodem"];

/// Every candidate serial device found, sorted, without duplicates. `/dev/serial/by-id/*`
/// (Linux) comes first when present — a stable, human-readable name (the dongle's own make and
/// serial number) unlike `/dev/ttyUSB0`, which is just enumeration order and can point at a
/// different device after a replug.
pub fn list() -> Vec<String> {
    let mut found = Vec::new();
    found.extend(by_id_entries(Path::new("/dev/serial/by-id")));
    found.extend(dev_entries(Path::new("/dev")));
    found.sort();
    found.dedup();
    found
}

fn by_id_entries(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path().to_string_lossy().into_owned())
        .collect()
}

fn dev_entries(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?.to_owned();
            DEV_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
                .then(|| format!("/dev/{name}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_recognised_prefixes_are_offered() {
        let dir = tempfile::tempdir().expect("a temp dir");
        for name in ["ttyUSB0", "ttyACM0", "cu.usbserial-1420", "ttyS0", "null"] {
            std::fs::write(dir.path().join(name), "").expect("writes");
        }
        let mut found = dev_entries(dir.path());
        found.sort();
        assert_eq!(
            found,
            vec!["/dev/cu.usbserial-1420", "/dev/ttyACM0", "/dev/ttyUSB0"]
        );
    }

    #[test]
    fn a_missing_or_unreadable_directory_is_an_empty_list_not_an_error() {
        assert!(dev_entries(Path::new("/does/not/exist")).is_empty());
        assert!(by_id_entries(Path::new("/does/not/exist")).is_empty());
    }

    #[test]
    fn by_id_entries_are_returned_by_their_full_path() {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::write(
            dir.path().join("usb-ITead_Sonoff_Zigbee_3.0-if00-port0"),
            "",
        )
        .expect("writes");
        let found = by_id_entries(dir.path());
        assert_eq!(found.len(), 1);
        assert!(
            found[0].ends_with("usb-ITead_Sonoff_Zigbee_3.0-if00-port0"),
            "{found:?}"
        );
    }
}
