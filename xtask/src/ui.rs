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

    let status = Command::new("trunk").arg("build").current_dir(&ui).status();
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
    Ok(())
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
