//! Golden fixtures: every example in `fixtures/types/` is checked against the Rust types and
//! the generated JSON Schemas. See `fixtures/README.md`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use irori_types::{
    Area, Device, DeviceDescription, Entity, EntityDescription, EntityState, ExtensionManifest,
    Floor, Rule, ServiceCall, StateReport,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/types")
}

/// Fixture documents: `.json`, or `.toml` for files people write by hand (extension manifests).
fn fixture_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json" || e == "toml"))
        .collect();
    files.sort();
    Ok(files)
}

/// Reads a fixture into the JSON data model, and deserializes it the way Irori will: JSON
/// straight from the text, TOML through the same JSON value the schema checks.
fn load<T: DeserializeOwned>(
    path: &Path,
) -> anyhow::Result<(serde_json::Value, Result<T, String>)> {
    let text = std::fs::read_to_string(path)?;
    if path.extension().is_some_and(|e| e == "toml") {
        let json: serde_json::Value =
            toml::from_str(&text).with_context(|| format!("{}: not valid TOML", path.display()))?;
        let value = serde_json::from_value(json.clone()).map_err(|e| e.to_string());
        Ok((json, value))
    } else {
        let json: serde_json::Value = serde_json::from_str(&text)?;
        let value = serde_json::from_str(&text).map_err(|e| e.to_string());
        Ok((json, value))
    }
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

#[test]
fn extension_manifests() -> anyhow::Result<()> {
    check::<ExtensionManifest>("extension-manifest")
}

#[test]
fn device_descriptions() -> anyhow::Result<()> {
    check::<DeviceDescription>("device-description")
}

#[test]
fn entity_descriptions() -> anyhow::Result<()> {
    check::<EntityDescription>("entity-description")
}

#[test]
fn state_reports() -> anyhow::Result<()> {
    check::<StateReport>("state-report")
}

#[test]
fn service_calls() -> anyhow::Result<()> {
    check::<ServiceCall>("service-call")
}

#[test]
fn rules() -> anyhow::Result<()> {
    check::<Rule>("rule")
}

/// Contribution kinds this version doesn't implement are kept, and named in warnings.
#[test]
fn manifest_warnings_name_ignored_contributions() -> anyhow::Result<()> {
    let dir = fixtures_dir().join("extension-manifest/valid");
    let (_, manifest) = load::<ExtensionManifest>(&dir.join("future_kinds_are_ignored.toml"))?;
    let manifest = manifest.map_err(anyhow::Error::msg)?;
    assert_eq!(
        manifest.warnings(),
        [
            "extension `home_overview`: `contributes.dashboard` isn't supported by this version of Irori yet; ignoring it",
            "extension `home_overview`: `contributes.theme` isn't a contribution kind this version of Irori knows; ignoring it",
        ]
    );
    assert_eq!(
        manifest.integration_id().map(String::from).as_deref(),
        Some("home_overview")
    );

    let (_, builtin) = load::<ExtensionManifest>(&dir.join("esphome_builtin.toml"))?;
    let builtin = builtin.map_err(anyhow::Error::msg)?;
    assert!(builtin.warnings().is_empty());
    assert!(!builtin.permissions.full_access());

    let (_, terminal) = load::<ExtensionManifest>(&dir.join("terminal_app_full_access.toml"))?;
    let terminal = terminal.map_err(anyhow::Error::msg)?;
    assert!(terminal.permissions.full_access());
    assert_eq!(terminal.integration_id(), None);
    Ok(())
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
