//! Resolves the version this build reports: `IRORI_VERSION` when the release job sets it, else an
//! exact tag on `HEAD`, else the next release as a development build (`0.7.0-dev`, from the
//! `next-release` file beside this one). It lives in the crate both the binary and
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
    println!("cargo:rerun-if-changed=next-release");
    println!("cargo:rustc-env=IRORI_VERSION={}", version());
}

/// A release sets `IRORI_VERSION` from the tag; a build made at an exact tag (a person running
/// `cargo build` after `git tag`) picks it up on its own; anything else is a development build of
/// the next release, `<next-release>-dev`. A leading `v` is dropped, so the tag `v0.2.0` prints as
/// `0.2.0`.
///
/// Why the next release rather than `0.0.0`: extensions say which Irori they need (`irori =
/// ">=0.7.0"`), and a pre-release counts as its release (`docs/specs/extensions.md` §5). A
/// development build that called itself `0.0.0` couldn't run the extensions built beside it. It
/// can't come from git: the dev container and CI build without the tags.
fn version() -> String {
    if let Ok(version) = std::env::var("IRORI_VERSION") {
        let version = version.trim();
        if !version.is_empty() {
            // One leading `v` at most, matching the release workflow's `${GITHUB_REF_NAME#v}`:
            // `vv…` and a bare `v` must be rejected, not silently accepted as a valid version.
            let version = version.strip_prefix('v').unwrap_or(version);
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
    let next = std::fs::read_to_string("next-release").expect("crates/irori-types/next-release");
    let next = next.trim();
    assert!(
        release_version::is_release_version(next) && !next.contains('-'),
        "next-release holds {next:?}; it must be the next MAJOR.MINOR.PATCH, e.g. 0.7.0"
    );
    format!("{next}-dev")
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
