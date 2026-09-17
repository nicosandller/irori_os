//! Golden fixtures for sequential-engine rule documents.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use irori_rules::Rule;
use serde::Serialize;
use serde::de::DeserializeOwned;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/types")
}

fn fixture_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    Ok(files)
}

fn load<T: DeserializeOwned>(
    path: &Path,
) -> anyhow::Result<(serde_json::Value, Result<T, String>)> {
    let text = std::fs::read_to_string(path)?;
    let json: serde_json::Value = serde_json::from_str(&text)?;
    let value = serde_json::from_str(&text).map_err(|e| e.to_string());
    Ok((json, value))
}

fn check<T>(name: &str) -> anyhow::Result<()>
where
    T: DeserializeOwned + Serialize + PartialEq + std::fmt::Debug,
{
    let doc = irori_rules::schemas()
        .into_iter()
        .find(|d| d.name == name)
        .with_context(|| format!("no schema named {name}"))?;
    let validator = jsonschema::draft202012::new(doc.schema.as_value())
        .map_err(|e| anyhow::anyhow!("schema {name} is not a valid JSON Schema: {e}"))?;

    let dir = fixtures_dir().join(name);
    let valid = fixture_files(&dir.join("valid"))?;
    let invalid = fixture_files(&dir.join("invalid"))?;
    ensure!(!valid.is_empty(), "{name}: no valid fixtures");
    ensure!(!invalid.is_empty(), "{name}: no invalid fixtures");

    for path in &valid {
        let (json, value) = load::<T>(path)?;
        let what = path.display();
        if let Some(e) = validator.iter_errors(&json).next() {
            bail!(
                "{what}: rejected by the JSON Schema: {e} at {}",
                e.instance_path()
            );
        }
        let value = value.map_err(|e| anyhow::anyhow!("{what}: rejected by Rust: {e}"))?;
        let written = serde_json::to_value(&value)?;
        ensure!(
            validator.is_valid(&written),
            "{what}: serialized form fails the schema: {written}"
        );
        let reread: T = serde_json::from_value(written)?;
        ensure!(reread == value, "{what}: does not round-trip");
    }

    for path in &invalid {
        let (json, value) = load::<T>(path)?;
        let what = path.display();
        let expected_path = path.with_extension("error.txt");
        let expected = std::fs::read_to_string(&expected_path)
            .with_context(|| format!("{what}: missing {}", expected_path.display()))?;
        let expected = expected.trim();
        match value {
            Ok(v) => bail!("{what}: accepted by Rust, but it's in invalid/: {v:?}"),
            Err(e) => ensure!(
                e.contains(expected),
                "{what}: error {e:?} does not contain {expected:?}"
            ),
        }
        let schema_allows = path
            .file_stem()
            .is_some_and(|stem| stem.to_string_lossy().ends_with(".schema-allows"));
        match (validator.is_valid(&json), schema_allows) {
            (true, false) => bail!(
                "{what}: accepted by the JSON Schema. If the schema can't express this rule, add `.schema-allows` before the extension"
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
fn rules() -> anyhow::Result<()> {
    check::<Rule>("rule")
}
