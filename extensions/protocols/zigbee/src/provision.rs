//! Downloads a managed Node.js runtime and installs Zigbee2MQTT, entirely within this
//! extension's own package directory (`./runtime`, `./z2m`, relative to this process's own
//! working directory — the same directory the core installs and runs every extension from).
//! Never touches any Node.js the host might already have.
//!
//! Shells out to `curl` and `tar` rather than adding an HTTP-client and archive-extraction
//! dependency for two downloads: `host_shell = true` is already the honest permission this
//! extension declares (it goes on to run `node`/`pnpm`/Zigbee2MQTT itself), so this doesn't add
//! a new kind of access, just one more thing run under it.
//!
//! **Known limit**: only Linux and macOS, x86_64 or arm64, with glibc. A musl-only host (Alpine,
//! say) can't run the official Node.js build this downloads — there's no second runtime to fall
//! back to, and detecting musl-vs-glibc reliably enough to matter is its own project.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tokio::process::Command;

const RUNTIME_DIR: &str = "runtime";
const Z2M_DIR: &str = "z2m";

/// A current Node.js LTS. Only used the very first time this extension runs on a machine —
/// after that, whichever version already made it to disk keeps being used, so Irori never
/// silently upgrades the Node underneath a working installation.
const DEFAULT_NODE_VERSION: &str = "24.21.0";

fn platform_target() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("linux-x64"),
        ("linux", "aarch64") => Ok("linux-arm64"),
        ("macos", "x86_64") => Ok("darwin-x64"),
        ("macos", "aarch64") => Ok("darwin-arm64"),
        (os, arch) => Err(format!(
            "no managed Node.js build for {os}/{arch}; this extension supports Linux and macOS, \
             x86_64 or arm64, with glibc"
        )),
    }
}

/// Absolute, always. `ensure_zigbee2mqtt` runs its command with `current_dir` set to `z2m/`, and
/// both the program to exec and every entry of a `PATH` given to the child resolve against the
/// child's own directory — so a relative `runtime/bin` there means `z2m/runtime/bin`, which is
/// nothing. That failure surfaces as a bare `No such file or directory`, which reads as a Node
/// that was never installed rather than as a path pointing at the wrong place.
fn node_bin_dir() -> PathBuf {
    let relative = Path::new(RUNTIME_DIR).join("bin");
    std::path::absolute(&relative).unwrap_or(relative)
}

pub fn node_binary() -> PathBuf {
    node_bin_dir().join("node")
}

fn corepack_binary() -> PathBuf {
    node_bin_dir().join("corepack")
}

/// The zigbee2mqtt package's entry point, once `ensure_zigbee2mqtt` has installed it.
pub fn zigbee2mqtt_entry() -> PathBuf {
    zigbee2mqtt_package_dir().join("index.js")
}

fn zigbee2mqtt_package_dir() -> PathBuf {
    Path::new(Z2M_DIR).join("node_modules").join("zigbee2mqtt")
}

/// Reads the installed package's own `"version"` field, if it's there at all — used to tell
/// whether the currently installed copy matches what settings now ask for.
fn installed_zigbee2mqtt_version() -> Option<String> {
    version_from_package_json(&zigbee2mqtt_package_dir())
}

fn version_from_package_json(package_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(package_dir.join("package.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

async fn run(command: &mut Command, what: &str) -> Result<(), String> {
    let output = command
        .output()
        .await
        .map_err(|e| format!("couldn't run {what}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{what} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Downloads and extracts Node.js if it isn't already provisioned (checked by
/// `runtime/bin/node` already existing — a one-time cost after the first successful run).
pub async fn ensure_node() -> Result<(), String> {
    if node_binary().is_file() {
        return Ok(());
    }
    let platform = platform_target()?;
    let version = DEFAULT_NODE_VERSION;
    let archive = format!("node-v{version}-{platform}.tar.gz");
    let url = format!("https://nodejs.org/dist/v{version}/{archive}");
    tracing::info!(%url, "downloading Node.js (only needed once)");

    tokio::fs::create_dir_all(RUNTIME_DIR)
        .await
        .map_err(|e| format!("couldn't create {RUNTIME_DIR}: {e}"))?;
    let archive_path = Path::new(RUNTIME_DIR).join(&archive);
    run(
        Command::new("curl")
            .args([
                "--fail",
                "--location",
                // Redirects may only stay on https. `--location` otherwise lets an https URL
                // redirect to plain http, and since the checksum file is fetched the same way,
                // a downgrade would have the archive checked against an attacker's own hash.
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--silent",
                "--show-error",
                "--output",
            ])
            .arg(&archive_path)
            .arg(&url),
        "curl (downloading Node.js)",
    )
    .await?;

    if let Err(why) = verify_checksum(&archive_path, version, &archive).await {
        let _ = tokio::fs::remove_file(&archive_path).await;
        return Err(why);
    }

    // The archive's own top-level directory is `node-v{version}-{platform}`; strip it so the
    // result lands directly at `runtime/bin/…` regardless of the version downloaded.
    run(
        Command::new("tar")
            .args(["xzf"])
            .arg(&archive_path)
            .args(["--strip-components=1", "-C"])
            .arg(RUNTIME_DIR),
        "tar (extracting Node.js)",
    )
    .await?;
    let _ = tokio::fs::remove_file(&archive_path).await;

    if !node_binary().is_file() {
        return Err(format!(
            "extracted {archive} but {} still doesn't exist",
            node_binary().display()
        ));
    }
    tracing::info!(version, "Node.js ready");
    Ok(())
}

/// Checks a downloaded archive against the SHA-256 nodejs.org itself publishes for that release,
/// so a corrupted download or a compromised mirror gets caught before anything from the archive
/// ever runs. `SHASUMS256.txt` lists every artifact for the release, one `<hash>  <filename>`
/// line each.
async fn verify_checksum(
    archive_path: &Path,
    version: &str,
    archive_name: &str,
) -> Result<(), String> {
    let url = format!("https://nodejs.org/dist/v{version}/SHASUMS256.txt");
    let output = Command::new("curl")
        .args(["--fail", "--location", "--silent", "--show-error"])
        // https only, both hops — see the note on the archive download above.
        .args(["--proto", "=https", "--proto-redir", "=https"])
        .arg(&url)
        .output()
        .await
        .map_err(|e| format!("couldn't run curl (fetching Node.js checksums): {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "curl (fetching Node.js checksums) failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let sums = String::from_utf8_lossy(&output.stdout);
    let expected = find_checksum(&sums, archive_name)
        .ok_or_else(|| format!("{archive_name} isn't listed in SHASUMS256.txt"))?;

    let bytes = tokio::fs::read(archive_path)
        .await
        .map_err(|e| format!("couldn't read {}: {e}", archive_path.display()))?;
    let actual = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    if actual != expected {
        return Err(format!(
            "{archive_name} failed checksum verification: expected {expected}, got {actual}"
        ));
    }
    Ok(())
}

/// Parses `SHASUMS256.txt`'s `<hash>  <filename>` lines, picking out the one for `archive_name`.
fn find_checksum(sums: &str, archive_name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?;
        (name == archive_name).then(|| hash.to_owned())
    })
}

/// Installs Zigbee2MQTT via `corepack pnpm` (Node's own bundled package manager launcher —
/// nothing extra to download for pnpm itself) if it isn't already there, or if a `version` is
/// given that doesn't match what's already installed — a settings change pinning (or re-pinning)
/// a version is a deliberate re-install, not left silently unapplied. `None` only installs if
/// nothing is there yet, leaving whatever's already installed alone from then on.
pub async fn ensure_zigbee2mqtt(version: Option<&str>) -> Result<(), String> {
    if zigbee2mqtt_entry().is_file() {
        match version {
            None => return Ok(()),
            Some(wanted) if installed_zigbee2mqtt_version().as_deref() == Some(wanted) => {
                return Ok(());
            }
            Some(wanted) => {
                tracing::info!(
                    wanted,
                    "requested Zigbee2MQTT version differs, reinstalling"
                );
            }
        }
    }
    tokio::fs::create_dir_all(Z2M_DIR)
        .await
        .map_err(|e| format!("couldn't create {Z2M_DIR}: {e}"))?;
    let spec = match version {
        Some(v) => format!("zigbee2mqtt@{}", checked_version(v)?),
        None => "zigbee2mqtt@latest".to_owned(),
    };
    tracing::info!(package = %spec, "installing Zigbee2MQTT");
    run(
        Command::new(corepack_binary())
            .args(["pnpm", "add", &spec])
            // pnpm blocks native postinstall build scripts by default; Zigbee2MQTT needs
            // exactly two — the serial port driver and one of its transitive Zigbee stack
            // deps — verified by hand (`pnpm add` without this fails with
            // `ERR_PNPM_IGNORED_BUILDS`, naming these two).
            .args(["--allow-build=@serialport/bindings-cpp"])
            .args(["--allow-build=unix-dgram"])
            .current_dir(Z2M_DIR)
            .env("COREPACK_ENABLE_DOWNLOAD_PROMPT", "0")
            .env("PATH", path_with_managed_node()),
        "pnpm (installing Zigbee2MQTT)",
    )
    .await?;
    if !zigbee2mqtt_entry().is_file() {
        return Err(format!(
            "installed {spec} but {} still doesn't exist",
            zigbee2mqtt_entry().display()
        ));
    }
    Ok(())
}

/// Corepack needs `node` reachable on `PATH` to run pnpm with (it shells out to the same
/// interpreter it's running under, resolved by name) — this extension's managed copy isn't on
/// the host's own `PATH`, so it's prepended here rather than relying on one being installed at
/// all.
fn path_with_managed_node() -> std::ffi::OsString {
    let mut path = node_bin_dir().into_os_string();
    if let Ok(existing) = std::env::var("PATH") {
        path.push(":");
        path.push(existing);
    }
    path
}

/// A pinned Zigbee2MQTT version, checked before it goes anywhere near a package spec.
///
/// This value comes from a settings file a person writes, and lands in `pnpm add zigbee2mqtt@…`
/// run with this extension's `host_shell` trust. npm's own spec syntax accepts far more than a
/// version there — an alias, a tarball URL, a git repository — so an unchecked value is a way to
/// have pnpm install and run something else entirely. Only a release version, the thing the
/// setting claims to be: digits and dots, optionally a `-suffix` for a pre-release.
fn checked_version(version: &str) -> Result<&str, String> {
    let (number, pre) = match version.split_once('-') {
        Some((number, pre)) => (number, Some(pre)),
        None => (version, None),
    };
    let number_ok = !number.is_empty()
        && number
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
    let pre_ok = pre.is_none_or(|pre| {
        !pre.is_empty()
            && pre
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    });
    if number_ok && pre_ok {
        Ok(version)
    } else {
        Err(format!(
            "`{version}` isn't a Zigbee2MQTT version: digits and dots, e.g. `2.6.2`, \
             optionally with a `-rc.1` style suffix"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pinned_version_is_a_version_and_not_an_npm_spec() {
        for good in ["2.6.2", "2", "2.6.2-rc.1", "1.42.0-dev-3"] {
            assert_eq!(checked_version(good), Ok(good), "{good}");
        }
        // Every one of these is a spec npm would happily install *something* from.
        for bad in [
            "latest",
            "npm:evil@1.0.0",
            "https://example.com/z2m.tgz",
            "github:someone/zigbee2mqtt",
            "../../../etc",
            "2.6.2 --allow-build=anything",
            "",
        ] {
            assert!(checked_version(bad).is_err(), "accepted `{bad}`");
        }
    }

    #[test]
    fn known_platforms_map_to_nodejs_orgs_own_names() {
        // Only meaningfully checks the current CI/dev platform, but that's the one that matters
        // for "does this build's own tests still make sense here."
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            assert_eq!(platform_target(), Ok("darwin-arm64"));
        }
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            assert_eq!(platform_target(), Ok("linux-x64"));
        }
    }

    #[test]
    fn entry_paths_hang_off_this_processs_own_package_directory() {
        let here = std::env::current_dir().expect("a working directory");
        assert_eq!(node_binary(), here.join("runtime/bin/node"));
        assert_eq!(
            zigbee2mqtt_entry(),
            Path::new("z2m/node_modules/zigbee2mqtt/index.js")
        );
    }

    /// `ensure_zigbee2mqtt` runs `corepack` with `current_dir` set to `z2m/`, which is where both
    /// the program path and the `PATH` it's given are resolved — `runtime/bin` relative means
    /// `z2m/runtime/bin`, so `corepack` is never found and Zigbee2MQTT can never install.
    #[test]
    fn node_is_named_absolutely_so_a_child_run_from_elsewhere_still_finds_it() {
        assert!(node_bin_dir().is_absolute(), "{:?}", node_bin_dir());
        assert!(corepack_binary().is_absolute());
        let path = path_with_managed_node();
        let path = path.to_string_lossy().into_owned();
        let first = path.split(':').next().expect("split yields one part");
        assert!(Path::new(first).is_absolute(), "{first}");
    }

    #[test]
    fn reads_the_version_field_out_of_an_installed_packages_package_json() {
        let dir = tempfile::tempdir().expect("can create a temp dir");
        std::fs::write(
            dir.path().join("package.json"),
            br#"{"name": "zigbee2mqtt", "version": "2.1.0"}"#,
        )
        .expect("can write package.json");
        assert_eq!(
            version_from_package_json(dir.path()),
            Some("2.1.0".to_owned())
        );
    }

    #[test]
    fn no_installed_package_json_is_none_not_an_error() {
        let dir = tempfile::tempdir().expect("can create a temp dir");
        assert_eq!(version_from_package_json(dir.path()), None);
    }

    #[test]
    fn finds_the_matching_line_in_a_shasums_file() {
        let sums = "\
            aaaa1111  node-v24.21.0-darwin-arm64.tar.gz\n\
            bbbb2222  node-v24.21.0-linux-x64.tar.gz\n";
        assert_eq!(
            find_checksum(sums, "node-v24.21.0-linux-x64.tar.gz"),
            Some("bbbb2222".to_owned())
        );
        assert_eq!(find_checksum(sums, "node-v24.21.0-win-x64.zip"), None);
    }
}
