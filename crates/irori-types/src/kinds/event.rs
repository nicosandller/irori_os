//! `event`: something that happens rather than a state something is in, e.g. a remote's button
//! being pressed twice, a doorbell ringing.
//!
//! Its value is the last thing that happened (`double`), but what matters is that it happened:
//! **every report is an occurrence**, so two `double` presses in a row are two changes, not one
//! (`docs/specs/entities.md` §5.3). A report the protocol is only repeating — a retained MQTT
//! message on reconnect — says so with `replayed`, and is not an occurrence.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::sensor::validate_options;
use crate::InvariantError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EventCapabilities {
    /// Everything it can report happening, at least one, e.g. `single`, `double`, `long`.
    #[schemars(length(min = 1, max = 256))]
    pub event_types: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_class: Option<EventClass>,
}

/// What kind of thing it reports happening, when that's one of Home Assistant's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventClass {
    /// A button being pressed: a remote's, a wall switch's.
    Button,
    Doorbell,
    Motion,
}

impl EventClass {
    /// The class Home Assistant calls `name`.
    pub fn from_ha(name: &str) -> Option<Self> {
        super::from_ha(name)
    }
}

impl EventCapabilities {
    /// Deserialization of an entity runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.event_types.is_empty() {
            return Err(InvariantError(
                "an event needs at least one event type".into(),
            ));
        }
        validate_options(&self.event_types)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EventState {
    /// What happened last: one of its `event_types`.
    pub event_type: String,
}

/// Whether a reported event is one it can report.
pub(crate) fn fits(caps: &EventCapabilities, state: &EventState) -> Result<(), String> {
    if caps.event_types.contains(&state.event_type) {
        Ok(())
    } else {
        Err(format!(
            "it reports {}, not {:?}",
            caps.event_types.join(", "),
            state.event_type
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_reports_only_its_types() {
        let remote = EventCapabilities {
            event_types: vec!["single".into(), "double".into(), "long".into()],
            device_class: Some(EventClass::Button),
        };
        assert!(remote.validate().is_ok());
        let happened = |t: &str| EventState {
            event_type: t.into(),
        };
        assert!(fits(&remote, &happened("double")).is_ok());
        assert_eq!(
            fits(&remote, &happened("triple")),
            Err(r#"it reports single, double, long, not "triple""#.to_owned())
        );
    }
}
