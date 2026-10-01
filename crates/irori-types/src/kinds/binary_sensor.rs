//! `binary_sensor`: a two-state reading, e.g. motion or a door contact.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BinarySensorCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_class: Option<BinarySensorClass>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BinarySensorClass {
    Motion,
    Occupancy,
    Door,
    Window,
    Moisture,
    Smoke,
    Gas,
    Vibration,
    Plug,
    Connectivity,
    Problem,
    Battery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BinarySensorState {
    /// `true` means detected/open/wet/etc., depending on the device class.
    pub on: bool,
}
