//! Asking an entity to do something.

use std::fmt;

use irori_types::{EntityId, IntegrationId, LightTurnOn};

/// What someone (a person, later a rule) asks an entity to do. The core turns it into the
/// integration's service call (`docs/specs/integrations.md` §7.1).
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
    /// The integration that owns the entity isn't running.
    NotRunning(IntegrationId),
    /// The integration says the device can't be reached.
    Unavailable(String),
    /// The integration says the device or service failed.
    Failed(String),
    /// No answer from the integration in time.
    Timeout,
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownEntity(id) => write!(f, "there's no entity `{id}`"),
            Self::NotSupported(why) => f.write_str(why),
            Self::NotRunning(integration) => {
                write!(f, "the `{integration}` integration isn't running")
            }
            Self::Unavailable(why) => write!(f, "device unavailable: {why}"),
            Self::Failed(why) => write!(f, "failed: {why}"),
            Self::Timeout => f.write_str("the integration didn't answer within 10 seconds"),
        }
    }
}

impl std::error::Error for CallError {}
