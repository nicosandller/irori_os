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

use irori_types::{ExtensionId, Name};
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
    pub version: String,
    pub source: String,
    #[serde(rename = "crate")]
    pub crate_name: String,
    pub bin: String,
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

/// Install an official extension into `dest` (`packages_dir/<id>`).
pub fn install_official(item: &Official, dest: &Path) -> Result<(), String> {
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

/// Whether this reports a real version rather than the workspace's own `0.0.0`: the release
/// workflow sets `IRORI_VERSION`, and a build made with `HEAD` sitting on an exact release tag
/// picks the same version up on its own (see `irori_types`'s `build.rs`) — both cases where
/// downloading is the right call even for a checkout with a perfectly good cargo in it. A plain
/// `cargo build`, `cargo build --release`, and `cargo xtask install` all still report `0.0.0`.
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
    version != "0.0.0" && !commit.ends_with("-modified")
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
    run(
        "curl",
        &[
            "-fsSL",
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
    stage_package(&source, &binary, dest, &item.bin)
}

/// `hint` explains, for this call, why a source build wasn't tried (or that one was and isn't
/// an option here) — so a download failure doesn't point back at a path that was already ruled
/// out.
fn download_github(item: &Official, dest: &Path, hint: &str) -> Result<(), String> {
    let target = env!("IRORI_TARGET");
    let url = format!(
        "https://github.com/{GITHUB_REPO}/releases/download/v{}/{bin}-{target}.tar.gz",
        item.version,
        bin = item.bin
    );
    install_url(&url, dest).map_err(|e| {
        format!(
            "couldn't download {url}: {e}. {hint}; \
             a Pi image should ship packages under /usr/share/irori/extensions."
        )
    })
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
    let icon = source.join("icon.svg");
    if icon.is_file() {
        fs::copy(&icon, dest.join("icon.svg")).map_err(|e| e.to_string())?;
    }
    fs::copy(binary, dest.join("bin").join(bin_name)).map_err(|e| e.to_string())?;
    Ok(())
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

    #[test]
    fn a_missing_command_is_not_runnable() {
        assert!(!command_runs("irori-packages-test-no-such-command"));
    }

    #[test]
    fn the_workspaces_own_version_is_not_a_release() {
        assert!(!release_build("0.0.0", "abc1234"));
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
