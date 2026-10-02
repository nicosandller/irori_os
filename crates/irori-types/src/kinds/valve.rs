//! `valve`: something that lets water or gas through, or doesn't, e.g. a main water shut-off or
//! an irrigation zone. Opens and closes like a cover, sometimes part of the way.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::cover::{OpenState, SetPosition};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValveCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_class: Option<ValveClass>,
    /// Can be opened part of the way, and says how far.
    #[serde(default)]
    pub position: bool,
    /// Can be stopped while it moves.
    #[serde(default)]
    pub stop: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ValveClass {
    Water,
    Gas,
}

impl ValveClass {
    /// The class Home Assistant calls `name`.
    pub fn from_ha(name: &str) -> Option<Self> {
        super::from_ha(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValveState {
    pub state: OpenState,
    /// 0 (closed) to 100 (open), when it can say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(max = 100))]
    pub position: Option<u8>,
}

impl ValveState {
    /// Deserialization runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), crate::InvariantError> {
        match self.position {
            Some(position) if position > 100 => Err(crate::InvariantError(format!(
                "a valve's position is 0-100, not {position}"
            ))),
            _ => Ok(()),
        }
    }
}

/// Whether a valve can be sent to a position. `Err` follows the entity's name.
pub(crate) fn supports_position(
    caps: &ValveCapabilities,
    data: &SetPosition,
) -> Result<(), String> {
    if !caps.position {
        return Err("can only open and close, not open part of the way".into());
    }
    if data.position > 100 {
        return Err(format!(
            "takes a position from 0 to 100, not {}",
            data.position
        ));
    }
    Ok(())
}

pub(crate) fn supports_stop(caps: &ValveCapabilities) -> Result<(), String> {
    if caps.stop {
        Ok(())
    } else {
        Err("can't be stopped while it moves".into())
    }
}

/// Whether a reported state is one this valve can be in.
pub(crate) fn fits(caps: &ValveCapabilities, state: &ValveState) -> Result<(), String> {
    if state.position.is_some() && !caps.position {
        return Err("it reports a position, but said it can't open part of the way".into());
    }
    Ok(())
}
