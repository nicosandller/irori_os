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
/// What a binary sensor's `on` means: Home Assistant's binary sensor device classes, by the
/// same names.
pub enum BinarySensorClass {
    Battery,
    BatteryCharging,
    CarbonMonoxide,
    Cold,
    Connectivity,
    Door,
    GarageDoor,
    Gas,
    GlassBreak,
    Heat,
    Light,
    Lock,
    Moisture,
    Motion,
    Moving,
    Occupancy,
    Opening,
    Plug,
    Power,
    Presence,
    Problem,
    Running,
    Safety,
    Smoke,
    Sound,
    Tamper,
    Update,
    Vibration,
    Window,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BinarySensorState {
    /// `true` means detected/open/wet/etc., depending on the device class.
    pub on: bool,
}

impl BinarySensorClass {
    /// The class Home Assistant calls `name` (`motion`), as protocols that speak its vocabulary
    /// (ESPHome, MQTT discovery) report it.
    pub fn from_ha(name: &str) -> Option<Self> {
        super::from_ha(name)
    }
}
