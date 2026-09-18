//! Official catalog, and how a click on Install turns a listing into a package on disk.
//!
//! Source of the code is this repo. The running binary never links it. Install copies a package
//! into `$DATA/extensions/<id>/` from, in order: a checkout of this repo (builds the crate),
//! packages shipped beside the binary (`IRORI_OFFICIAL_PACKAGES` or `/usr/share/irori/extensions`),
//! or a GitHub release of this repo.

use std::fs;
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
    if let Some(root) = workspace_root() {
        return build_from_checkout(item, &root, dest);
    }
    download_github(item, dest)
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
/// that could escape it and link members, which `tar -x` would leave lying in wait.
fn install_archive(archive: &Path, dest: &Path) -> Result<(), String> {
    let archive = archive.to_str().ok_or("package path is not UTF-8")?;
    let listing = run_captured("tar", &["-tzf", archive])?;
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
    }
    Ok(())
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

fn download_github(item: &Official, dest: &Path) -> Result<(), String> {
    let target = env!("IRORI_TARGET");
    let url = format!(
        "https://github.com/{GITHUB_REPO}/releases/download/v{}/{bin}-{target}.tar.gz",
        item.version,
        bin = item.bin
    );
    install_url(&url, dest).map_err(|e| {
        format!(
            "couldn't download {url}: {e}. In a git checkout, Install builds from source; \
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
