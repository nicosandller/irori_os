//! Resolves the version this build reports: `IRORI_VERSION` when the release job sets it, else an
//! exact tag on `HEAD`, else the workspace's `0.0.0`. It lives in the crate both the binary and
//! the core depend on, so `irori version` and the extension-compatibility check can't disagree.

use std::process::Command;

// A build script can't import its own crate, so the version shape check is shared by `include!`;
// `extension.rs` uses the same file, so the gate and the runtime parser can't drift.
mod release_version {
    include!("src/release_version.rs");
}

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
        let version = version.trim();
        // One leading `v` at most, matching the release workflow's `${GITHUB_REF_NAME#v}`: a
        // `vv…` tag must not be accepted as a valid version.
        let version = version.strip_prefix('v').unwrap_or(version);
        if !version.is_empty() {
            assert!(
                release_version::is_release_version(version),
                "IRORI_VERSION={version:?} isn't MAJOR.MINOR.PATCH with an optional -prerelease"
            );
            return version.to_owned();
        }
    }
    if let Some(tag) = git(&["describe", "--tags", "--exact-match"]) {
        let version = tag.strip_prefix('v').unwrap_or(tag.as_str());
        if release_version::is_release_version(version) {
            return version.to_owned();
        }
    }
    std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_owned())
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
