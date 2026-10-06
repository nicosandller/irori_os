//! Home Assistant's MQTT binary sensor: on or off, as it words them.

use irori_types::{
    BinarySensorCapabilities, BinarySensorClass, BinarySensorState, Capabilities, State,
};

use crate::discovery::{EntityTopics, str_field, word_field};
use crate::state::{Message, on_off_template, said_on_off};
use crate::template::ValueTemplate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinarySensorTopics {
    pub state_topic: String,
    pub value_template: ValueTemplate,
    pub payload_on: String,
    pub payload_off: String,
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let state_topic = str_field(root, "state_topic")
        .ok_or("a binary_sensor needs a `state_topic`")?
        .to_owned();
    Ok((
        Capabilities::BinarySensor(BinarySensorCapabilities {
            device_class: str_field(root, "device_class").and_then(BinarySensorClass::from_ha),
        }),
        EntityTopics::BinarySensor(BinarySensorTopics {
            state_topic,
            value_template: on_off_template(root),
            payload_on: word_field(root, "payload_on", "ON"),
            payload_off: word_field(root, "payload_off", "OFF"),
        }),
    ))
}

pub(crate) fn decode(
    topics: &BinarySensorTopics,
    message: Message,
) -> Option<Result<State, String>> {
    if !message.on(Some(&topics.state_topic)) {
        return None;
    }
    let on = said_on_off(
        &message,
        &topics.value_template,
        &topics.payload_on,
        &topics.payload_off,
    )?;
    Some(on.map(|on| State::BinarySensor(BinarySensorState { on })))
}

#[cfg(test)]
mod tests {
    use crate::discovery::parse;
    use crate::state::decode;
    use crate::topic::Component;
    use irori_types::*;

    fn read(config: &str, topic: &str, payload: &str) -> Option<Result<State, String>> {
        let parsed = parse(Component::BinarySensor, config.as_bytes()).expect("parses");
        decode(&parsed.topics, topic, payload.as_bytes(), None)
    }

    fn on(config: &str, topic: &str, payload: &str) -> Option<bool> {
        match read(config, topic, payload)?.expect("reads") {
            State::BinarySensor(state) => Some(state.on),
            other => panic!("not a binary sensor: {other:?}"),
        }
    }

    /// Zigbee2MQTT's door contact (a Moes ZSS-S01-GWM-C-MS): `contact` is true while the magnet
    /// is there, which is the door *closed*, so its "on" is `false`.
    const Z2M_CONTACT: &str = r#"{
        "unique_id": "0xa4c1383528272d32_contact_zigbee2mqtt",
        "name": "Open",
        "device_class": "door",
        "state_topic": "zigbee2mqtt/0xa4c1383528272d32",
        "value_template": "{{ value_json[\"contact\"] }}",
        "payload_on": false,
        "payload_off": true
    }"#;

    #[test]
    fn a_zigbee_door_is_open_when_its_contact_is_false() {
        let topic = "zigbee2mqtt/0xa4c1383528272d32";
        let body = |contact: &str| {
            format!(r#"{{"battery":100,"contact":{contact},"tamper":false,"voltage":2900}}"#)
        };
        assert_eq!(on(Z2M_CONTACT, topic, &body("false")), Some(true), "open");
        assert_eq!(on(Z2M_CONTACT, topic, &body("true")), Some(false), "shut");
    }

    #[test]
    fn a_sensor_not_heard_from_yet_says_nothing() {
        let topic = "zigbee2mqtt/0xa4c1383528272d32";
        // What Zigbee2MQTT sends right after pairing, and a message without the key at all.
        assert!(read(Z2M_CONTACT, topic, r#"{"battery":100,"contact":null}"#).is_none());
        assert!(read(Z2M_CONTACT, topic, r#"{"battery":100}"#).is_none());
    }

    #[test]
    fn the_bridges_own_sensors_read() {
        let connection = r#"{
            "unique_id": "bridge_connection_state", "name": "Connection state",
            "state_topic": "zigbee2mqtt/bridge/state",
            "value_template": "{{ value_json.state }}",
            "payload_on": "online", "payload_off": "offline"
        }"#;
        let topic = "zigbee2mqtt/bridge/state";
        assert_eq!(on(connection, topic, r#"{"state":"online"}"#), Some(true));
        assert_eq!(on(connection, topic, r#"{"state":"offline"}"#), Some(false));

        let restart = r#"{
            "unique_id": "bridge_restart_required", "name": "Restart required",
            "state_topic": "zigbee2mqtt/bridge/info",
            "value_template": "{{ value_json.restart_required }}",
            "payload_on": true, "payload_off": false
        }"#;
        let topic = "zigbee2mqtt/bridge/info";
        assert_eq!(
            on(restart, topic, r#"{"restart_required":true}"#),
            Some(true)
        );
        assert_eq!(
            on(restart, topic, r#"{"restart_required":false}"#),
            Some(false)
        );
    }

    #[test]
    fn a_plain_sensor_without_a_template_still_reads() {
        // Tasmota: the payload is the word itself.
        let config = r#"{"unique_id": "t1", "name": "Door", "state_topic": "tele/door"}"#;
        assert_eq!(on(config, "tele/door", "ON"), Some(true));
        assert_eq!(on(config, "tele/door", "OFF"), Some(false));
        assert!(
            read(config, "tele/door", "maybe")
                .expect("is for it")
                .is_err()
        );
        assert!(read(config, "tele/other", "ON").is_none());
    }
}
