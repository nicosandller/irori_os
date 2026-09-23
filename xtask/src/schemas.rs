//! `cargo xtask schemas [--check]`: writes JSON Schemas from `irori-types` (core) and
//! `irori-rules` (sequential engine) to `schemas/`, and each external extension's own
//! `config.schema.json` (its manifest's `config_schema`, `docs/specs/extensions.md` §5) next to
//! its `irori-extension.toml` — or with `--check`, fails if any checked-in file is stale.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};

pub fn run(check: bool) -> anyhow::Result<()> {
    let dir = repo_root().join("schemas");
    let mut docs = irori_types::schemas();
    docs.extend(irori_rules::schemas());

    let mut stale = Vec::new();
    for doc in &docs {
        let path = dir.join(format!("{}.schema.json", doc.name));
        write_or_check(&path, &doc.schema, check, &mut stale)?;
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

    let mut written = docs.len();
    for (package_dir, settings_schema) in extension_config_schemas() {
        let path = repo_root().join(package_dir).join("config.schema.json");
        write_or_check(&path, &settings_schema, check, &mut stale)?;
        written += 1;
    }

    if !stale.is_empty() {
        for path in &stale {
            eprintln!("stale: {}", path.display());
        }
        bail!("JSON Schemas are out of date; run `cargo xtask schemas` and commit the result");
    }
    if check {
        println!("JSON Schemas up to date ({written} files)");
    }
    Ok(())
}

/// Each external extension's `Settings` type, generating the same `config.schema.json` its own
/// manifest points `config_schema` at — written once here rather than by hand, so the file can
/// never drift from the type that actually deserializes it.
fn extension_config_schemas() -> Vec<(&'static str, schemars::Schema)> {
    vec![
        (
            "extensions/protocols/mqtt",
            schemars::schema_for!(irori_protocol_mqtt::settings::Settings),
        ),
        (
            "extensions/protocols/zigbee",
            schemars::schema_for!(irori_protocol_zigbee::settings::Settings),
        ),
    ]
}

fn write_or_check(
    path: &Path,
    schema: &impl serde::Serialize,
    check: bool,
    stale: &mut Vec<PathBuf>,
) -> anyhow::Result<()> {
    let mut json = serde_json::to_string_pretty(schema)?;
    json.push('\n');
    let current = std::fs::read_to_string(path).ok();
    if current.as_deref() == Some(json.as_str()) {
        return Ok(());
    }
    if check {
        stale.push(path.to_path_buf());
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the repository root")
        .to_path_buf()
}
