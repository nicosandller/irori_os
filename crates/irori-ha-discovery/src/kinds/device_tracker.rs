//! Home Assistant's MQTT device tracker: a topic saying where it is, in words, and optionally a
//! topic of JSON attributes that carries its position.
//!
//! Home Assistant reads the words as a zone's name. Irori has only home so far, so a tracker
//! in any other named place is not home.

use irori_types::{Capabilities, State, TrackerCapabilities, TrackerSource, TrackerState};

use crate::discovery::{EntityTopics, owned_str, str_field};
use crate::state::{Message, decode_text};
use crate::template::ValueTemplate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackerTopics {
    pub state_topic: Option<String>,
    pub value_template: ValueTemplate,
    pub payload_home: String,
    pub payload_not_home: String,
    /// What it says when it no longer knows.
    pub payload_reset: String,
    /// Where its position arrives, as JSON: `latitude`, `longitude`, `gps_accuracy`.
    pub json_attributes_topic: Option<String>,
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let topics = TrackerTopics {
        state_topic: str_field(root, "state_topic").map(str::to_owned),
        value_template: ValueTemplate::parse(str_field(root, "value_template")),
        payload_home: owned_str(root, "payload_home", "home"),
        payload_not_home: owned_str(root, "payload_not_home", "not_home"),
        payload_reset: owned_str(root, "payload_reset", "None"),
        json_attributes_topic: str_field(root, "json_attributes_topic").map(str::to_owned),
    };
    if topics.state_topic.is_none() && topics.json_attributes_topic.is_none() {
        return Err(
            "a device tracker needs a `state_topic` or a `json_attributes_topic`".to_owned(),
        );
    }
    Ok((
        Capabilities::DeviceTracker(TrackerCapabilities {
            source_type: str_field(root, "source_type").and_then(TrackerSource::from_ha),
        }),
        EntityTopics::DeviceTracker(Box::new(topics)),
    ))
}

/// Whether it's home comes on one topic and where it is on another, so each message changes
/// its part and keeps the rest of what was last said.
pub(crate) fn decode(
    tracker: &TrackerTopics,
    message: Message,
    previous: Option<&State>,
) -> Option<Result<State, String>> {
    let before = match previous {
        Some(State::DeviceTracker(state)) => Some(state.clone()),
        _ => None,
    };
    if message.on(tracker.state_topic.as_deref()) {
        return Some(
            decode_text(message.payload, &tracker.value_template).and_then(|said| {
                let said = said.trim();
                if said == tracker.payload_reset {
                    return Err("it no longer knows where it is".to_owned());
                }
                // Home, or anywhere else: another zone's name is somewhere that isn't home.
                let home = said == tracker.payload_home;
                Ok(State::DeviceTracker(TrackerState {
                    home,
                    ..before.unwrap_or_else(|| TrackerState::at_home(home))
                }))
            }),
        );
    }
    if message.on(tracker.json_attributes_topic.as_deref()) {
        // A position before it has said whether it's home is nothing to report yet, unless
        // position is all it ever says.
        let before = match (before, &tracker.state_topic) {
            (Some(before), _) => before,
            (None, None) => TrackerState::at_home(false),
            (None, Some(_)) => return None,
        };
        return Some(
            serde_json::from_slice::<serde_json::Value>(message.payload)
                .map_err(|e| format!("its attributes aren't JSON: {e}"))
                .and_then(|said| {
                    let number = |key: &str| said.get(key).and_then(serde_json::Value::as_f64);
                    let state = TrackerState {
                        latitude: number("latitude").or(before.latitude),
                        longitude: number("longitude").or(before.longitude),
                        accuracy: number("gps_accuracy").or(before.accuracy),
                        ..before
                    };
                    state.validate().map_err(|e| e.to_string())?;
                    Ok(State::DeviceTracker(state))
                }),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;

    #[test]
    fn a_phone_is_home_or_not_and_says_where_on_another_topic() {
        let payload = br#"{"unique_id": "phone", "name": "Phone", "source_type": "gps",
            "state_topic": "owntracks/phone/state",
            "json_attributes_topic": "owntracks/phone/attributes"}"#;
        let parsed = parse(Component::DeviceTracker, payload).expect("valid");
        assert_eq!(
            parsed.capabilities,
            Capabilities::DeviceTracker(TrackerCapabilities {
                source_type: Some(TrackerSource::Gps),
            })
        );
        let position = br#"{"latitude": 50.8467, "longitude": 4.3525, "gps_accuracy": 12}"#;
        // Where, before whether: nothing to say yet.
        assert_eq!(
            crate::state::decode(&parsed.topics, "owntracks/phone/attributes", position, None),
            None
        );
        let home = crate::state::decode(&parsed.topics, "owntracks/phone/state", b"home", None)
            .expect("its topic")
            .expect("reads");
        assert_eq!(home, State::DeviceTracker(TrackerState::at_home(true)));
        let placed = crate::state::decode(
            &parsed.topics,
            "owntracks/phone/attributes",
            position,
            Some(&home),
        )
        .expect("its topic")
        .expect("reads");
        // At work is somewhere that isn't home; where it is stays as last said.
        let away = crate::state::decode(
            &parsed.topics,
            "owntracks/phone/state",
            b"work",
            Some(&placed),
        );
        assert_eq!(
            away,
            Some(Ok(State::DeviceTracker(TrackerState {
                home: false,
                latitude: Some(50.8467),
                longitude: Some(4.3525),
                accuracy: Some(12.0),
            })))
        );
        assert!(
            crate::state::decode(
                &parsed.topics,
                "owntracks/phone/state",
                b"None",
                Some(&placed)
            )
            .expect("its topic")
            .is_err()
        );
        assert!(
            crate::state::encode(&parsed.topics, &irori_types::Service::SwitchTurnOn).is_err(),
            "nothing can be asked of it"
        );
    }

    #[test]
    fn a_tracker_needs_somewhere_to_say_anything() {
        let error = parse(
            Component::DeviceTracker,
            br#"{"unique_id": "x", "name": "X"}"#,
        )
        .expect_err("refused");
        assert!(error.contains("state_topic"), "{error}");
    }
}
