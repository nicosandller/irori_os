//! What this binary is and what was compiled into it.

use std::fmt;

use serde::Serialize;

/// The release tag for a tagged build, otherwise the workspace's `0.0.0`. See `irori-types`.
pub const VERSION: &str = irori_types::VERSION;
/// The commit this was built from, `-modified` if the tree had changes, or `unknown` without
/// git. Development builds are all `0.0.0`, so this is what tells two apart.
pub const COMMIT: &str = env!("IRORI_COMMIT");
pub const BUILT_AT: &str = env!("IRORI_BUILT_AT");

#[derive(Debug, Serialize)]
pub struct BuildInfo {
    pub version: &'static str,
    pub commit: &'static str,
    pub built_at: &'static str,
    pub target: &'static str,
    pub features: Vec<&'static str>,
    pub sqlite_version: &'static str,
}

impl BuildInfo {
    pub fn current() -> Self {
        Self {
            version: VERSION,
            commit: COMMIT,
            built_at: BUILT_AT,
            target: env!("IRORI_TARGET"),
            features: enabled_features(),
            sqlite_version: rusqlite::version(),
        }
    }
}

impl fmt::Display for BuildInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let features = if self.features.is_empty() {
            "none".to_owned()
        } else {
            self.features.join(", ")
        };
        writeln!(f, "irori {} ({})", self.version, self.commit)?;
        writeln!(f, "built:    {}", self.built_at)?;
        writeln!(f, "target:   {}", self.target)?;
        writeln!(f, "features: {features}")?;
        write!(f, "sqlite:   {}", self.sqlite_version)
    }
}

fn enabled_features() -> Vec<&'static str> {
    [
        ("ui", cfg!(feature = "ui")),
        ("assist", cfg!(feature = "assist")),
    ]
    .into_iter()
    .filter_map(|(name, enabled)| enabled.then_some(name))
    .collect()
}
