//! What this binary is and what was compiled into it.

use std::fmt;

use serde::Serialize;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Serialize)]
pub struct BuildInfo {
    pub version: &'static str,
    pub target: &'static str,
    pub features: Vec<&'static str>,
    pub sqlite_version: &'static str,
}

impl BuildInfo {
    pub fn current() -> Self {
        Self {
            version: VERSION,
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
        writeln!(f, "irori {}", self.version)?;
        writeln!(f, "target:   {}", self.target)?;
        writeln!(f, "features: {features}")?;
        write!(f, "sqlite:   {}", self.sqlite_version)
    }
}

fn enabled_features() -> Vec<&'static str> {
    [
        ("int-mqtt", cfg!(feature = "int-mqtt")),
        ("int-demo", cfg!(feature = "int-demo")),
        ("int-esphome", cfg!(feature = "int-esphome")),
        ("ui", cfg!(feature = "ui")),
        ("assist", cfg!(feature = "assist")),
    ]
    .into_iter()
    .filter_map(|(name, enabled)| enabled.then_some(name))
    .collect()
}
