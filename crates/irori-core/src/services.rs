//! Asking an entity to do something.

use std::fmt;

use irori_types::{EntityId, ProtocolId};

/// What someone (a person, a rule) asks an entity to do: one of its kind's actions with that
/// action's data, or `toggle`. The core turns it into the protocol's service call
/// (`docs/specs/protocols.md` §7.1), checking both against the entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// The part of the service name after the dot (`turn_on`), or `toggle`.
    pub action: String,
    pub data: serde_json::Map<String, serde_json::Value>,
}

impl Command {
    /// The action the core resolves itself, from the entity's current value.
    pub const TOGGLE: &str = "toggle";

    /// An action without data.
    pub fn new(action: impl Into<String>) -> Self {
        Self {
            action: action.into(),
            data: serde_json::Map::new(),
        }
    }

    /// An action with data, e.g. `turn_on` with a light's `LightTurnOn`. Data that isn't a JSON
    /// object counts as none.
    pub fn with(action: impl Into<String>, data: &impl serde::Serialize) -> Self {
        let data = match serde_json::to_value(data) {
            Ok(serde_json::Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        };
        Self {
            action: action.into(),
            data,
        }
    }

    pub fn turn_on() -> Self {
        Self::new("turn_on")
    }

    pub fn turn_off() -> Self {
        Self::new("turn_off")
    }

    pub fn toggle() -> Self {
        Self::new(Self::TOGGLE)
    }
}

/// Why a command didn't happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    UnknownEntity(EntityId),
    /// The entity can't do that, e.g. brightness on a light that doesn't dim.
    NotSupported(String),
    /// The protocol that owns the entity isn't running.
    NotRunning(ProtocolId),
    /// The protocol says the device can't be reached.
    Unavailable(String),
    /// The protocol says the device or service failed.
    Failed(String),
    /// The whole call, queueing included, didn't finish in time.
    Timeout,
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownEntity(id) => write!(f, "there's no entity `{id}`"),
            Self::NotSupported(why) => f.write_str(why),
            Self::NotRunning(protocol) => {
                write!(f, "the `{protocol}` protocol isn't running")
            }
            Self::Unavailable(why) => write!(f, "device unavailable: {why}"),
            Self::Failed(why) => write!(f, "failed: {why}"),
            Self::Timeout => {
                f.write_str("the call didn't finish within 10 seconds (waiting for other calls on the same entity, or for the protocol to answer)")
            }
        }
    }
}

impl std::error::Error for CallError {}
