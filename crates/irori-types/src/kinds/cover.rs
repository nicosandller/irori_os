//! `cover`: something that opens and closes, e.g. a blind, a curtain, a garage door, a gate.

use std::sync::LazyLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::InvariantError;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoverCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_class: Option<CoverClass>,
    /// Can be sent to a position between open and closed, and says where it is.
    #[serde(default)]
    pub position: bool,
    /// Has slats that tilt, and says how far.
    #[serde(default)]
    pub tilt: bool,
    /// Can be stopped while it's moving.
    #[serde(default)]
    pub stop: bool,
}

/// What it covers, when that's one of Home Assistant's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoverClass {
    Awning,
    Blind,
    Curtain,
    Damper,
    Door,
    Garage,
    Gate,
    Shade,
    Shutter,
    Window,
}

impl CoverClass {
    /// The class Home Assistant calls `name`.
    pub fn from_ha(name: &str) -> Option<Self> {
        super::from_ha(name)
    }
}

/// Where something that opens and closes is: a cover's or a valve's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpenState {
    Open,
    Opening,
    Closed,
    Closing,
}

impl OpenState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Opening => "opening",
            Self::Closed => "closed",
            Self::Closing => "closing",
        }
    }

    /// Whether it is open, or on its way there: what a toggle closes.
    pub fn is_open_or_opening(self) -> bool {
        matches!(self, Self::Open | Self::Opening)
    }

    /// Read back from [`OpenState::as_str`], e.g. a remembered command.
    pub fn parse(text: &str) -> Option<Self> {
        super::from_ha(text)
    }
}

/// Every text a cover's or valve's primary value can be, for rules to check against.
pub(crate) static OPEN_STATES: LazyLock<Vec<String>> = LazyLock::new(|| {
    [
        OpenState::Open,
        OpenState::Opening,
        OpenState::Closed,
        OpenState::Closing,
    ]
    .iter()
    .map(|state| state.as_str().to_owned())
    .collect()
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoverState {
    pub state: OpenState,
    /// 0 (closed) to 100 (open), when it can say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(max = 100))]
    pub position: Option<u8>,
    /// 0 (closed) to 100 (open), for one whose slats tilt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(max = 100))]
    pub tilt: Option<u8>,
}

impl CoverState {
    /// Deserialization runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        for (what, value) in [("position", self.position), ("tilt", self.tilt)] {
            if let Some(value) = value
                && value > 100
            {
                return Err(InvariantError(format!(
                    "a cover's {what} is 0-100, not {value}"
                )));
            }
        }
        Ok(())
    }
}

/// Data for `cover.set_position` and `valve.set_position`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetPosition {
    /// 0 (closed) to 100 (open).
    #[schemars(range(max = 100))]
    pub position: u8,
}

/// Data for `cover.set_tilt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTilt {
    /// 0 (closed) to 100 (open).
    #[schemars(range(max = 100))]
    pub tilt: u8,
}

fn percent(what: &str, value: u8) -> Result<(), String> {
    if value <= 100 {
        Ok(())
    } else {
        Err(format!("takes a {what} from 0 to 100, not {value}"))
    }
}

/// Whether a cover can be sent to a position. `Err` follows the entity's name.
pub(crate) fn supports_position(
    caps: &CoverCapabilities,
    data: &SetPosition,
) -> Result<(), String> {
    if !caps.position {
        return Err("can only open and close, not go to a position".into());
    }
    percent("position", data.position)
}

/// Whether a cover's slats can be tilted.
pub(crate) fn supports_tilt(caps: &CoverCapabilities, data: &SetTilt) -> Result<(), String> {
    if !caps.tilt {
        return Err("has nothing to tilt".into());
    }
    percent("tilt", data.tilt)
}

/// Whether a cover can be stopped.
pub(crate) fn supports_stop(caps: &CoverCapabilities) -> Result<(), String> {
    if caps.stop {
        Ok(())
    } else {
        Err("can't be stopped while it moves".into())
    }
}

/// Whether a reported state is one this cover can be in.
pub(crate) fn fits(caps: &CoverCapabilities, state: &CoverState) -> Result<(), String> {
    if state.position.is_some() && !caps.position {
        return Err("it reports a position, but said it can't go to one".into());
    }
    if state.tilt.is_some() && !caps.tilt {
        return Err("it reports a tilt, but said it has nothing to tilt".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cover_does_only_what_it_says_it_can() {
        let blind = CoverCapabilities {
            device_class: Some(CoverClass::Blind),
            position: true,
            tilt: false,
            stop: true,
        };
        assert!(supports_position(&blind, &SetPosition { position: 40 }).is_ok());
        assert_eq!(
            supports_tilt(&blind, &SetTilt { tilt: 20 }),
            Err("has nothing to tilt".to_owned())
        );
        let garage = CoverCapabilities::default();
        assert!(supports_position(&garage, &SetPosition { position: 40 }).is_err());
        assert!(supports_stop(&garage).is_err());
        assert!(
            fits(
                &garage,
                &CoverState {
                    state: OpenState::Open,
                    position: Some(100),
                    tilt: None
                }
            )
            .is_err()
        );
    }

    #[test]
    fn a_position_above_100_is_refused() {
        let state: Result<CoverState, _> =
            serde_json::from_str(r#"{"state": "open", "position": 140}"#);
        // u8 takes 140, so the range is checked by `validate`, which a `State` runs.
        assert!(state.is_ok_and(|s| s.validate().is_err()));
        assert_eq!(OpenState::parse("closing"), Some(OpenState::Closing));
        assert_eq!(OPEN_STATES.len(), 4);
    }
}
