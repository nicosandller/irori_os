//! Updating Irori itself: finding out that a newer release exists, and replacing the running
//! binary with it.
//!
//! The same release the installer downloads (`install/install.sh`): `irori-<os>-<arch>.tar.gz`
//! and the `SHA256SUMS` published beside it. Nothing takes the running binary's place until it
//! has matched its checksum and been run once to say which version it is. The binary it replaces
//! is kept next to it as `irori.previous`, so a release that turns out bad is one `mv` away from
//! undone. Then Irori restarts the way the Settings page's Restart does.
//!
//! Only a release that was installed where Irori can write is replaced this way. A build from
//! source, or a binary a package manager or an image put somewhere read-only, says when a newer
//! release exists and leaves installing it to whoever put it there.
//!
//! Looking for a release is the one thing Irori asks the internet on its own: once after it
//! starts and twice a day, a single request to GitHub that says nothing about the home.
//! `[updates] check = false` in `irori.toml` turns that off; asking by hand still works.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use irori_types::Version;
use sha2::{Digest as _, Sha256};
use tokio::sync::Notify;

const REPO: &str = "nicosandller/irori_os";

/// How long GitHub gets to answer a question, and to keep a download moving.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long the release itself gets to arrive: tens of megabytes, on a slow line.
const DOWNLOADED_WITHIN: Duration = Duration::from_secs(10 * 60);

/// How long the new binary gets to say which version it is.
const TRIED_FOR: Duration = Duration::from_secs(20);

/// The binary this process was started from, noted before anything can move it: once it has
/// been replaced, the operating system's own answer may be the old file under its new name.
pub static EXE: OnceLock<PathBuf> = OnceLock::new();

/// How a restart is asked for: wake the server's shutdown, and say it's a restart
/// (`main.rs`). Unset in a process that can't restart itself.
pub static RESTART: OnceLock<(Arc<Notify>, Arc<AtomicBool>)> = OnceLock::new();

/// Whether to look for a release without being asked (`[updates] check`).
pub static LOOKS: OnceLock<bool> = OnceLock::new();

/// A release of Irori.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// The first line of its notes.
    pub summary: Option<String>,
    /// Where the notes are.
    pub url: Option<String>,
}

/// Whether `latest` is a later release than `installed`. A development build counts as the
/// release it is working towards, so it is never offered the one before it.
pub fn newer(installed: &Version, latest: &Version) -> bool {
    latest.release() > installed.release()
}

/// The running binary's place, as it was when this process started.
pub fn exe() -> Option<PathBuf> {
    EXE.get().cloned().or_else(|| std::env::current_exe().ok())
}

/// The release asset for a build target, as the release workflow names it.
fn asset(target: &str) -> Option<String> {
    let os = if target.contains("linux") {
        "linux"
    } else if target.contains("apple-darwin") {
        "darwin"
    } else {
        return None;
    };
    let arch = match target.split('-').next()? {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => return None,
    };
    Some(format!("irori-{os}-{arch}.tar.gz"))
}

/// Whether this Irori can replace itself: a release build, of a kind releases are published
/// for, sitting where this process may write, and able to restart.
pub fn installable() -> bool {
    let release = !irori_types::VERSION.ends_with("-dev")
        && !crate::build_info::COMMIT.ends_with("-modified");
    release
        && cfg!(unix)
        && RESTART.get().is_some()
        && asset(env!("IRORI_TARGET")).is_some()
        && exe().is_some_and(|exe| writable(&exe))
}

/// Whether a file can be put beside `exe` and take its place. Tried rather than read off the
/// permissions, which say nothing about a read-only volume.
fn writable(exe: &Path) -> bool {
    let Some(dir) = exe.parent() else {
        return false;
    };
    let probe = dir.join(format!(".irori-update-probe-{}", std::process::id()));
    let made = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    made
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .user_agent(concat!("irori/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| error.to_string())
}

/// GitHub's answer about the latest release, as a [`Release`].
fn parse_release(body: &serde_json::Value) -> Result<Release, String> {
    let tag = body
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .ok_or("GitHub's answer names no release")?;
    let version = Version::try_from(tag.trim_start_matches('v').to_owned())
        .map_err(|_| format!("`{tag}` isn't a version Irori understands"))?;
    let text = |key: &str| body.get(key).and_then(serde_json::Value::as_str);
    // Generated notes open with a heading; the first line that says something is the summary.
    let summary = text("body").and_then(|notes| {
        notes
            .lines()
            .map(|line| line.trim().trim_start_matches(['#', '*', '-', ' ']))
            .find(|line| !line.is_empty() && !line.eq_ignore_ascii_case("what's changed"))
            .map(|line| line.chars().take(255).collect())
    });
    Ok(Release {
        version,
        summary,
        url: text("html_url")
            .filter(|url| url.starts_with("https://"))
            .map(str::to_owned),
    })
}

/// The latest release, as GitHub has it now.
pub async fn latest() -> Result<Release, String> {
    let unreachable = |_| "Couldn't reach github.com to look for a newer Irori.".to_owned();
    let body: serde_json::Value = client()?
        .get(format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))
        .header("accept", "application/vnd.github+json")
        .timeout(PATIENCE)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(unreachable)?
        .json()
        .await
        .map_err(unreachable)?;
    parse_release(&body)
}

/// The checksum `sums` (a `SHA256SUMS` file) gives for `asset`.
fn checksum_for(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let digest = words.next()?;
        let name = words.next()?.trim_start_matches(['*', '.', '/']);
        (name == asset && digest.len() == 64).then(|| digest.to_ascii_lowercase())
    })
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Downloads `version`, checks it, tries it, puts it in the running binary's place, and asks
/// for a restart. `Ok` means the restart is on its way; this process ends a moment later.
pub async fn install(version: &Version) -> Result<(), String> {
    if !installable() {
        return Err(
            "This Irori wasn't installed in a way it can update itself. Update it the way it \
             was installed."
                .to_owned(),
        );
    }
    let asset = asset(env!("IRORI_TARGET")).ok_or("no release is published for this machine")?;
    let exe = exe().ok_or("Irori can't tell where its own binary is")?;
    let base = format!("https://github.com/{REPO}/releases/download/v{version}");
    let client = client()?;
    let unreachable = |_| format!("Couldn't download Irori {version} from github.com.");
    let fetch = |name: String| {
        let request = client.get(format!("{base}/{name}"));
        async move {
            request
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
        }
    };
    let sums = tokio::time::timeout(PATIENCE, async {
        fetch("SHA256SUMS".to_owned()).await?.text().await
    })
    .await
    .map_err(|_| format!("github.com took too long to answer about Irori {version}."))?
    .map_err(unreachable)?;
    let expected = checksum_for(&sums, &asset)
        .ok_or_else(|| format!("Irori {version} has no checksum for this machine's download."))?;
    // A release is tens of megabytes; it is checked whole before any of it is written.
    let archive = tokio::time::timeout(DOWNLOADED_WITHIN, async {
        fetch(asset.clone()).await?.bytes().await
    })
    .await
    .map_err(|_| format!("The download of Irori {version} stalled. Nothing was changed."))?
    .map_err(unreachable)?;
    if sha256(&archive) != expected {
        return Err(format!(
            "The download of Irori {version} doesn't match its published checksum. Nothing was \
             changed."
        ));
    }
    let version = version.clone();
    tokio::task::spawn_blocking(move || place(&exe, &archive, &version))
        .await
        .map_err(|error| error.to_string())??;
    restart()
}

/// Asks the server to shut down and start again as the binary now in place.
fn restart() -> Result<(), String> {
    let (restart, restarting) = RESTART
        .get()
        .ok_or("The new Irori is in place. Restart Irori to use it.")?;
    restarting.store(true, Ordering::SeqCst);
    tracing::info!("Irori was updated; restarting into the new version");
    restart.notify_one();
    Ok(())
}

/// Unpacks `archive` beside `exe`, makes sure what came out runs and is `version`, and swaps it
/// in. The binary that was there stays as `<exe>.previous`. Nothing is left behind when it fails,
/// and `exe` is only touched by the last step, a rename.
fn place(exe: &Path, archive: &[u8], version: &Version) -> Result<(), String> {
    let dir = exe.parent().ok_or("Irori's binary has no directory")?;
    // Beside the binary, so the swap is a rename on one filesystem and can't stop halfway.
    let staging = dir.join(format!(".irori-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|error| format!("Couldn't write beside Irori's binary: {error}."))?;
    let placed = (|| {
        let packed = staging.join("irori.tar.gz");
        std::fs::write(&packed, archive).map_err(|error| error.to_string())?;
        // Only the one member the release is known to hold, so nothing else in an archive can
        // land anywhere.
        let unpacked = std::process::Command::new("tar")
            .arg("-xzf")
            .arg(&packed)
            .arg("-C")
            .arg(&staging)
            .arg("irori")
            .output()
            .map_err(|_| "Unpacking the new Irori needs tar on this machine.".to_owned())?;
        if !unpacked.status.success() {
            return Err("The new Irori's download couldn't be unpacked.".to_owned());
        }
        let new = staging.join("irori");
        make_runnable(&new)?;
        tried(&new, version)?;
        swap(exe, &new)
    })();
    let _ = std::fs::remove_dir_all(&staging);
    placed
}

#[cfg(unix)]
fn make_runnable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|error| error.to_string())
}

#[cfg(not(unix))]
fn make_runnable(_: &Path) -> Result<(), String> {
    Ok(())
}

/// Runs the new binary's `version --json` and holds it to the version that was asked for. One
/// that won't start on this machine, or is something else, never takes the place of one that
/// works.
fn tried(new: &Path, version: &Version) -> Result<(), String> {
    let mut child = std::process::Command::new(new)
        .args(["version", "--json"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| format!("The new Irori doesn't run on this machine: {error}."))?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < TRIED_FOR => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("The new Irori didn't answer when it was tried.".to_owned());
            }
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    let said: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| "The new Irori didn't say which version it is.".to_owned())?;
    let is = said.get("version").and_then(serde_json::Value::as_str);
    if !output.status.success() || is != Some(version.as_str()) {
        return Err(format!(
            "The download says it is Irori {}, not {version}.",
            is.unwrap_or("something else")
        ));
    }
    Ok(())
}

/// Puts `new` where `exe` is, keeping what was there as `<exe>.previous`.
fn swap(exe: &Path, new: &Path) -> Result<(), String> {
    let mut previous = exe.as_os_str().to_owned();
    previous.push(".previous");
    let previous = PathBuf::from(previous);
    let failed = |error: std::io::Error| format!("Couldn't replace Irori's binary: {error}.");
    // A copy, not a move: the running binary's place is never empty, whatever interrupts this.
    std::fs::copy(exe, &previous).map_err(failed)?;
    std::fs::rename(new, exe).map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::try_from(text.to_owned()).expect("a version")
    }

    #[test]
    fn only_a_later_release_is_newer() {
        assert!(newer(&version("1.2.0"), &version("1.3.0")));
        assert!(!newer(&version("1.3.0"), &version("1.3.0")));
        assert!(!newer(&version("1.3.0"), &version("1.2.9")));
        // A development build of 1.3.0 isn't offered 1.2.0, and is offered 1.3.1.
        assert!(!newer(&version("1.3.0-dev"), &version("1.2.0")));
        assert!(!newer(&version("1.3.0-dev"), &version("1.3.0")));
        assert!(newer(&version("1.3.0-dev"), &version("1.3.1")));
    }

    #[test]
    fn the_asset_is_named_as_the_release_names_it() {
        let named = |target| asset(target);
        assert_eq!(
            named("aarch64-unknown-linux-musl").as_deref(),
            Some("irori-linux-arm64.tar.gz")
        );
        assert_eq!(
            named("x86_64-apple-darwin").as_deref(),
            Some("irori-darwin-x64.tar.gz")
        );
        assert_eq!(named("riscv64gc-unknown-linux-gnu"), None);
        assert_eq!(named("x86_64-pc-windows-msvc"), None);
    }

    #[test]
    fn githubs_answer_becomes_a_release() {
        let release = parse_release(&serde_json::json!({
            "tag_name": "v1.4.0",
            "html_url": "https://github.com/nicosandller/irori_os/releases/tag/v1.4.0",
            "body": "## What's Changed\n* Updates install one at a time by @someone\n",
        }))
        .expect("a release");
        assert_eq!(release.version, version("1.4.0"));
        assert_eq!(
            release.summary.as_deref(),
            Some("Updates install one at a time by @someone")
        );
        assert!(release.url.is_some());
        assert!(parse_release(&serde_json::json!({"tag_name": "nightly"})).is_err());
        assert!(parse_release(&serde_json::json!({"message": "Not Found"})).is_err());
    }

    #[test]
    fn a_download_is_held_to_its_published_checksum() {
        let sums = format!(
            "{}  irori-linux-arm64.tar.gz\n{}  irori-ext-mqtt-x.tar.gz\n",
            sha256(b"the binary"),
            sha256(b"something else")
        );
        let expected = checksum_for(&sums, "irori-linux-arm64.tar.gz").expect("listed");
        assert_eq!(expected, sha256(b"the binary"));
        assert_ne!(expected, sha256(b"the binary, tampered with"));
        assert_eq!(checksum_for(&sums, "irori-darwin-x64.tar.gz"), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_old_binary_is_kept_beside_the_new_one() {
        let dir = tempfile::tempdir().expect("a directory");
        let exe = dir.path().join("irori");
        let new = dir.path().join("new");
        std::fs::write(&exe, b"old").expect("written");
        std::fs::write(&new, b"new").expect("written");
        swap(&exe, &new).expect("swapped");
        assert_eq!(std::fs::read(&exe).expect("there"), b"new");
        assert_eq!(
            std::fs::read(dir.path().join("irori.previous")).expect("kept"),
            b"old"
        );
        assert!(!new.exists());
        assert!(writable(&exe));
    }

    /// A stand-in for a downloaded release: a script that answers `version --json`.
    #[cfg(unix)]
    #[test]
    fn a_download_that_is_another_version_or_wont_run_never_takes_the_place() {
        let dir = tempfile::tempdir().expect("a directory");
        let exe = dir.path().join("irori");
        std::fs::write(&exe, b"old").expect("written");
        let pack = |script: &str| {
            let source = dir.path().join("source");
            let _ = std::fs::remove_dir_all(&source);
            std::fs::create_dir_all(&source).expect("made");
            std::fs::write(source.join("irori"), script).expect("written");
            let packed = dir.path().join("release.tar.gz");
            let made = std::process::Command::new("tar")
                .arg("-czf")
                .arg(&packed)
                .arg("-C")
                .arg(&source)
                .arg("irori")
                .status()
                .expect("tar runs");
            assert!(made.success());
            std::fs::read(&packed).expect("packed")
        };
        let says = |version: &str| format!("#!/bin/sh\necho '{{\"version\": \"{version}\"}}'\n");

        let wrong = place(&exe, &pack(&says("1.3.9")), &version("1.4.0"));
        assert!(wrong.is_err_and(|why| why.contains("1.3.9")));
        let broken = place(&exe, &pack("#!/bin/sh\nexit 1\n"), &version("1.4.0"));
        assert!(broken.is_err());
        assert_eq!(std::fs::read(&exe).expect("there"), b"old", "untouched");
        assert!(!dir.path().join("irori.previous").exists());

        place(&exe, &pack(&says("1.4.0")), &version("1.4.0")).expect("placed");
        assert!(
            std::fs::read_to_string(&exe)
                .expect("there")
                .contains("1.4.0")
        );
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .expect("listed")
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| name.starts_with(".irori-update"))
            .collect();
        assert!(left.is_empty(), "nothing left behind: {left:?}");
    }
}
