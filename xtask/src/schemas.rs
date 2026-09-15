//! `cargo xtask schemas [--check]`: writes the JSON Schemas generated from `irori-types` to
//! `schemas/`, or with `--check`, fails if the checked-in files are stale.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};

pub fn run(check: bool) -> anyhow::Result<()> {
    let dir = repo_root().join("schemas");
    let docs = irori_types::schemas();

    let mut stale = Vec::new();
    for doc in &docs {
        let path = dir.join(format!("{}.schema.json", doc.name));
        let mut json = serde_json::to_string_pretty(&doc.schema)?;
        json.push('\n');
        let current = std::fs::read_to_string(&path).ok();
        if current.as_deref() == Some(json.as_str()) {
            continue;
        }
        if check {
            stale.push(path);
        } else {
            std::fs::create_dir_all(&dir)?;
            std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
            println!("wrote {}", path.display());
        }
    }

    // Schemas for types that no longer exist.
    let expected: Vec<String> = docs
        .iter()
        .map(|d| format!("{}.schema.json", d.name))
        .collect();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".schema.json") && !expected.contains(&name) {
                if check {
                    stale.push(entry.path());
                } else {
                    std::fs::remove_file(entry.path())?;
                    println!("removed {}", entry.path().display());
                }
            }
        }
    }

    if !stale.is_empty() {
        for path in &stale {
            eprintln!("stale: {}", path.display());
        }
        bail!("JSON Schemas are out of date; run `cargo xtask schemas` and commit the result");
    }
    if check {
        println!("JSON Schemas up to date ({} files)", docs.len());
    }
    Ok(())
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the repository root")
        .to_path_buf()
}
