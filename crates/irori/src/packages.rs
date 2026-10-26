//! Official catalog, and how a click on Install turns a listing into a package on disk.
//!
//! Source of the code is this repo. The running binary never links it. Install copies a package
//! into `$DATA/extensions/<id>/` from, in order: packages shipped beside the binary
//! (`IRORI_OFFICIAL_PACKAGES` or `/usr/share/irori/extensions`), a checkout of this repo (builds
//! the crate — only when this isn't the release workflow's own binary, and cargo is on its
//! PATH, so a distributed release always downloads instead, the same as a machine with no
//! checkout at all), or a GitHub release of this repo.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use irori_types::{ExtensionId, Name, Version};
use serde::{Deserialize, Serialize};

const CATALOG: &str = include_str!("../../../extensions/official.toml");
const GITHUB_REPO: &str = "nicosandller/irori_os";

/// One official extension, as the Extensions page lists it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Official {
    pub id: ExtensionId,
    pub name: Name,
    pub category: String,
    pub description: String,
    /// The extension's own version, not Irori's: the one its manifest declares, raised when its
    /// package changes. Install fetches the package this build of Irori shipped, so this is the
    /// version that arrives; an installed copy with another version is offered the update. Kept in step
    /// with the manifest by `the_catalog_has_the_version_each_manifest_declares`.
    pub version: Version,
    pub source: String,
    #[serde(rename = "crate")]
    pub crate_name: String,
    pub bin: String,
    /// Whether installing it means full access to this machine. The Extensions page says so in
    /// those words, and the install route refuses until that is explicitly approved. This has to
    /// live in the catalog, not only in the manifest: a not-yet-installed extension's manifest
    /// isn't on disk on a machine that downloaded the binary. Kept in step with the manifest by
    /// `the_catalog_says_full_access_exactly_when_the_manifest_does`.
    #[serde(default)]
    pub full_access: bool,
}

#[derive(Debug, Deserialize)]
struct CatalogFile {
    extension: Vec<Official>,
}

/// Every official extension, in catalog order.
pub fn official() -> Vec<Official> {
    let file: CatalogFile = toml::from_str(CATALOG).expect("extensions/official.toml is valid");
    file.extension
}

pub fn official_by_id(id: &ExtensionId) -> Option<Official> {
    official().into_iter().find(|item| &item.id == id)
}

/// Where installed packages live (`$DATA/extensions`).
pub fn packages_dir(data: &Path) -> PathBuf {
    data.join("extensions")
}

/// The file an official install leaves in its package, and an install from anywhere else never
/// does: what says a package came from the catalog and not from whoever chose its id.
const OFFICIAL_MARK: &str = ".irori-official";

/// Whether the package installed as `id` under `packages` is the catalog's: the id is one the
/// catalog has, and the package was put there by an official install. Trusted with regular users
/// (`docs/specs/access.md` §4.1) on this and not on the name, which anything can be called.
pub fn is_official_install(packages: &Path, id: &ExtensionId) -> bool {
    static CATALOG_IDS: std::sync::OnceLock<std::collections::BTreeSet<ExtensionId>> =
        std::sync::OnceLock::new();
    CATALOG_IDS
        .get_or_init(|| official().into_iter().map(|item| item.id).collect())
        .contains(id)
        && packages.join(id.as_str()).join(OFFICIAL_MARK).is_file()
}

/// Install an official extension into `dest` (`packages_dir/<id>`), and mark it the catalog's.
pub fn install_official(item: &Official, dest: &Path) -> Result<(), String> {
    fetch_official(item, dest)?;
    fs::write(dest.join(OFFICIAL_MARK), item.id.as_str()).map_err(|e| e.to_string())
}

fn fetch_official(item: &Official, dest: &Path) -> Result<(), String> {
    if let Some(dir) = official_packages_dir()
        && let Ok(()) = copy_package(&dir.join(item.id.as_str()), dest)
    {
        return Ok(());
    }
    // Building from a checkout is a developer convenience, not something the binary this repo
    // actually ships should ever do: that binary runs on machines that may well have the repo
    // cloned too (this one, for instance), and a real install should exercise the same GitHub
    // download every other consumer's install does. `cfg!(debug_assertions)` can't tell those
    // apart — `cargo xtask install` builds in release mode too — so this checks what actually
    // does instead; see `is_distributed_release`.
    if !is_distributed_release() {
        match workspace_root() {
            Some(root) if cargo_runnable() => return build_from_checkout(item, &root, dest),
            // A checkout is there but unusable: pointing at "run from a checkout" as the fix
            // would send them right back to the path that was just ruled out.
            Some(_) => {
                return download_github(
                    item,
                    dest,
                    "this checkout has no cargo on its PATH to build with",
                );
            }
            None => {
                return download_github(
                    item,
                    dest,
                    "in a git checkout, Install builds from source",
                );
            }
        }
    }
    download_github(
        item,
        dest,
        "a distributed release always downloads, never builds from a checkout",
    )
}

/// Whether this reports a released version rather than a development build (`0.7.0-dev`): the
/// release workflow sets `IRORI_VERSION`, and a build made with `HEAD` sitting on an exact release
/// tag picks the same version up on its own (see `irori_types`'s `build.rs`) — both cases where
/// downloading is the right call even for a checkout with a perfectly good cargo in it. A plain
/// `cargo build`, `cargo build --release`, and `cargo xtask install` all report a `-dev` version,
/// which the release workflow never tags.
///
/// A tagged `HEAD` alone isn't enough, though: it says nothing about the working tree, so
/// building at a tag with local edits still reports that tag's version. `COMMIT` (`build.rs` in
/// `crates/irori`) carries the `-modified` suffix for that case, so it's checked too — otherwise
/// a checkout with real local changes would quietly install the stale, unmodified GitHub asset
/// instead of building what's actually on disk.
fn is_distributed_release() -> bool {
    release_build(irori_types::VERSION, crate::build_info::COMMIT)
}

fn release_build(version: &str, commit: &str) -> bool {
    !version.ends_with("-dev") && !commit.ends_with("-modified")
}

fn cargo_runnable() -> bool {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    command_runs(&cargo)
}

fn command_runs(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Install from a tarball URL (non-official, or a pinned official asset).
///
/// The archive comes from an untrusted source, so tar is used only to read: every member name
/// is checked to stay inside `dest`, and each regular file's bytes are written by us, so a
/// `..` member or a symlink inside the archive can never land outside `dest`.
pub fn install_url(url: &str, dest: &Path) -> Result<(), String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("only http:// and https:// URLs are allowed".into());
    }
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let archive = dest.join("download.tar.gz");
    // A download that stalls gives up instead of hanging: the install holds its extension's
    // claim until it ends, so a hang would refuse every Install, Update and Uninstall of it
    // until Irori restarts. Slow is fine; under 1 kB/s for a whole minute isn't.
    run(
        "curl",
        &[
            "-fsSL",
            "--connect-timeout",
            "30",
            "--speed-limit",
            "1024",
            "--speed-time",
            "60",
            "--output",
            archive.to_str().ok_or("package path is not UTF-8")?,
            url,
        ],
    )?;
    install_archive(&archive, dest)?;
    let _ = fs::remove_file(&archive);
    Ok(())
}

/// Reads every member of `archive` and writes each regular file under `dest`, refusing names
/// that could escape it and link members, which `tar -x` would leave lying in wait. A written
/// file keeps the mode the archive records for its member, so a `bin/<name>` entry comes out
/// executable instead of landing at the 0644 that a plain `File::create` gives it.
fn install_archive(archive: &Path, dest: &Path) -> Result<(), String> {
    let archive = archive.to_str().ok_or("package path is not UTF-8")?;
    let listing = run_captured("tar", &["-tzf", archive])?;
    let modes = member_modes(archive)?;
    for name in listing.lines() {
        let normalized = checked_member(name)?;
        if normalized.is_empty() {
            continue;
        }
        // Only an official install says a package is the catalog's; an archive can't.
        if normalized == OFFICIAL_MARK {
            continue;
        }
        if name.ends_with('/') {
            fs::create_dir_all(dest.join(&normalized)).map_err(|e| e.to_string())?;
            continue;
        }
        let out = dest.join(&normalized);
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        run_to_file("tar", &["-xzOf", archive, name], &out)?;
        if let Some(mode) = modes.get(&normalized) {
            fs::set_permissions(&out, PermissionsExt::from_mode(*mode))
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Permission bits of every member of `archive`, keyed by member name as a context-free
/// listing names it. Both util-tar flavors open each verbose line with the ten-character mode
/// string (`-rwxr-xr-x … ./bin/demo`), and end it with the member name. The same `./` is
/// rubbed off a name here as [`checked_member`] rubs off the name in the short listing, so the
/// look-ups line up.
fn member_modes(archive: &str) -> Result<std::collections::BTreeMap<String, u32>, String> {
    let listing = run_captured("tar", &["-tvzpf", archive])?;
    let mut modes = std::collections::BTreeMap::new();
    for line in listing.lines() {
        let name = line
            .split_whitespace()
            .next_back()
            .unwrap_or("")
            .strip_prefix("./")
            .unwrap_or("")
            .trim_end_matches('/');
        let Some(mode) = tar_mode(line) else {
            continue;
        };
        if !name.is_empty() {
            modes.insert(name.to_owned(), mode);
        }
    }
    Ok(modes)
}

/// The `-rwxrwxrwx` part of a verbose tar line, as the raw bits. Anything outside a regular
/// file, a directory, or a link (a device node, say) is left out; we never write those.
fn tar_mode(line: &str) -> Option<u32> {
    let mode = line.chars().take(10).collect::<String>();
    if mode.len() != 10 || !mode.starts_with(['-', 'd', 'l']) {
        return None;
    }
    let mut bits = 0u32;
    for group in 0..3 {
        let mut value = 0u32;
        for c in mode[1 + group * 3..4 + group * 3].chars() {
            value |= match c {
                'r' => 4,
                'w' => 2,
                'x' => 1,
                _ => 0,
            };
        }
        bits |= value << ((2 - group) * 3);
    }
    Some(bits)
}

/// The path a member must land on under `dest`: its name with a leading root and a trailing
/// slash (on a directory) rubbed off. Refuses anything that could escape `dest` — an absolute
/// path, or a `..` or `.` component — because `tar -x` would happily follow both.
fn checked_member(name: &str) -> Result<String, String> {
    let normalized = name
        .strip_prefix("./")
        .unwrap_or(name)
        .trim_end_matches('/');
    if normalized.is_empty() {
        return Ok(normalized.to_owned());
    }
    for part in normalized.split('/') {
        if part.is_empty() || part == ".." || part == "." {
            return Err(format!(
                "the package tarball has a suspicious member `{name}`"
            ));
        }
    }
    Ok(normalized.to_owned())
}

fn official_packages_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("IRORI_OFFICIAL_PACKAGES") {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return Some(path);
        }
    }
    let beside = PathBuf::from("/usr/share/irori/extensions");
    beside.is_dir().then_some(beside)
}

fn workspace_root() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("IRORI_WORKSPACE") {
        let path = PathBuf::from(dir);
        if path.join("extensions/official.toml").is_file() {
            return Some(path);
        }
    }
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("extensions/official.toml").is_file() && dir.join("Cargo.toml").is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn build_from_checkout(item: &Official, root: &Path, dest: &Path) -> Result<(), String> {
    let source = root.join(&item.source);
    if !source.join("irori-extension.toml").is_file() {
        return Err(format!(
            "this checkout has no extension at {}",
            source.display()
        ));
    }
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(&cargo)
        .args(["build", "-p", &item.crate_name, "--bin", &item.bin])
        .current_dir(root)
        .status()
        .map_err(|e| format!("couldn't run cargo: {e}"))?;
    if !status.success() {
        return Err(format!("cargo build -p {} failed", item.crate_name));
    }
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.join("target"));
    let binary = target_dir.join("debug").join(&item.bin);
    if !binary.is_file() {
        return Err(format!("built binary missing at {}", binary.display()));
    }
    // Its page, if it brings one and it isn't built yet. Best effort: without a wasm toolchain
    // the extension still installs, and the shell says the page wasn't built rather than
    // showing an empty frame (`docs/specs/automations.md` §B3).
    if let Some(page) = page_dir(&source)? {
        let dir = source.join(page.as_str());
        if !dir.join("dist/index.html").is_file() {
            match Command::new("trunk")
                .arg("build")
                .current_dir(&dir)
                .status()
            {
                Ok(status) if status.success() => {}
                Ok(status) => {
                    tracing::warn!(dir = %dir.display(), %status, "couldn't build the extension's page")
                }
                Err(error) => {
                    tracing::warn!(%error, "no `trunk` to build the extension's page with; `cargo xtask ui` builds it")
                }
            }
        }
    }
    stage_package(&source, &binary, dest, &item.bin)
}

/// The folder an extension's page lives in, from its manifest's `[[contributes.app]]`.
fn page_dir(source: &Path) -> Result<Option<irori_types::PackagePath>, String> {
    let text = fs::read_to_string(source.join("irori-extension.toml"))
        .map_err(|e| format!("can't read the manifest in {}: {e}", source.display()))?;
    let manifest = irori_protocol::parse_manifest(&text)?;
    Ok(manifest.app().and_then(|app| {
        app.entry
            .as_str()
            .rsplit_once('/')
            .and_then(|(dir, _)| irori_types::PackagePath::try_from(dir.to_owned()).ok())
    }))
}

/// `hint` explains, for this call, why a source build wasn't tried (or that one was and isn't
/// an option here) — so a download failure doesn't point back at a path that was already ruled
/// out.
fn download_github(item: &Official, dest: &Path, hint: &str) -> Result<(), String> {
    let url = release_asset_url_for(item);
    install_url(&url, dest).map_err(|e| {
        format!(
            "couldn't download {url}: {e}. {hint}; \
             a Pi image should ship packages under /usr/share/irori/extensions."
        )
    })
}

/// The release workflow uploads every official extension's tarball under the *app's* tag
/// (`irori_types::VERSION`), not the extension's own declared `version` — `official.toml` rarely
/// bumps an extension's version between app releases, so using `item.version` here would ask
/// GitHub for whatever old release last carried that number, silently installing a stale (and
/// possibly manifest-incompatible) build instead of the one this binary actually shipped with.
/// `item.version` plays no part in this on purpose; see the tests below.
fn release_asset_url_for(item: &Official) -> String {
    release_asset_url(irori_types::VERSION, env!("IRORI_TARGET"), &item.bin)
}

fn release_asset_url(version: &str, target: &str, bin: &str) -> String {
    format!("https://github.com/{GITHUB_REPO}/releases/download/v{version}/{bin}-{target}.tar.gz")
}

fn copy_package(from: &Path, dest: &Path) -> Result<(), String> {
    if !from.join("irori-extension.toml").is_file() {
        return Err(format!("{} is not an extension package", from.display()));
    }
    copy_dir(from, dest)
}

fn stage_package(source: &Path, binary: &Path, dest: &Path, bin_name: &str) -> Result<(), String> {
    fs::create_dir_all(dest.join("bin")).map_err(|e| e.to_string())?;
    fs::copy(
        source.join("irori-extension.toml"),
        dest.join("irori-extension.toml"),
    )
    .map_err(|e| e.to_string())?;
    // The files the manifest itself names — its icon and its `config_schema`. Both are
    // `PackagePath`s the author chose, not fixed names: `config.schema.json` beside the manifest
    // is only a convention, and `schemas/settings.json` is just as valid. Copying fixed names
    // instead installs a package whose manifest points at a file that isn't in it, and
    // `config_schema` then reads as `None` — to the UI, the same as an extension with no
    // settings at all. `xtask/src/package.rs` does this for release tarballs, identically.
    for path in declared_files(source)? {
        if path.overwrites_packaged_file(bin_name) {
            return Err(format!(
                "the manifest's `{path}` would replace a file the package writes itself"
            ));
        }
        let from = source.join(path.as_str());
        if !from.is_file() {
            return Err(format!(
                "the manifest declares `{path}`, which isn't in {}",
                source.display()
            ));
        }
        let to = dest.join(path.as_str());
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::copy(&from, &to).map_err(|e| e.to_string())?;
    }
    fs::copy(binary, dest.join("bin").join(bin_name)).map_err(|e| e.to_string())?;
    // A page built in the source (`<page>/dist/`) goes where its entry says, `<page>/`.
    if let Some(page) = page_dir(source)? {
        let dist = source.join(page.as_str()).join("dist");
        if dist.join("index.html").is_file() {
            copy_dir(&dist, &dest.join(page.as_str()))?;
        }
    }
    Ok(())
}

/// The `icon` and `config_schema` paths an extension's manifest declares, if any. `PackagePath`
/// is what has already rejected an absolute path or one climbing out of the package, so nothing
/// this returns can name a file outside the extension's own directory.
fn declared_files(source: &Path) -> Result<Vec<irori_types::PackagePath>, String> {
    let text = fs::read_to_string(source.join("irori-extension.toml"))
        .map_err(|e| format!("can't read the manifest in {}: {e}", source.display()))?;
    let manifest = irori_protocol::parse_manifest(&text)?;
    Ok([manifest.extension.icon, manifest.extension.config_schema]
        .into_iter()
        .flatten()
        .collect())
}

fn copy_dir(from: &Path, dest: &Path) -> Result<(), String> {
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let dest = dest.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &dest)?;
        } else {
            fs::copy(&from, &dest).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .map_err(|e| format!("couldn't run {cmd}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{cmd} failed"))
    }
}

fn run_captured(cmd: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("couldn't run {cmd}: {e}"))?;
    if !output.status.success() {
        return Err(format!("{cmd} failed"));
    }
    String::from_utf8(output.stdout).map_err(|_| format!("{cmd} output isn't UTF-8"))
}

/// Runs `cmd` with `out` as its stdout, so its whole output lands in one file rather than
/// being buffered in memory.
fn run_to_file(cmd: &str, args: &[&str], out: &Path) -> Result<(), String> {
    let file = fs::File::create(out).map_err(|e| e.to_string())?;
    let status = Command::new(cmd)
        .args(args)
        .stdout(Stdio::from(file))
        .status()
        .map_err(|e| format!("couldn't run {cmd}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{cmd} failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// Each official extension's manifest, read from this checkout.
    fn manifests() -> Vec<(Official, irori_types::ExtensionManifest)> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        official()
            .into_iter()
            .map(|item| {
                let path = root.join(&item.source).join("irori-extension.toml");
                let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                    panic!("no manifest for {} at {}: {e}", item.id, path.display())
                });
                let manifest = irori_protocol::parse_manifest(&text)
                    .unwrap_or_else(|e| panic!("{}'s manifest doesn't parse: {e}", item.id));
                (item, manifest)
            })
            .collect()
    }

    /// The catalog is what the Extensions page knows before anything is installed, and the
    /// manifest is what the extension actually declares. They are written in two files; this is
    /// what stops `full_access` drifting from `host_shell`.
    #[test]
    fn the_catalog_says_full_access_exactly_when_the_manifest_does() {
        for (item, manifest) in manifests() {
            assert_eq!(
                item.full_access,
                manifest.permissions.full_access(),
                "{}",
                item.id
            );
        }
    }

    /// The catalog's version is what an installed copy is compared with, and the manifest's is
    /// what an installed copy reports. If they differ, an extension that was just updated is
    /// offered the same update again, or a newer package is never offered at all. Both files
    /// once said different things for Zigbee (0.2.0 and 0.3.0).
    #[test]
    fn the_catalog_has_the_version_each_manifest_declares() {
        for (item, manifest) in manifests() {
            assert_eq!(
                item.version, manifest.extension.version,
                "{}: extensions/official.toml and {}/irori-extension.toml must give the same \
                 version; raise both",
                item.id, item.source
            );
        }
    }

    /// Builds a `.tar.gz` of `tree`, as the release pipeline does.
    fn tarball(tree: &Path, name: &str) -> PathBuf {
        let archive = tree.with_file_name(name);
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(tree)
            .arg(".")
            .status()
            .expect("tar is on PATH");
        assert!(status.success());
        archive
    }

    /// A source checkout with a manifest saying `declares`, and each of `files` written into it.
    fn checkout(root: &Path, declares: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let source = root.join("source");
        std::fs::create_dir_all(&source).expect("mkdir");
        std::fs::write(
            source.join("irori-extension.toml"),
            format!(
                "[extension]\nid = \"x\"\nname = \"X\"\nversion = \"0.1.0\"\n\
                 irori = \">=0.0.0\"\n{declares}"
            ),
        )
        .expect("write manifest");
        for (path, contents) in files {
            let at = source.join(path);
            if let Some(parent) = at.parent() {
                std::fs::create_dir_all(parent).expect("mkdir");
            }
            std::fs::write(at, contents).expect("write");
        }
        source
    }

    #[test]
    fn staging_from_a_checkout_carries_the_config_schema_along_when_the_extension_has_one() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = checkout(
            root.path(),
            "config_schema = \"config.schema.json\"\n",
            &[("config.schema.json", "{}")],
        );
        let binary = root.path().join("built-binary");
        std::fs::write(&binary, "not a real elf, just bytes").expect("write binary");
        let dest = root.path().join("dest");

        stage_package(&source, &binary, &dest, "irori-ext-x").expect("stages");

        assert_eq!(
            std::fs::read_to_string(dest.join("config.schema.json")).expect("read"),
            "{}"
        );
    }

    /// The manifest is copied first and the declared files after it. A schema path of
    /// `irori-extension.toml` would replace the manifest, and the installed package would no
    /// longer parse.
    #[test]
    fn staging_refuses_a_declared_file_that_would_replace_the_manifest_or_the_binary() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = checkout(
            root.path(),
            "config_schema = \"irori-extension.toml\"\n",
            &[],
        );
        let binary = root.path().join("built-binary");
        std::fs::write(&binary, "not a real elf, just bytes").expect("write binary");
        let dest = root.path().join("dest");

        let error = stage_package(&source, &binary, &dest, "irori-ext-x").expect_err("refused");
        assert!(error.contains("would replace"), "{error}");
        let manifest =
            std::fs::read_to_string(dest.join("irori-extension.toml")).expect("still there");
        assert!(
            manifest.contains("id = \"x\""),
            "the manifest must still be the manifest:\n{manifest}"
        );
    }

    /// `config_schema` is a path the extension's author chose, not a fixed name — a manifest
    /// saying `schemas/settings.json` must get that file, at that path, or the installed package
    /// points at a schema that was never copied and its settings form never appears.
    #[test]
    fn staging_follows_wherever_the_manifest_says_the_schema_is() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = checkout(
            root.path(),
            "config_schema = \"schemas/settings.json\"\nicon = \"art/logo.svg\"\n",
            &[
                ("schemas/settings.json", "{\"title\": \"deep\"}"),
                ("art/logo.svg", "<svg/>"),
            ],
        );
        let binary = root.path().join("built-binary");
        std::fs::write(&binary, "not a real elf, just bytes").expect("write binary");
        let dest = root.path().join("dest");

        stage_package(&source, &binary, &dest, "irori-ext-x").expect("stages");

        assert_eq!(
            std::fs::read_to_string(dest.join("schemas/settings.json")).expect("read"),
            "{\"title\": \"deep\"}"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("art/logo.svg")).expect("read"),
            "<svg/>"
        );
        assert!(!dest.join("config.schema.json").exists());
    }

    /// A manifest naming a file the checkout doesn't have is a broken extension, not something
    /// to install half of and let fail later as "this one just has no settings".
    #[test]
    fn staging_refuses_a_manifest_naming_a_file_that_isnt_there() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = checkout(root.path(), "config_schema = \"config.schema.json\"\n", &[]);
        let binary = root.path().join("built-binary");
        std::fs::write(&binary, "not a real elf, just bytes").expect("write binary");

        let refused = stage_package(&source, &binary, &root.path().join("dest"), "irori-ext-x")
            .expect_err("no such file");
        assert!(refused.contains("config.schema.json"), "{refused}");
    }

    #[test]
    fn staging_from_a_checkout_is_fine_without_a_config_schema() {
        let root = tempfile::tempdir().expect("temp dir");
        let source = checkout(root.path(), "", &[]);
        let binary = root.path().join("built-binary");
        std::fs::write(&binary, "not a real elf, just bytes").expect("write binary");
        let dest = root.path().join("dest");

        stage_package(&source, &binary, &dest, "irori-ext-x").expect("stages");

        assert!(!dest.join("config.schema.json").exists());
    }

    #[test]
    fn a_missing_command_is_not_runnable() {
        assert!(!command_runs("irori-packages-test-no-such-command"));
    }

    #[test]
    fn the_release_url_has_the_expected_shape() {
        let url = release_asset_url("0.4.2", "aarch64-apple-darwin", "irori-ext-demo");
        assert_eq!(
            url,
            "https://github.com/nicosandller/irori_os/releases/download/\
             v0.4.2/irori-ext-demo-aarch64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn the_release_url_selection_ignores_the_items_own_version() {
        // Regression test for the actual bug, not just the formatter: this used to take
        // `item.version` (the extension's own, rarely-bumped declared version) instead of the
        // app's, silently downloading a stale release. A fixture whose declared version differs
        // from the app's own catches a revert to `item.version` that a test only calling
        // `release_asset_url` directly, with no `Official` in the picture, wouldn't.
        let item = Official {
            id: ExtensionId::try_from("demo").expect("a valid id"),
            name: Name::try_from("Demo").expect("a valid name"),
            category: "demo".to_owned(),
            description: String::new(),
            version: Version::try_from("9.9.9").expect("a valid version"),
            source: "extensions/demo".to_owned(),
            crate_name: "irori-protocol-demo".to_owned(),
            bin: "irori-ext-demo".to_owned(),
            full_access: false,
        };
        let url = release_asset_url_for(&item);
        assert!(
            url.contains(&format!("/v{}/", irori_types::VERSION)),
            "expected the app's own version ({}) in {url}",
            irori_types::VERSION
        );
        assert!(
            !url.contains("v9.9.9"),
            "item.version should play no part in {url}"
        );
    }

    #[test]
    fn a_development_build_is_not_a_release() {
        assert!(!release_build("0.7.0-dev", "abc1234"));
        assert!(!release_build(irori_types::VERSION, "abc1234-modified"));
    }

    #[test]
    fn a_tagged_pre_release_is_a_release() {
        assert!(release_build("0.7.0-beta.1", "abc1234"));
    }

    #[test]
    fn a_clean_build_at_a_release_tag_is_a_release() {
        assert!(release_build("0.4.1", "abc1234"));
    }

    #[test]
    fn a_release_tag_with_local_edits_is_not_a_release() {
        // Otherwise a checkout with real changes, built at a tag, would quietly install the
        // stale unmodified GitHub asset instead of what's actually on disk.
        assert!(!release_build("0.4.1", "abc1234-modified"));
    }

    #[test]
    fn a_real_command_is_runnable() {
        // `true` ignores `--version` and just exits 0, the same shape a real `cargo --version`
        // would take — this is standing in for "cargo is on PATH", not testing cargo itself.
        assert!(command_runs("true"));
    }

    #[test]
    fn a_normal_archive_extracts_flat_onto_dest() {
        let tree = tempfile::tempdir().expect("a temp dir");
        let dest = tree.path().join("dest");
        let pkg = tree.path().join("pkg");
        std::fs::create_dir_all(pkg.join("bin")).expect("the pkg dir");
        std::fs::write(pkg.join("irori-extension.toml"), "[extension]\n").expect("the manifest");
        std::fs::write(pkg.join("bin/demo"), "elf-bytes").expect("the binary");
        let archive = tarball(&pkg, "pkg.tar.gz");

        install_archive(&archive, &dest).expect("a clean archive installs");

        assert_eq!(
            std::fs::read_to_string(dest.join("irori-extension.toml"))
                .expect("the manifest is extracted"),
            "[extension]\n"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("bin/demo")).expect("the binary is extracted"),
            "elf-bytes"
        );
    }

    #[test]
    fn an_executable_member_keeps_its_exec_bit() {
        let tree = tempfile::tempdir().expect("a temp dir");
        let dest = tree.path().join("dest");
        let pkg = tree.path().join("pkg");
        std::fs::create_dir_all(pkg.join("bin")).expect("the pkg dir");
        std::fs::write(pkg.join("bin/demo"), "elf-bytes").expect("the binary");
        std::fs::set_permissions(pkg.join("bin/demo"), PermissionsExt::from_mode(0o755))
            .expect("the binary is marked executable");
        std::fs::write(pkg.join("irori-extension.toml"), "[extension]\n").expect("the manifest");
        let archive = tarball(&pkg, "pkg.tar.gz");

        install_archive(&archive, &dest).expect("a clean archive installs");

        let mode = std::fs::metadata(dest.join("bin/demo"))
            .expect("the binary is extracted")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755, "bin/demo must stay executable");
        let manifest = std::fs::metadata(dest.join("irori-extension.toml"))
            .expect("the manifest is extracted")
            .permissions()
            .mode();
        assert_eq!(manifest & 0o777, 0o644, "the manifest stays a plain file");
    }

    /// A package is the catalog's because an official install put it there, not because of the
    /// id its manifest gives it: one fetched from a URL that calls itself `automations`, and
    /// carries the mark in its archive, is not.
    #[test]
    fn only_an_official_install_is_trusted_with_regular_users() {
        let packages = tempfile::tempdir().expect("temp dir");
        let automations = ExtensionId::try_from("automations").expect("an id");
        let somebody_elses = ExtensionId::try_from("zq_somebody_elses").expect("an id");
        // Not installed at all, and installed by anything but an official install.
        assert!(!is_official_install(packages.path(), &automations));
        std::fs::create_dir_all(packages.path().join("automations")).expect("mkdir");
        assert!(!is_official_install(packages.path(), &automations));

        // A tarball that carries the mark is unpacked without it.
        let tree = tempfile::tempdir().expect("temp dir");
        std::fs::write(tree.path().join(OFFICIAL_MARK), "automations").expect("write");
        std::fs::write(tree.path().join("irori-extension.toml"), "x").expect("write");
        let archive = tarball(tree.path(), "forged.tar.gz");
        let dest = packages.path().join("automations");
        install_archive(&archive, &dest).expect("unpacks");
        assert!(dest.join("irori-extension.toml").is_file());
        assert!(
            !dest.join(OFFICIAL_MARK).exists(),
            "an archive can't say it"
        );
        assert!(!is_official_install(packages.path(), &automations));

        // An official install does.
        std::fs::write(dest.join(OFFICIAL_MARK), "automations").expect("write");
        assert!(is_official_install(packages.path(), &automations));
        // And an id the catalog doesn't have never is, whatever is in its folder.
        std::fs::create_dir_all(packages.path().join("zq_somebody_elses")).expect("mkdir");
        std::fs::write(
            packages
                .path()
                .join("zq_somebody_elses")
                .join(OFFICIAL_MARK),
            "x",
        )
        .expect("write");
        assert!(!is_official_install(packages.path(), &somebody_elses));
    }

    #[test]
    fn tar_mode_reads_the_mode_off_a_verbose_line() {
        for (line, expect) in [
            (
                "-rwxr-xr-x  0 izitooi izitooi 850736 Sep 19 22:00 ./bin/demo",
                0o755,
            ),
            (
                "-rw-r--r--  0 izitooi izitooi 427 Sep 18 21:52 ./irori-extension.toml",
                0o644,
            ),
            ("drwxr-xr-x  0 izitooi izitooi 0 Sep 19 22:00 ./bin/", 0o755),
        ] {
            let got = tar_mode(line).expect("a verbose line names a mode");
            assert_eq!(got & 0o7777, expect, "parsing {line}");
        }
        for junk in [
            "",
            "not-a-mode",
            "??rw-r--r--  0 izitooi izitooi 1 Jan 1 00:00 ./fx",
        ] {
            assert_eq!(tar_mode(junk), None, "no mode in {junk:?}");
        }
    }

    #[test]
    fn a_symlink_member_stays_a_file_inside_dest() {
        let tree = tempfile::tempdir().expect("a temp dir");
        let dest = tree.path().join("dest");
        let pkg = tree.path().join("pkg");
        std::fs::create_dir_all(&pkg).expect("the pkg dir");
        let outside = tree.path().join("outside");
        std::fs::write(&outside, "secret").expect("the outside file");
        symlink("../outside", pkg.join("evil")).expect("a fake link in the pkg");
        std::fs::write(pkg.join("ok"), "fine").expect("a normal file");
        let archive = tarball(&pkg, "pkg.tar.gz");

        install_archive(&archive, &dest).expect("a link is neutralized, not followed");

        let evil = dest.join("evil");
        let meta = std::fs::symlink_metadata(&evil).expect("the extracted evil entry");
        assert!(
            meta.is_file(),
            "an archive link must not be recreated as a link"
        );
        assert!(!meta.file_type().is_symlink());
        assert_eq!(
            std::fs::read_to_string(dest.join("ok")).expect("ok is extracted"),
            "fine"
        );
    }

    #[test]
    fn a_dot_dot_member_is_refused() {
        for bad in ["../outside/x", "/etc/passwd", "a/../b", "a/./b"] {
            let err = checked_member(bad)
                .expect_err("the member must be refused")
                .contains("suspicious");
            assert!(err, "{bad} must be refused");
        }
        assert_eq!(
            checked_member("./bin/demo").expect("a ./ prefix is fine"),
            "bin/demo"
        );
        assert_eq!(
            checked_member("irori-extension.toml").expect("a plain name is fine"),
            "irori-extension.toml"
        );
        assert_eq!(checked_member("x/").expect("a trailing slash is fine"), "x");
        assert_eq!(checked_member("./").expect("the archive root is fine"), "");
    }
}
