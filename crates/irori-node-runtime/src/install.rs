//! Downloads a managed Node.js runtime and installs one npm package, entirely within the
//! extension's own package directory (`./runtime`, and the package's own folder, relative to the
//! process's working directory — the directory the core installs and runs every extension from).
//! Never touches any Node.js the host might already have.
//!
//! Shells out to `curl` and `tar` rather than adding an HTTP-client and archive-extraction
//! dependency for two downloads: `host_shell = true` is already the honest permission an
//! extension using this declares (it goes on to run `node`/`pnpm` and the package itself), so
//! this doesn't add a new kind of access, just one more thing run under it.
//!
//! **Known limit**: only Linux and macOS, x86_64 or arm64, with glibc. A musl-only host (Alpine,
//! say) can't run the official Node.js build this downloads — there's no second runtime to fall
//! back to, and detecting musl-vs-glibc reliably enough to matter is its own project.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tokio::process::Command;

const RUNTIME_DIR: &str = "runtime";

/// A current Node.js LTS. Only used the very first time an extension runs on a machine — after
/// that, whichever version already made it to disk keeps being used, so Irori never silently
/// upgrades the Node underneath a working installation. Both Zigbee2MQTT and the Matter server
/// (22.13 or later, 24 recommended) run on it.
pub const DEFAULT_NODE_VERSION: &str = "24.21.0";

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

/// Absolute, always. [`Package::ensure`] runs its command with `current_dir` set to the
/// package's own folder, and both the program to exec and every entry of a `PATH` given to the
/// child resolve against the child's own directory — so a relative `runtime/bin` there means
/// `z2m/runtime/bin`, which is nothing. That failure surfaces as a bare `No such file or
/// directory`, which reads as a Node that was never installed rather than as a path pointing at
/// the wrong place.
fn node_bin_dir() -> PathBuf {
    let relative = Path::new(RUNTIME_DIR).join("bin");
    std::path::absolute(&relative).unwrap_or(relative)
}

/// The managed `node`, once [`ensure_node`] has put it there.
pub fn node_binary() -> PathBuf {
    node_bin_dir().join("node")
}

fn corepack_binary() -> PathBuf {
    node_bin_dir().join("corepack")
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

/// The one npm package an extension runs, and how to install it.
#[derive(Debug, Clone, Copy)]
pub struct Package {
    /// Its name on npm, e.g. `zigbee2mqtt`.
    pub name: &'static str,
    /// What people call it, for messages: "Zigbee2MQTT".
    pub title: &'static str,
    /// The folder it's installed into, beside `runtime/`, e.g. `z2m`.
    pub dir: &'static str,
    /// The `name` in that folder's own `package.json`.
    pub project: &'static str,
    /// The file that runs it, inside the installed package, e.g. `index.js`.
    pub entry: &'static str,
    /// Dependencies whose native build scripts pnpm may run. pnpm blocks them by default.
    pub allow_builds: &'static [&'static str],
    /// Dependencies whose build scripts are deliberately never run: optional native parts the
    /// extension doesn't use (the Matter server's Bluetooth). Recent pnpm refuses to install
    /// while any build is left undecided, so these are written down as declined.
    pub decline_builds: &'static [&'static str],
}

impl Package {
    /// Where the installed package itself lives.
    pub fn package_dir(&self) -> PathBuf {
        Path::new(self.dir).join("node_modules").join(self.name)
    }

    /// The file to hand to `node`, once [`Package::ensure`] has installed it.
    pub fn entry(&self) -> PathBuf {
        self.package_dir().join(self.entry)
    }

    /// The installed package's own `"version"`, if it's there at all — used to tell whether the
    /// copy on disk matches what settings now ask for.
    pub fn installed_version(&self) -> Option<String> {
        version_from_package_json(&self.package_dir())
    }

    /// Installs the package via `corepack pnpm` (Node's own bundled package-manager launcher —
    /// nothing extra to download for pnpm itself) if it isn't already there, or if a `version` is
    /// given that doesn't match what's already installed — a settings change pinning (or
    /// re-pinning) a version is a deliberate re-install, not left silently unapplied. `None`
    /// installs `default_version` if nothing is there yet, and leaves whatever's already
    /// installed alone from then on.
    pub async fn ensure(&self, version: Option<&str>, default_version: &str) -> Result<(), String> {
        if self.entry().is_file() {
            match version {
                None => return Ok(()),
                Some(wanted) if self.installed_version().as_deref() == Some(wanted) => {
                    return Ok(());
                }
                Some(wanted) => {
                    tracing::info!(
                        wanted,
                        package = self.name,
                        "requested version differs, reinstalling"
                    );
                }
            }
        }
        let dir = Path::new(self.dir);
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| format!("couldn't create {}: {e}", self.dir))?;
        claim_as_own_project(dir, self.project, self.decline_builds).await?;
        let spec = match version {
            Some(v) => format!("{}@{}", self.name, checked_version(v, self.title)?),
            None if default_version == "latest" => format!("{}@latest", self.name),
            None => format!(
                "{}@{}",
                self.name,
                checked_version(default_version, self.title)?
            ),
        };
        tracing::info!(package = %spec, "installing {}", self.title);
        let mut command = Command::new(corepack_binary());
        command.args(["pnpm", "add", &spec]);
        for allowed in self.allow_builds {
            command.arg(format!("--allow-build={allowed}"));
        }
        run(
            command
                .current_dir(dir)
                .env("COREPACK_ENABLE_DOWNLOAD_PROMPT", "0")
                .env("PATH", path_with_managed_node()),
            &format!("pnpm (installing {})", self.title),
        )
        .await?;
        if !self.entry().is_file() {
            return Err(format!(
                "installed {spec} but {} still doesn't exist",
                self.entry().display()
            ));
        }
        Ok(())
    }
}

fn version_from_package_json(package_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(package_dir.join("package.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// Makes `dir` a pnpm project and workspace root of its own, so `pnpm add` installs *there*.
///
/// pnpm doesn't install into the directory it's run from — it installs into the nearest project
/// above it. In a directory with no `package.json` it walks up until it finds one, and whatever
/// it finds (a stray `~/package.json`, the checkout this data directory happens to sit inside)
/// gets the package added to it instead, with pnpm exiting 0 and the folder left empty. A
/// `package.json` alone isn't enough either: a `pnpm-workspace.yaml` anywhere above still takes
/// the lockfile and the `--allow-build` approvals. Both files, right here, stop the walk.
///
/// The workspace file starts with the declined builds (`allowBuilds: { name: false }`), so pnpm
/// knows they were decided rather than forgotten. Neither file is overwritten: after the first
/// install they're pnpm's to keep (the dependency in one, the build approvals in the other).
async fn claim_as_own_project(
    dir: &Path,
    project: &str,
    decline_builds: &[&str],
) -> Result<(), String> {
    let manifest = serde_json::json!({ "name": project, "private": true }).to_string() + "\n";
    let mut workspace = String::new();
    if !decline_builds.is_empty() {
        workspace.push_str("allowBuilds:\n");
        for declined in decline_builds {
            // Quoted, as JSON is YAML: a scoped name starts with `@`, which YAML reserves.
            workspace.push_str(&format!(
                "  {}: false\n",
                serde_json::Value::from(*declined)
            ));
        }
    }
    for (name, contents) in [
        ("package.json", manifest.as_str()),
        ("pnpm-workspace.yaml", workspace.as_str()),
    ] {
        let path = dir.join(name);
        if !path.exists() {
            tokio::fs::write(&path, contents)
                .await
                .map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// Corepack needs `node` reachable on `PATH` to run pnpm with (it shells out to the same
/// interpreter it's running under, resolved by name) — the managed copy isn't on the host's own
/// `PATH`, so it's prepended here rather than relying on one being installed at all.
fn path_with_managed_node() -> std::ffi::OsString {
    let mut path = node_bin_dir().into_os_string();
    if let Ok(existing) = std::env::var("PATH") {
        path.push(":");
        path.push(existing);
    }
    path
}

/// A pinned package version, checked before it goes anywhere near a package spec.
///
/// This value comes from a settings file a person writes, and lands in `pnpm add <name>@…` run
/// with the extension's `host_shell` trust. npm's own spec syntax accepts far more than a version
/// there — an alias, a tarball URL, a git repository — so an unchecked value is a way to have
/// pnpm install and run something else entirely. Only a release version, the thing the setting
/// claims to be: digits and dots, optionally a `-suffix` for a pre-release. `title` names the
/// package in the refusal.
pub fn checked_version<'a>(version: &'a str, title: &str) -> Result<&'a str, String> {
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
            "`{version}` isn't a {title} version: digits and dots, e.g. `2.6.2`, \
             optionally with a `-rc.1` style suffix"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const Z2M: Package = Package {
        name: "zigbee2mqtt",
        title: "Zigbee2MQTT",
        dir: "z2m",
        project: "irori-zigbee2mqtt",
        entry: "index.js",
        allow_builds: &["unix-dgram"],
        decline_builds: &[],
    };

    #[test]
    fn a_pinned_version_is_a_version_and_not_an_npm_spec() {
        for good in ["2.6.2", "2", "2.6.2-rc.1", "1.42.0-dev-3"] {
            assert_eq!(checked_version(good, "Zigbee2MQTT"), Ok(good), "{good}");
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
            let refused = checked_version(bad, "Matter server").expect_err(bad);
            assert!(refused.contains("Matter server version"), "{refused}");
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
            Z2M.entry(),
            Path::new("z2m/node_modules/zigbee2mqtt/index.js")
        );
    }

    /// [`Package::ensure`] runs `corepack` with `current_dir` set to the package's folder, which
    /// is where both the program path and the `PATH` it's given are resolved — `runtime/bin`
    /// relative means `z2m/runtime/bin`, so `corepack` is never found and nothing can install.
    #[test]
    fn node_is_named_absolutely_so_a_child_run_from_elsewhere_still_finds_it() {
        assert!(node_bin_dir().is_absolute(), "{:?}", node_bin_dir());
        assert!(corepack_binary().is_absolute());
        let path = path_with_managed_node();
        let path = path.to_string_lossy().into_owned();
        let first = path.split(':').next().expect("split yields one part");
        assert!(Path::new(first).is_absolute(), "{first}");
    }

    /// Without both files pnpm installs into whichever project it finds further up the tree and
    /// reports success, leaving the folder empty.
    #[tokio::test]
    async fn the_install_directory_is_its_own_project_and_keeps_what_pnpm_wrote_there() {
        let dir = tempfile::tempdir().expect("can create a temp dir");
        claim_as_own_project(dir.path(), "irori-zigbee2mqtt", &[])
            .await
            .expect("can claim");
        let manifest = std::fs::read_to_string(dir.path().join("package.json"))
            .expect("package.json was written");
        let manifest: serde_json::Value =
            serde_json::from_str(&manifest).expect("package.json is JSON");
        assert_eq!(manifest["name"], "irori-zigbee2mqtt");
        assert!(dir.path().join("pnpm-workspace.yaml").is_file());

        let approvals = "allowBuilds:\n  unix-dgram: true\n";
        std::fs::write(dir.path().join("pnpm-workspace.yaml"), approvals).expect("can write");
        claim_as_own_project(dir.path(), "irori-zigbee2mqtt", &["usb"])
            .await
            .expect("can claim again");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("pnpm-workspace.yaml")).expect("still there"),
            approvals
        );
    }

    /// The Matter server's Bluetooth parts are declined in writing, scoped names quoted, so pnpm
    /// installs without asking about them.
    #[tokio::test]
    async fn declined_builds_are_written_down_before_the_first_install() {
        let dir = tempfile::tempdir().expect("can create a temp dir");
        claim_as_own_project(
            dir.path(),
            "irori-matter-server",
            &["@stoprocent/noble", "usb"],
        )
        .await
        .expect("can claim");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("pnpm-workspace.yaml")).expect("written"),
            "allowBuilds:\n  \"@stoprocent/noble\": false\n  \"usb\": false\n"
        );
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
