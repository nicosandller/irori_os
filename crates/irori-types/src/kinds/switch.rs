//! `switch`: on/off, e.g. a smart plug or relay.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SwitchCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_class: Option<SwitchClass>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SwitchClass {
    Outlet,
    Switch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SwitchState {
    pub on: bool,
}

impl SwitchClass {
    /// The class Home Assistant calls `name` (`outlet`), as protocols that speak its vocabulary
    /// (ESPHome, MQTT discovery) report it.
    pub fn from_ha(name: &str) -> Option<Self> {
        super::from_ha(name)
    }
}
