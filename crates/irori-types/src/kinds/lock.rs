//! `lock`: a lock, e.g. a front door's smart lock.
//!
//! Unlocking and opening are the actions that let someone in, so pages ask before sending them
//! (`docs/specs/entities.md` §4.4). A lock may need a code for them, which travels with the call
//! and is never kept.

use std::sync::LazyLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockCapabilities {
    /// Can open the door as well as unlock it, e.g. by pulling the latch.
    #[serde(default)]
    pub open: bool,
    /// Needs a code to lock, unlock or open.
    #[serde(default)]
    pub requires_code: bool,
    /// A regular expression the code matches, as the device says. The device checks it; pages
    /// show it to whoever is typing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_format: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LockStatus {
    Locked,
    Unlocked,
    Locking,
    Unlocking,
    /// Stuck: it tried and couldn't finish.
    Jammed,
    Open,
    Opening,
}

impl LockStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Locked => "locked",
            Self::Unlocked => "unlocked",
            Self::Locking => "locking",
            Self::Unlocking => "unlocking",
            Self::Jammed => "jammed",
            Self::Open => "open",
            Self::Opening => "opening",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        super::from_ha(text)
    }
}

/// Every text a lock's primary value can be, for rules to check against.
pub(crate) static LOCK_STATES: LazyLock<Vec<String>> = LazyLock::new(|| {
    [
        LockStatus::Locked,
        LockStatus::Unlocked,
        LockStatus::Locking,
        LockStatus::Unlocking,
        LockStatus::Jammed,
        LockStatus::Open,
        LockStatus::Opening,
    ]
    .iter()
    .map(|state| state.as_str().to_owned())
    .collect()
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockState {
    pub state: LockStatus,
}

/// Data for `lock.lock`, `lock.unlock` and `lock.open`: the code, for a lock that needs one.
///
/// Its `Debug` never shows the code, so a call that ends up in a log or an error message doesn't
/// take the code with it.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockCode {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl std::fmt::Debug for LockCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockCode")
            .field("code", &self.code.as_ref().map(|_| "<hidden>"))
            .finish()
    }
}

/// Whether a lock can be asked to lock, unlock or (`opening`) open with this data. `Err` follows
/// the entity's name.
pub(crate) fn supports(
    caps: &LockCapabilities,
    data: &LockCode,
    opening: bool,
) -> Result<(), String> {
    if opening && !caps.open {
        return Err("can lock and unlock, but not open the door".into());
    }
    if caps.requires_code && data.code.as_deref().is_none_or(str::is_empty) {
        return Err("needs a code".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_that_needs_a_code_is_refused_without_one() {
        let front_door = LockCapabilities {
            open: false,
            requires_code: true,
            code_format: Some("^\\d{4}$".into()),
        };
        assert_eq!(
            supports(&front_door, &LockCode::default(), false),
            Err("needs a code".to_owned())
        );
        let with = LockCode {
            code: Some("1234".into()),
        };
        assert!(supports(&front_door, &with, false).is_ok());
        assert!(supports(&front_door, &with, true).is_err(), "it can't open");
        assert_eq!(LockStatus::parse("jammed"), Some(LockStatus::Jammed));
        assert!(
            !format!("{with:?}").contains("1234"),
            "the code stays out of logs"
        );
    }
}
