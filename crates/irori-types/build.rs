//! Resolves the version this build reports: `IRORI_VERSION` when the release job sets it, else an
//! exact tag on `HEAD`, else the workspace's `0.0.0`. It lives in the crate both the binary and
//! the core depend on, so `irori version` and the extension-compatibility check can't disagree.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=IRORI_VERSION");
    // Run on every build: a new tag on `HEAD` changes the version without changing a file, and a
    // path that never exists always counts as changed (the same trick as `crates/irori/build.rs`).
    println!("cargo:rerun-if-changed=.irori-always-rerun");
    println!("cargo:rustc-env=IRORI_VERSION={}", version());
}

/// A release sets `IRORI_VERSION` from the tag; a build made at an exact tag (a person running
/// `cargo build` after `git tag`) picks it up on its own; anything else falls back to the
/// workspace's `0.0.0`. A leading `v` is dropped, so the tag `v0.2.0` prints as `0.2.0`.
fn version() -> String {
    if let Ok(version) = std::env::var("IRORI_VERSION") {
        let version = version.trim().trim_start_matches('v');
        if !version.is_empty() {
            assert!(
                is_version(version),
                "IRORI_VERSION={version:?} isn't MAJOR.MINOR.PATCH with an optional -prerelease"
            );
            return version.to_owned();
        }
    }
    if let Some(tag) = git(&["describe", "--tags", "--exact-match"]) {
        let version = tag.trim_start_matches('v');
        if is_version(version) {
            return version.to_owned();
        }
    }
    std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_owned())
}

/// The shape `irori_types::Version` accepts: `MAJOR.MINOR.PATCH`, optional `-prerelease`, no
/// leading zeros, at most 9 digits a part, no `+build`. Kept in step with `parse_version` in
/// `crates/irori-types/src/extension.rs`; a tag that doesn't match must not become `VERSION`,
/// because `Core::new` parses it with `expect` and would panic at startup.
fn is_version(version: &str) -> bool {
    if version.len() > 64 || version.contains('+') {
        return false;
    }
    let (release, pre) = match version.split_once('-') {
        Some((release, pre)) => (release, Some(pre)),
        None => (version, None),
    };
    let number = |part: &str| {
        !part.is_empty()
            && part.len() <= 9
            && part.bytes().all(|b| b.is_ascii_digit())
            && !(part.len() > 1 && part.starts_with('0'))
    };
    let mut parts = release.split('.');
    let release_ok = number(parts.next().unwrap_or(""))
        && number(parts.next().unwrap_or(""))
        && number(parts.next().unwrap_or(""))
        && parts.next().is_none();
    if !release_ok {
        return false;
    }
    match pre {
        None => true,
        Some(pre) => pre.split('.').all(|id| {
            !id.is_empty()
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !(id.len() > 1 && id.starts_with('0') && id.bytes().all(|b| b.is_ascii_digit()))
        }),
    }
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
        .map(|text| text.trim().to_owned())
}
