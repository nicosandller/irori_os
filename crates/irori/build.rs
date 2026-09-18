//! Records what this binary was built from, for `irori version` and `/api/health`: the version,
//! the target triple, the commit, and when.
//!
//! The workspace version stays `0.0.0` between releases, so the version a release binary reports
//! comes from the tag instead: `IRORI_VERSION` when the release job sets it, else an exact tag on
//! `HEAD`, else the manifest version. The commit is still what tells two development builds apart.

use std::process::Command;

fn main() {
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_owned());
    println!("cargo:rustc-env=IRORI_TARGET={target}");
    println!("cargo:rustc-env=IRORI_VERSION={}", version());
    println!("cargo:rustc-env=IRORI_COMMIT={}", commit());
    println!("cargo:rustc-env=IRORI_BUILT_AT={}", built_at());
    // Run on every build. Naming any real path here would opt out of Cargo's own change
    // tracking and pin this to those paths alone — and then editing a file in another crate,
    // or committing, would leave `IRORI_COMMIT` and `IRORI_BUILT_AT` describing an older build
    // than the one being run. A path that never exists is always "changed", which is what
    // makes this honest; two `git` calls per build is the price.
    println!("cargo:rerun-if-changed=.irori-always-rerun");
}

/// The version to report. A release sets `IRORI_VERSION` from the tag; a build made at an exact
/// tag (a person running `cargo build` after `git tag`) picks it up on its own; anything else
/// falls back to the workspace's `0.0.0`. A leading `v` is dropped, so the tag `v0.2.0` prints
/// as `0.2.0`.
fn version() -> String {
    if let Ok(version) = std::env::var("IRORI_VERSION") {
        let version = version.trim().trim_start_matches('v');
        if !version.is_empty() {
            return version.to_owned();
        }
    }
    if let Some(tag) = git(&["describe", "--tags", "--exact-match"]) {
        let version = tag.trim_start_matches('v');
        if !version.is_empty() {
            return version.to_owned();
        }
    }
    std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_owned())
}

/// The short commit, with `-modified` when the working tree has uncommitted changes. `unknown`
/// when there's no git here (a tarball, or a container that didn't copy `.git`), which is a
/// fact worth showing rather than a failure worth stopping for.
fn commit() -> String {
    let Some(commit) = git(&["rev-parse", "--short=7", "HEAD"]) else {
        return "unknown".to_owned();
    };
    match git(&["status", "--porcelain", "--untracked-files=no"]) {
        Some(changes) if !changes.is_empty() => format!("{commit}-modified"),
        _ => commit,
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

/// When this build ran, in UTC, to the minute. `SOURCE_DATE_EPOCH` is honoured so a
/// reproducible build stays reproducible.
fn built_at() -> String {
    let seconds = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|epoch| epoch.parse::<i64>().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| i64::try_from(since.as_secs()).unwrap_or(0))
        });
    format_utc(seconds)
}

/// `YYYY-MM-DD HH:MM UTC` from a Unix timestamp. Written out rather than pulled in: a build
/// script that needs a date library is a dependency every build pays for.
fn format_utc(seconds: i64) -> String {
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (hour, minute) = (rest / 3600, (rest % 3600) / 60);
    // Days since 1970-01-01, by the civil-from-days algorithm (Howard Hinnant's, public domain).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = era * 400 + yoe + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} UTC")
}
