//! Official catalog, and how a click on Install turns a listing into a package on disk.
//!
//! Source of the code is this repo. The running binary never links it. Install copies a package
//! into `$DATA/extensions/<id>/` from, in order: a checkout of this repo (builds the crate),
//! packages shipped beside the binary (`IRORI_OFFICIAL_PACKAGES` or `/usr/share/irori/extensions`),
//! or a GitHub release of this repo.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
pub fn install_url(url: &str, dest: &Path) -> Result<(), String> {
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
    run(
        "tar",
        &[
            "-xzf",
            archive.to_str().ok_or("package path is not UTF-8")?,
            "-C",
            dest.to_str().ok_or("package path is not UTF-8")?,
        ],
    )?;
    let _ = fs::remove_file(archive);
    Ok(())
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
