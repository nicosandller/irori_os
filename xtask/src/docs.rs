//! Checks the claims in the docs that a machine can check.
//!
//! Prose goes stale quietly. This catches the kind that has already slipped through twice: the
//! default cargo features are stated in three places — `crates/irori/Cargo.toml`, ROADMAP §2.1,
//! and decision D17 — and a change to one is easy to make without the others.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};

pub fn run() -> anyhow::Result<()> {
    let root = workspace_root();
    let manifest = read(&root, "crates/irori/Cargo.toml")?;
    let roadmap = read(&root, "ROADMAP.md")?;

    let defaults = default_features(&manifest)?;
    let mut problems = Vec::new();

    // §2.1 quotes the feature list exactly, so it can be compared exactly.
    let quoted = format!("`default = [{}]`", quote_list(&defaults));
    if !roadmap.contains(&quoted) {
        problems.push(format!(
            "ROADMAP §2.1 doesn't quote the binary's default features.\n     expected: {quoted}\n\
             \x20    (from crates/irori/Cargo.toml)"
        ));
    }

    // D17 used to list compiled-in protocols. Official extensions are packages now, so
    // default features must not include `protocol-*`, and D17 must not claim they are compiled in.
    let d17 = roadmap
        .lines()
        .find(|line| line.starts_with("| D17 |"))
        .context("ROADMAP has no D17 row")?
        .to_lowercase();
    for feature in compiled_in_protocols(&defaults) {
        problems.push(format!(
            "default features still compile `{feature}` into the binary; official extensions are packages"
        ));
    }
    if !d17.contains("installable") && !d17.contains("not cargo features") {
        problems.push(
            "ROADMAP D17 should say official extensions are installable packages, not cargo features"
                .into(),
        );
    }

    if problems.is_empty() {
        println!("docs OK (default features: {})", defaults.join(", "));
        return Ok(());
    }
    let mut message = format!("{} documentation claim(s) don't match:\n", problems.len());
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

fn quote_list(features: &[String]) -> String {
    features
        .iter()
        .map(|feature| format!("\"{feature}\""))
        .collect::<Vec<_>>()
        .join(", ")
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
