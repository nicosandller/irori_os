//! Checks the claims in the docs that a machine can check.
//!
//! Official extensions ship as installable packages, never as cargo features compiled into
//! `irori`; this keeps `crates/irori/Cargo.toml`'s default features honest about that.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};

pub fn run() -> anyhow::Result<()> {
    let root = workspace_root();
    let manifest = read(&root, "crates/irori/Cargo.toml")?;

    let defaults = default_features(&manifest)?;
    let mut problems = Vec::new();

    // Official extensions are packages, not cargo features: default features must not compile
    // a `protocol-*` extension into the binary.
    for feature in compiled_in_protocols(&defaults) {
        problems.push(format!(
            "default features still compile `{feature}` into the binary; official extensions are packages"
        ));
    }
    if problems.is_empty() {
        println!("docs OK (default features: {})", defaults.join(", "));
        return Ok(());
    }
    let mut message = format!("{} claim(s) don't match:\n", problems.len());
    for problem in &problems {
        message.push_str(&format!("  - {problem}\n"));
    }
    bail!(message);
}

/// Whether `text` names `word`, as a word: "matters" doesn't mention Matter, and a check that
/// thinks it does is a check nobody will keep.
#[cfg(test)]
fn mentions(text: &str, word: &str) -> bool {
    let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    text.match_indices(word).any(|(at, _)| {
        boundary(text[..at].chars().next_back()) && boundary(text[at + word.len()..].chars().next())
    })
}

/// The `default = [...]` list from a Cargo manifest's `[features]`.
fn default_features(manifest: &str) -> anyhow::Result<Vec<String>> {
    let features = manifest
        .split("[features]")
        .nth(1)
        .context("crates/irori/Cargo.toml has no [features] section")?;
    let line = features
        .lines()
        .find(|line| line.trim_start().starts_with("default"))
        .context("crates/irori/Cargo.toml has no `default` feature list")?;
    let list = line
        .split_once('[')
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(inside, _)| inside)
        .context("the `default` feature list isn't a bracketed list")?;
    Ok(list
        .split(',')
        .map(|item| item.trim().trim_matches('"').to_owned())
        .filter(|item| !item.is_empty())
        .collect())
}

/// Default features that would compile a protocol extension into the binary — always a bug now
/// that official extensions are packages (D17), not cargo features.
fn compiled_in_protocols(defaults: &[String]) -> Vec<&String> {
    defaults
        .iter()
        .filter(|f| f.starts_with("protocol-"))
        .collect()
}

fn read(root: &Path, relative: &str) -> anyhow::Result<String> {
    std::fs::read_to_string(root.join(relative)).with_context(|| format!("can't read {relative}"))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ has a parent")
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_feature_list_is_read_from_the_manifest() {
        let manifest = "[package]\nname = \"irori\"\n\n[features]\n\
                        default = [\"protocol-mqtt\", \"ui\"]\nprotocol-mqtt = []\n";
        assert_eq!(
            default_features(manifest).expect("a default list"),
            ["protocol-mqtt", "ui"]
        );
    }

    #[test]
    fn a_protocol_feature_compiled_into_defaults_is_reported() {
        let defaults = ["protocol-mqtt".to_owned(), "ui".to_owned()];
        assert_eq!(compiled_in_protocols(&defaults), vec![&defaults[0]]);
        assert!(compiled_in_protocols(&["ui".to_owned()]).is_empty());
    }

    #[test]
    fn a_manifest_without_features_says_so_rather_than_passing() {
        assert!(default_features("[package]\nname = \"irori\"\n").is_err());
    }

    #[test]
    fn a_word_inside_another_word_is_not_a_mention() {
        assert!(!mentions("the test that matters", "matter"));
        assert!(!mentions("demolition", "demo"));
        assert!(mentions("the mqtt, demo and esphome extensions", "demo"));
        assert!(mentions("ends with esphome", "esphome"));
    }

    /// The check has to hold for this repository, or it isn't checking anything.
    #[test]
    fn the_repository_itself_passes() {
        run().expect("the docs match the manifest");
    }
}
