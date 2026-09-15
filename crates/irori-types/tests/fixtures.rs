//! Golden fixtures: every example in `fixtures/types/` is checked against the Rust types and
//! the generated JSON Schemas. See `fixtures/README.md`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use irori_types::{Area, Device, Entity, EntityState, Floor};
use serde::Serialize;
use serde::de::DeserializeOwned;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/types")
}

fn json_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    Ok(files)
}

fn check<T>(name: &str) -> anyhow::Result<()>
where
    T: DeserializeOwned + Serialize + PartialEq + std::fmt::Debug,
{
    let doc = irori_types::schemas()
        .into_iter()
        .find(|d| d.name == name)
        .with_context(|| format!("no schema named {name}"))?;
    let validator = jsonschema::draft202012::new(doc.schema.as_value())
        .map_err(|e| anyhow::anyhow!("schema {name} is not a valid JSON Schema: {e}"))?;

    let dir = fixtures_dir().join(name);
    let valid = json_files(&dir.join("valid"))?;
    let invalid = json_files(&dir.join("invalid"))?;
    ensure!(!valid.is_empty(), "{name}: no valid fixtures");
    ensure!(!invalid.is_empty(), "{name}: no invalid fixtures");

    for path in &valid {
        let text = std::fs::read_to_string(path)?;
        let json: serde_json::Value = serde_json::from_str(&text)?;
        let what = path.display();

        if let Some(e) = validator.iter_errors(&json).next() {
            bail!(
                "{what}: rejected by the JSON Schema: {e} at {}",
                e.instance_path()
            );
        }
        let value: T = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("{what}: rejected by Rust: {e}"))?;

        // What Irori writes must be readable back, and must itself pass the schema.
        let written = serde_json::to_value(&value)?;
        ensure!(
            validator.is_valid(&written),
            "{what}: serialized form fails the schema: {written}"
        );
        let reread: T = serde_json::from_value(written)?;
        ensure!(reread == value, "{what}: does not round-trip");
    }

    for path in &invalid {
        let text = std::fs::read_to_string(path)?;
        let json: serde_json::Value = serde_json::from_str(&text)?;
        let what = path.display();
        let expected_path = path.with_extension("error.txt");
        let expected = std::fs::read_to_string(&expected_path)
            .with_context(|| format!("{what}: missing {}", expected_path.display()))?;
        let expected = expected.trim();

        match serde_json::from_str::<T>(&text) {
            Ok(v) => bail!("{what}: accepted by Rust, but it's in invalid/: {v:?}"),
            Err(e) => ensure!(
                e.to_string().contains(expected),
                "{what}: error {:?} does not contain {expected:?}",
                e.to_string()
            ),
        }

        let schema_allows = path.to_string_lossy().ends_with(".schema-allows.json");
        match (validator.is_valid(&json), schema_allows) {
            (true, false) => bail!(
                "{what}: accepted by the JSON Schema. If the schema can't express this rule, rename it to *.schema-allows.json"
            ),
            (false, true) => bail!(
                "{what}: the JSON Schema now rejects this; drop `.schema-allows` from the file name"
            ),
            _ => {}
        }
    }
    Ok(())
}

#[test]
fn floors() -> anyhow::Result<()> {
    check::<Floor>("floor")
}

#[test]
fn areas() -> anyhow::Result<()> {
    check::<Area>("area")
}

#[test]
fn devices() -> anyhow::Result<()> {
    check::<Device>("device")
}

#[test]
fn entities() -> anyhow::Result<()> {
    check::<Entity>("entity")
}

#[test]
fn entity_states() -> anyhow::Result<()> {
    check::<EntityState>("entity-state")
}

/// Every schema has a fixtures folder, and every fixtures folder has a schema.
#[test]
fn every_schema_has_fixtures() -> anyhow::Result<()> {
    let mut expected: Vec<String> = irori_types::schemas()
        .iter()
        .map(|d| d.name.to_owned())
        .collect();
    expected.sort();
    let mut folders: Vec<String> = std::fs::read_dir(fixtures_dir())?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    folders.sort();
    ensure!(
        folders == expected,
        "fixture folders {folders:?} != schemas {expected:?}"
    );
    Ok(())
}
