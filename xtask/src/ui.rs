//! Builds the web UI (`crates/irori-ui`) and puts it where the binary embeds it from.
//!
//! The UI is a separate wasm crate built by `trunk`, not by the workspace's `cargo build`, so a
//! plain `cargo build` needs no wasm toolchain and still produces a working binary (it serves the
//! placeholder page in `crates/irori/assets/` instead). Run this, then rebuild `irori`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, bail};

/// Kept out of the repository except for the note that holds the folder (see `.gitignore`).
const KEEP: &str = ".gitkeep";

pub fn run() -> anyhow::Result<()> {
    let root = workspace_root();
    let ui = root.join("crates/irori-ui");
    let dist = ui.join("dist");
    let embedded = root.join("crates/irori/ui");

    let status = trunk_command(&ui).status();
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => bail!("`trunk build` failed ({status})"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
            "`trunk` isn't installed. It builds the UI to wasm:\n\
             \n    cargo install trunk --locked\n    rustup target add wasm32-unknown-unknown\n"
        ),
        Err(e) => return Err(e).context("failed to run `trunk`"),
    }

    // Replace what's embedded rather than merge: filenames carry a content hash, so stale files
    // would pile up build after build and end up inside the binary.
    empty(&embedded)?;
    let mut copied = 0;
    for entry in std::fs::read_dir(&dist).with_context(|| format!("no {}", dist.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), embedded.join(entry.file_name()))?;
            copied += 1;
        }
    }
    if copied == 0 {
        bail!("`trunk build` produced nothing in {}", dist.display());
    }

    let total: u64 = std::fs::read_dir(&embedded)?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len())
        .sum();
    println!(
        "UI built: {copied} files, {} KB in {}\nRebuild `irori` to embed it (`cargo build`).",
        total / 1024,
        embedded.display()
    );
    pages(&root)
}

/// Builds the pages official extensions bring (`[[contributes.app]]`), each into its own
/// `dist/`, where packaging picks it up (`cargo xtask package`, and Install from a checkout).
fn pages(root: &Path) -> anyhow::Result<()> {
    for dir in crate::package::page_dirs(root)? {
        let status = trunk_command(&dir).status();
        match status {
            Ok(status) if status.success() => {
                println!("page built: {}", dir.join("dist").display())
            }
            Ok(status) => bail!("`trunk build` failed in {} ({status})", dir.display()),
            Err(e) => return Err(e).context("failed to run `trunk`"),
        }
    }
    Ok(())
}

/// Trunk.toml asks for a release build. The dev image sets `IRORI_TRUNK_RELEASE=0`
/// so a container rebuild skips wasm-opt and LTO. CI leaves the variable unset
/// and still gets the release page. `--release false` is trunk's own override
/// (`num_args = 0..=1`); a bare `--release` would turn release on.
fn trunk_command(dir: &Path) -> Command {
    trunk_command_with(std::env::var("IRORI_TRUNK_RELEASE").ok().as_deref(), dir)
}

fn trunk_command_with(release_mode: Option<&str>, dir: &Path) -> Command {
    let mut command = Command::new("trunk");
    command.arg("build").current_dir(dir);
    if release_mode == Some("0") {
        command.args(["--release", "false"]);
    }
    command
}

/// Empties the embed folder, keeping the note that holds it in the repository.
fn empty(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name() == KEEP {
            continue;
        }
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(entry.path())?;
        } else {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Builds the UI and installs the binary, so `irori run` works from anywhere.
pub fn install() -> anyhow::Result<()> {
    run()?;
    let root = workspace_root();
    println!("installing the irori binary...");
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["install", "--locked", "--path", "crates/irori"])
        .current_dir(&root)
        .status()
        .context("failed to run `cargo install`")?;
    if !status.success() {
        bail!("`cargo install` failed ({status})");
    }
    println!(
        "\nInstalled. Start Irori from anywhere with:\n\n    irori run\n\n\
         If that isn't found, add cargo's bin directory to your PATH:\n\n    \
         export PATH=\"$HOME/.cargo/bin:$PATH\"\n"
    );
    Ok(())
}

fn workspace_root() -> PathBuf {
    // `xtask/` is one level below the workspace root, wherever the repository is checked out.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ has a parent")
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::trunk_command_with;
    use std::path::Path;

    #[test]
    fn dev_image_builds_the_pages_without_release() {
        let command = trunk_command_with(Some("0"), Path::new("."));
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["build", "--release", "false"]);
    }

    #[test]
    fn ci_keeps_the_trunk_toml_release_default() {
        let command = trunk_command_with(None, Path::new("."));
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["build"]);
    }
}
