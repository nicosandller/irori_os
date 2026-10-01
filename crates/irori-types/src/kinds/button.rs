//! `button`: something to press, e.g. a device's restart or identify button, a gate opener.
//!
//! It has no value: pressing it is an action, not a state it's in, so its `EntityState.state` is
//! always `null` and there's nothing for an automation to compare or wait for.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ButtonCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_class: Option<ButtonClass>,
}

/// What pressing it does, when that's one of Home Assistant's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ButtonClass {
    /// Makes the device show itself: a blink, a beep.
    Identify,
    Restart,
    /// Installs an update the device has.
    Update,
}

impl ButtonClass {
    /// The class Home Assistant calls `name`.
    pub fn from_ha(name: &str) -> Option<Self> {
        super::from_ha(name)
    }
}
