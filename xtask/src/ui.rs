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
    let level = Level::for_release_mode(std::env::var("IRORI_TRUNK_RELEASE").ok().as_deref());
    let compressed = compress_dir(&embedded, level)?;
    println!(
        "UI built: {copied} files, {} KB in {} (plus {compressed} compressed copies)\n\
         Rebuild `irori` to embed it (`cargo build`).",
        total / 1024,
        embedded.display()
    );
    pages(&root)
}

/// File types worth compressing: text, and wasm (which shrinks to about a quarter). Images other
/// than SVG are compressed already.
const COMPRESSIBLE: &[&str] = &["wasm", "js", "css", "html", "svg"];

/// Below this, the saving is smaller than the headers that announce it.
const MIN_SIZE: u64 = 1024;

/// How hard to squeeze. A release build is squeezed as far as it goes, once, for every download
/// after it. The dev image's wasm is unoptimized (about 220 MB), where brotli's top level would
/// take minutes on every rebuild, so it gets quick levels instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Level {
    gzip: u32,
    brotli: u32,
}

impl Level {
    const BEST: Self = Self {
        gzip: 9,
        brotli: 11,
    };
    const QUICK: Self = Self { gzip: 1, brotli: 1 };

    fn for_release_mode(release_mode: Option<&str>) -> Self {
        if release_mode == Some("0") {
            Self::QUICK
        } else {
            Self::BEST
        }
    }
}

/// Writes `<file>.gz` and `<file>.br` next to each compressible file, which the server sends to a
/// browser that accepts them (`server.rs`, `ui::serve`). A copy that comes out no smaller than its
/// original isn't written; the original is served instead. Returns how many copies were written.
fn compress_dir(dir: &Path, level: Level) -> anyhow::Result<usize> {
    let mut written = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let compressible = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| COMPRESSIBLE.contains(&ext));
        if !entry.file_type()?.is_file() || !compressible || entry.metadata()?.len() < MIN_SIZE {
            continue;
        }
        let data = std::fs::read(&path)?;
        for (suffix, packed) in [("gz", gzip(&data, level)?), ("br", brotli(&data, level)?)] {
            if packed.len() < data.len() {
                let mut name = entry.file_name();
                name.push(".");
                name.push(suffix);
                std::fs::write(dir.join(name), packed)?;
                written += 1;
            }
        }
    }
    Ok(written)
}

fn gzip(data: &[u8], level: Level) -> anyhow::Result<Vec<u8>> {
    use std::io::Write as _;
    let mut encoder =
        flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level.gzip));
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

fn brotli(data: &[u8], level: Level) -> anyhow::Result<Vec<u8>> {
    let params = brotli::enc::BrotliEncoderParams {
        quality: level.brotli as i32,
        // The largest window a browser has to accept (RFC 7932's 16 MB).
        lgwin: 24,
        ..Default::default()
    };
    let mut out = Vec::new();
    brotli::BrotliCompress(&mut std::io::Cursor::new(data), &mut out, &params)?;
    Ok(out)
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
    use super::{Level, compress_dir, trunk_command_with};
    use std::io::Read as _;
    use std::path::Path;

    #[test]
    fn compressed_copies_unpack_to_the_original() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let text = "a page that repeats itself. ".repeat(200);
        std::fs::write(dir.path().join("app.js"), &text)?;
        // Too small to be worth it, and a type that isn't text.
        std::fs::write(dir.path().join("tiny.css"), "body{}")?;
        std::fs::write(dir.path().join("photo.png"), text.as_bytes())?;

        let written = compress_dir(dir.path(), Level::QUICK)?;
        assert_eq!(written, 2);

        let mut unzipped = String::new();
        flate2::read::GzDecoder::new(std::fs::File::open(dir.path().join("app.js.gz"))?)
            .read_to_string(&mut unzipped)?;
        assert_eq!(unzipped, text);

        let mut unbrotlied = String::new();
        brotli::Decompressor::new(std::fs::File::open(dir.path().join("app.js.br"))?, 4096)
            .read_to_string(&mut unbrotlied)?;
        assert_eq!(unbrotlied, text);

        for skipped in ["tiny.css.gz", "tiny.css.br", "photo.png.gz", "photo.png.br"] {
            assert!(!dir.path().join(skipped).exists(), "{skipped}");
        }
        Ok(())
    }

    #[test]
    fn dev_image_compresses_quickly() {
        assert_eq!(Level::for_release_mode(Some("0")), Level::QUICK);
        assert_eq!(Level::for_release_mode(None), Level::BEST);
    }

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
