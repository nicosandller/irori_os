//! The bridge's messages. Each travels as a JSON string with `irori: 1`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The bridge's version, on every message.
pub const VERSION: u8 = 1;

/// Page → shell: do something and answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub irori: u8,
    pub id: u64,
    pub op: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

/// Shell → page: the answer to a [`Request`] with the same `id`, or an event (`event` set, no
/// `id`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub irori: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// For an event: `theme` or `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
}

impl Reply {
    pub fn answer(id: u64, result: Result<serde_json::Value, String>) -> Self {
        let (value, error) = match result {
            Ok(value) => (Some(value), None),
            Err(error) => (None, Some(error)),
        };
        Self {
            irori: VERSION,
            id: Some(id),
            value,
            error,
            event: None,
        }
    }

    pub fn event(name: &str, value: serde_json::Value) -> Self {
        Self {
            irori: VERSION,
            id: None,
            value: Some(value),
            error: None,
            event: Some(name.to_owned()),
        }
    }
}

/// How the shell looks right now, so the page can look the same.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    pub dark: bool,
    /// The system asks for less motion, or Settings turned motion off.
    pub reduced_motion: bool,
    /// The shell's CSS custom properties, `--bg` → `#faf7f4`.
    pub tokens: BTreeMap<String, String>,
}

/// What `hello` answers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub theme: Theme,
    /// The part of the address after `/apps/<extension>/`.
    pub path: String,
}

/// The tokens the shell hands over: its palette, type and motion.
pub const TOKENS: &[&str] = &[
    "--ink",
    "--paper",
    "--ember",
    "--muted",
    "--fg",
    "--bg",
    "--line",
    "--card",
    "--sunk",
    "--warn",
    "--error",
    "--sans",
    "--mono",
    "--dur-fast",
    "--dur-base",
    "--dur-layout",
    "--dur-expressive",
    "--ease-out",
    "--ease-spring",
    "--ease-emphasized",
    "--ease-bounce",
];

/// The scope each op needs, if any (`docs/specs/automations.md` §B4).
pub fn scope_for(op: &str) -> Option<&'static str> {
    match op {
        "registry" => Some("registry:read"),
        "states" => Some("states:read"),
        "history" => Some("history:read"),
        _ => None,
    }
}
