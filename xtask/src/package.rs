//! Builds every official extension and packs each one as a release asset.
//!
//! `cargo xtask package --target <triple> [--tool build|zigbuild] [--out <dir>]` builds each
//! extension in the catalog (`extensions/official.toml`) for `<triple>` and writes
//! `<out>/<bin>-<triple>.tar.gz`: a tarball of the package just as `crates/irori`'s
//! `stage_package` arranges it, the manifest, the icon, and the binary in `bin/`. The running
//! binary downloads these by name from a GitHub release (see `packages.rs`), so the naming and
//! the layout must stay in step with `download_github` and `install_archive`. The release
//! workflow runs this once per build target.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, bail};
use irori_types::PackagePath;
use serde::Deserialize;

/// One `[[extension]]` in `extensions/official.toml`, enough to build and stage the package.
#[derive(Debug, Deserialize)]
struct Official {
    source: String,
    #[serde(rename = "crate")]
    crate_name: String,
    bin: String,
}

#[derive(Debug, Deserialize)]
struct CatalogFile {
    extension: Vec<Official>,
}

pub fn run() -> anyhow::Result<()> {
    let mut target = None;
    let mut tool = "build".to_owned();
    let mut out = None;
    let mut args = std::env::args().skip(2);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--target" => target = Some(args.next().context("--target needs a value")?),
            "--tool" => tool = args.next().context("--tool needs a value")?,
            "--out" => out = Some(args.next().context("--out needs a value")?),
            other => bail!("unexpected argument to `package`: {other:?}"),
        }
    }
    let target = target.context("package needs --target <triple> (e.g. aarch64-apple-darwin)")?;
    if tool != "build" && tool != "zigbuild" {
        bail!("--tool must be `build` or `zigbuild`, not {tool:?}");
    }
    let root = workspace_root();
    let out = out
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("dist/packages"));
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());

    let text = fs::read_to_string(root.join("extensions/official.toml"))
        .context("extensions/official.toml is missing")?;
    let catalog: CatalogFile =
        toml::from_str(&text).context("extensions/official.toml is invalid")?;
    if catalog.extension.is_empty() {
        bail!("extensions/official.toml lists no extensions");
    }

    fs::create_dir_all(&out).with_context(|| format!("can't create {}", out.display()))?;
    for item in &catalog.extension {
        package(&cargo, &tool, &target, &root, &out, item)?;
    }
    println!(
        "packaged {} extensions into {}",
        catalog.extension.len(),
        out.display()
    );
    Ok(())
}

/// Builds one extension for `target` and writes its `<bin>-<triple>.tar.gz` asset.
fn package(
    cargo: &str,
    tool: &str,
    target: &str,
    root: &Path,
    out: &Path,
    item: &Official,
) -> anyhow::Result<()> {
    let source = root.join(&item.source);
    let manifest = source.join("irori-extension.toml");
    if !manifest.is_file() {
        bail!("this checkout has no extension at {}", source.display());
    }
    build(cargo, tool, target, root, &item.crate_name, &item.bin)?;

    let binary = root
        .join("target")
        .join(target)
        .join("release")
        .join(&item.bin);
    if !binary.is_file() {
        bail!("built binary missing at {}", binary.display());
    }

    let stage = out.join(format!("{}-{}", item.bin, target));
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(stage.join("bin"))
        .with_context(|| format!("can't create {}", stage.display()))?;
    fs::copy(&manifest, stage.join("irori-extension.toml"))?;
    let declared = declared_files(&manifest)?;
    for path in &declared {
        if path.overwrites_packaged_file(&item.bin) {
            bail!(
                "{}'s manifest `{path}` would replace a file the package writes itself",
                item.bin
            );
        }
        let from = source.join(path.as_str());
        if !from.is_file() {
            bail!(
                "{}'s manifest declares `{path}`, which isn't in {}",
                item.bin,
                source.display()
            );
        }
        let to = stage.join(path.as_str());
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&from, &to)?;
    }
    if let Some(page) = page_dir(&manifest)? {
        let dist = source.join(page.as_str()).join("dist");
        if !dist.join("index.html").is_file() {
            bail!(
                "{} brings a page, but {} isn't built; run `cargo xtask ui` first",
                item.bin,
                dist.display()
            );
        }
        copy_dir(&dist, &stage.join(page.as_str()))?;
    }
    let exe = stage.join("bin").join(&item.bin);
    fs::copy(&binary, &exe)?;
    fs::set_permissions(&exe, PermissionsExt::from_mode(0o755))?;

    let archive = out.join(format!("{}-{}.tar.gz", item.bin, target));
    let status = Command::new("tar")
        .arg("-czf")
        .arg(archive.to_str().expect("the output path is UTF-8"))
        .arg("-C")
        .arg(stage.to_str().expect("the stage path is UTF-8"))
        .arg(".")
        .status()
        .context("couldn't run `tar`")?;
    if !status.success() {
        bail!("`tar` failed packaging {}", item.bin);
    }
    let _ = fs::remove_dir_all(&stage);
    let size = fs::metadata(&archive)
        .map(|meta| meta.len())
        .expect("the archive was just written");
    let also = if declared.is_empty() {
        "manifest and binary only".to_owned()
    } else {
        format!(
            "with {}",
            declared
                .iter()
                .map(PackagePath::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    println!("  {}: {size} bytes, {also}", archive.display());
    Ok(())
}

/// The folder of an extension's page (`[[contributes.app]]`'s `entry`, less its file name): the
/// page's own crate in the source, and where its built files go in the package.
fn page_dir(manifest: &Path) -> anyhow::Result<Option<PackagePath>> {
    #[derive(Debug, Deserialize)]
    struct Declared {
        #[serde(default)]
        contributes: Contributes,
    }
    #[derive(Debug, Default, Deserialize)]
    struct Contributes {
        #[serde(default)]
        app: Vec<App>,
    }
    #[derive(Debug, Deserialize)]
    struct App {
        entry: String,
    }
    let text = fs::read_to_string(manifest)
        .with_context(|| format!("can't read {}", manifest.display()))?;
    let declared: Declared = toml::from_str(&text)
        .with_context(|| format!("{} isn't a valid manifest", manifest.display()))?;
    let Some(app) = declared.contributes.app.first() else {
        return Ok(None);
    };
    let Some((dir, _)) = app.entry.rsplit_once('/') else {
        bail!(
            "{}: a page's entry has to be in a folder of its own, like app/index.html",
            manifest.display()
        );
    };
    Ok(Some(PackagePath::try_from(dir.to_owned())?))
}

/// The page crates of every official extension that brings one, for `cargo xtask ui`.
pub fn page_dirs(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let text = fs::read_to_string(root.join("extensions/official.toml"))
        .context("extensions/official.toml is missing")?;
    let catalog: CatalogFile =
        toml::from_str(&text).context("extensions/official.toml is invalid")?;
    let mut dirs = Vec::new();
    for item in &catalog.extension {
        let source = root.join(&item.source);
        if let Some(page) = page_dir(&source.join("irori-extension.toml"))? {
            dirs.push(source.join(page.as_str()));
        }
    }
    Ok(dirs)
}

fn copy_dir(from: &Path, to: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// The files an extension's own manifest names, so they travel with the package.
///
/// `icon` and `config_schema` are `PackagePath`s the author chose — `config.schema.json` next to
/// the manifest by convention, but `schemas/settings.json` is just as valid. Copying fixed names
/// instead would ship a package whose manifest points at a file that was never in it: the icon
/// silently doesn't appear, and `config_schema` reads as `None`, which is the same to the UI as
/// an extension with no settings at all. `crates/irori`'s `stage_package` does the same for the
/// dev-checkout install path, for the same reason.
fn declared_files(manifest: &Path) -> anyhow::Result<Vec<PackagePath>> {
    /// Just the two path fields; everything else in the manifest is somebody else's business
    /// here, and `irori-types`' own manifest type is serialize-only.
    #[derive(Debug, Deserialize)]
    struct Declared {
        extension: DeclaredExtension,
    }
    #[derive(Debug, Deserialize)]
    struct DeclaredExtension {
        #[serde(default)]
        icon: Option<String>,
        #[serde(default)]
        config_schema: Option<String>,
    }

    let text = fs::read_to_string(manifest)
        .with_context(|| format!("can't read {}", manifest.display()))?;
    let declared: Declared = toml::from_str(&text)
        .with_context(|| format!("{} isn't a valid manifest", manifest.display()))?;
    [declared.extension.icon, declared.extension.config_schema]
        .into_iter()
        .flatten()
        // `PackagePath` is what rejects an absolute path or one climbing out of the package,
        // so nothing here can name a file outside the extension's own directory.
        .map(|path| PackagePath::try_from(path).map_err(anyhow::Error::from))
        .collect()
}

/// Builds `<crate>` for `target` the way the release workflow builds the binary itself:
/// `zigbuild` on the Linux cells (static musl), plain `cargo build` elsewhere.
fn build(
    cargo: &str,
    tool: &str,
    target: &str,
    root: &Path,
    crate_name: &str,
    bin: &str,
) -> anyhow::Result<()> {
    let mut command = Command::new(cargo);
    match tool {
        "build" => {
            command.arg("build");
        }
        "zigbuild" => {
            command.arg("zigbuild");
        }
        other => bail!("--tool must be `build` or `zigbuild`, not {other:?}"),
    }
    command
        .args([
            "--locked",
            "--release",
            "--target",
            target,
            "-p",
            crate_name,
            "--bin",
            bin,
        ])
        .current_dir(root);
    let status = command
        .status()
        .with_context(|| format!("couldn't run `{cargo}`"))?;
    if !status.success() {
        bail!("building {crate_name} failed ({status})");
    }
    Ok(())
}

/// `xtask/` sits one level below the workspace root, wherever the repository is checked out.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ has a parent")
        .to_path_buf()
}
