//! Asking an entity to do something.

use std::fmt;

use irori_types::{EntityId, LightTurnOn, ProtocolId};

/// What someone (a person, later a rule) asks an entity to do. The core turns it into the
/// protocol's service call (`docs/specs/protocols.md` §7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// For a light, optionally with brightness or color. A switch takes no data.
    TurnOn(LightTurnOn),
    TurnOff,
    /// On if it's off (or unknown), off if it's on.
    Toggle,
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
