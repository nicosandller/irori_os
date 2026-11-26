//! Which system runs a device: Irori, through one of its own extensions, or Home Assistant,
//! through the bridge to it (`extensions/protocols/homeassistant`).
//!
//! A home that has no Home Assistant has only one side, so nothing is labelled there: the
//! label appears only while the Home Assistant extension is installed, and a native home looks
//! as it always did.

use crate::api::Home;

/// The Home Assistant extension's id.
pub const HOME_ASSISTANT: &str = "homeassistant";

/// Who runs a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// One of Irori's own extensions talks to it.
    Irori,
    /// It comes through Home Assistant, which keeps running it.
    HomeAssistant,
}

impl Side {
    pub fn words(self) -> &'static str {
        match self {
            Self::Irori => "Irori",
            Self::HomeAssistant => "Home Assistant",
        }
    }
}

/// Whether the Home Assistant extension is installed here: the only time sides are worth saying.
pub fn bridged(home: &Home) -> bool {
    home.extensions
        .keys()
        .any(|extension| extension.as_str() == HOME_ASSISTANT)
}

/// The side of what came through `protocol`; `None` in a home without Home Assistant.
pub fn of(home: &Home, protocol: &str) -> Option<Side> {
    bridged(home).then(|| {
        if protocol == HOME_ASSISTANT {
            Side::HomeAssistant
        } else {
            Side::Irori
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home_with(extensions: &[&str]) -> Home {
        let mut home = Home::default();
        for id in extensions {
            home.extensions.insert(
                id.parse().expect("an extension id"),
                serde_json::from_value(serde_json::json!({ "state": "running" }))
                    .expect("an extension"),
            );
        }
        home
    }

    #[test]
    fn a_home_without_home_assistant_has_no_sides() {
        let home = home_with(&["zigbee", "esphome"]);
        assert_eq!(of(&home, "zigbee"), None);
        assert_eq!(of(&home, "homeassistant"), None);
        assert_eq!(of(&Home::default(), "zigbee"), None);
    }

    #[test]
    fn with_home_assistant_each_device_has_a_side() {
        let home = home_with(&["zigbee", "homeassistant"]);
        assert_eq!(of(&home, "zigbee"), Some(Side::Irori));
        assert_eq!(of(&home, "homeassistant"), Some(Side::HomeAssistant));
    }
}
