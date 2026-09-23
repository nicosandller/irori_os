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

fn node_bin_dir() -> PathBuf {
    Path::new(RUNTIME_DIR).join("bin")
}

pub fn node_binary() -> PathBuf {
    node_bin_dir().join("node")
}

fn corepack_binary() -> PathBuf {
    node_bin_dir().join("corepack")
}

/// The zigbee2mqtt package's entry point, once `ensure_zigbee2mqtt` has installed it.
pub fn zigbee2mqtt_entry() -> PathBuf {
    Path::new(Z2M_DIR)
        .join("node_modules")
        .join("zigbee2mqtt")
        .join("index.js")
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
                "--silent",
                "--show-error",
                "--output",
            ])
            .arg(&archive_path)
            .arg(&url),
        "curl (downloading Node.js)",
    )
    .await?;

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

/// Installs Zigbee2MQTT via `corepack pnpm` (Node's own bundled package manager launcher —
/// nothing extra to download for pnpm itself) if it isn't already there. `version` pins an
/// exact release; `None` installs whatever's currently latest, the one time this runs.
pub async fn ensure_zigbee2mqtt(version: Option<&str>) -> Result<(), String> {
    if zigbee2mqtt_entry().is_file() {
        return Ok(());
    }
    tokio::fs::create_dir_all(Z2M_DIR)
        .await
        .map_err(|e| format!("couldn't create {Z2M_DIR}: {e}"))?;
    let spec = match version {
        Some(v) => format!("zigbee2mqtt@{v}"),
        None => "zigbee2mqtt@latest".to_owned(),
    };
    tracing::info!(package = %spec, "installing Zigbee2MQTT (only needed once)");
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn entry_paths_are_relative_to_this_processs_own_package_directory() {
        assert_eq!(node_binary(), Path::new("runtime/bin/node"));
        assert_eq!(
            zigbee2mqtt_entry(),
            Path::new("z2m/node_modules/zigbee2mqtt/index.js")
        );
    }
}
