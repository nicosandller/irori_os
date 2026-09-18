// The shared shape check for `MAJOR.MINOR.PATCH[-prerelease]` version strings.
//
// One copy is used by `crates/irori-types/build.rs` (via `include!`, since a build script can't
// import its own crate) and by `parse_version` in `extension.rs`, so what the build gate accepts
// is exactly what `irori_types::Version` parses at runtime. The release workflow's validate step
// mirrors the same rules as a regex; keep that in step with this file.

/// True when `version` is a shape `irori_types::Version` accepts: `MAJOR.MINOR.PATCH`, each part
/// at most nine digits with no leading zeros, an optional dot-separated `-prerelease`, no
/// `+build` metadata, at most 64 characters.
pub fn is_release_version(version: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::is_release_version;

    #[test]
    fn accepts_valid_release_and_pre_release_versions() {
        for ok in [
            "0.0.0",
            "0.2.0",
            "1.4.0",
            "10.0.0",
            "0.3.0-beta.1",
            "1.2.3-alpha",
            "1.2.3-alpha.1.beta",
            "2.0.0-rc.1",
            "0.0.0-20260101",
        ] {
            assert!(is_release_version(ok), "{ok:?} should be accepted");
        }
    }

    #[test]
    fn rejects_every_shape_a_version_must_not_have() {
        for bad in [
            "",
            "0.0",
            "0.0.0.0",
            "x.0.0",
            "0.x.0",
            "00.1.0",
            "1.01.0",
            "1.0.00",
            "1234567890.0.0",
            "0.0.0+sha.abc",
            "0.0.0-alpha..1",
            "0.0.0-alpha_1",
            "0.0.0-alpha.01",
            "0.0.0-",
            "v0.2.0",
            " 0.2.0",
            "0.2.0 ",
            "this-tag-is-way-too-long-0.0.0-000000000000000000000000000000000000000000000000000",
        ] {
            assert!(!is_release_version(bad), "{bad:?} should be rejected");
        }
    }
}
