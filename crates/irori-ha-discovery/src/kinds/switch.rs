//! Home Assistant's MQTT switch: a plain on and off.

use irori_types::{Capabilities, Service, State, SwitchCapabilities, SwitchClass, SwitchState};

use crate::discovery::{EntityTopics, owned_str, str_field, word_field};
use crate::state::{Message, Publish, on_off_template, said_on_off, text_publish};
use crate::template::ValueTemplate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchTopics {
    pub state_topic: Option<String>,
    pub command_topic: String,
    pub payload_on: String,
    pub payload_off: String,
    pub value_template: ValueTemplate,
    /// What it reports when on and off: the payloads that command it, unless it says otherwise.
    pub state_on: String,
    pub state_off: String,
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let device_class = str_field(root, "device_class").and_then(SwitchClass::from_ha);
    let command_topic = str_field(root, "command_topic")
        .ok_or("a switch needs a `command_topic`")?
        .to_owned();
    let payload_on = owned_str(root, "payload_on", "ON");
    let payload_off = owned_str(root, "payload_off", "OFF");
    Ok((
        Capabilities::Switch(SwitchCapabilities { device_class }),
        EntityTopics::Switch(SwitchTopics {
            state_topic: str_field(root, "state_topic").map(str::to_owned),
            command_topic,
            value_template: on_off_template(root),
            state_on: word_field(root, "state_on", &payload_on),
            state_off: word_field(root, "state_off", &payload_off),
            payload_on,
            payload_off,
        }),
    ))
}

pub(crate) fn decode(topics: &SwitchTopics, message: Message) -> Option<Result<State, String>> {
    if !message.on(topics.state_topic.as_deref()) {
        return None;
    }
    let on = said_on_off(
        &message,
        &topics.value_template,
        &topics.state_on,
        &topics.state_off,
    )?;
    Some(on.map(|on| State::Switch(SwitchState { on })))
}

pub(crate) fn encode(topics: &SwitchTopics, service: &Service) -> Result<Vec<Publish>, String> {
    let payload = match service {
        Service::SwitchTurnOn => &topics.payload_on,
        Service::SwitchTurnOff => &topics.payload_off,
        service => return Err(super::no_service("a switch", service)),
    };
    Ok(vec![text_publish(&topics.command_topic, payload)])
}

#[cfg(test)]
mod tests {
    use crate::discovery::parse;
    use crate::state::{decode, encode};
    use crate::topic::Component;
    use irori_types::*;

    fn on(config: &str, topic: &str, payload: &str) -> Option<bool> {
        let parsed = parse(Component::Switch, config.as_bytes()).expect("parses");
        match decode(&parsed.topics, topic, payload.as_bytes(), None)?.expect("reads") {
            State::Switch(state) => Some(state.on),
            other => panic!("not a switch: {other:?}"),
        }
    }

    #[test]
    fn a_zigbee_plug_reads_its_state_out_of_the_devices_message() {
        let config = r#"{
            "unique_id": "0x1_switch_zigbee2mqtt", "name": "Plug",
            "state_topic": "zigbee2mqtt/plug", "command_topic": "zigbee2mqtt/plug/set",
            "value_template": "{{ value_json.state }}",
            "payload_on": "ON", "payload_off": "OFF"
        }"#;
        let topic = "zigbee2mqtt/plug";
        assert_eq!(on(config, topic, r#"{"state":"ON","power":3}"#), Some(true));
        assert_eq!(
            on(config, topic, r#"{"state":"OFF","power":0}"#),
            Some(false)
        );
        assert_eq!(on(config, topic, r#"{"power":0}"#), None);
    }

    #[test]
    fn a_zigbee_setting_that_is_true_or_false_reads() {
        // How Zigbee2MQTT writes a switch whose device says `true` and `false`.
        let config = r#"{
            "unique_id": "0x1_child_lock", "name": "Child lock",
            "state_topic": "zigbee2mqtt/plug", "command_topic": "zigbee2mqtt/plug/set/child_lock",
            "value_template": "{% if value_json[\"child_lock\"] %}true{% else %}false{% endif %}",
            "payload_on": "true", "payload_off": "false"
        }"#;
        let topic = "zigbee2mqtt/plug";
        assert_eq!(on(config, topic, r#"{"child_lock":true}"#), Some(true));
        assert_eq!(on(config, topic, r#"{"child_lock":false}"#), Some(false));
    }

    #[test]
    fn the_bridges_permit_join_reads_by_its_own_words_and_commands_by_its_payloads() {
        let config = r#"{
            "unique_id": "bridge_permit_join", "name": "Permit join",
            "state_topic": "zigbee2mqtt/bridge/info",
            "value_template": "{{ value_json.permit_join | lower }}",
            "command_topic": "zigbee2mqtt/bridge/request/permit_join",
            "state_on": "true", "state_off": "false",
            "payload_on": "{\"time\": 254}", "payload_off": "{\"time\": 0}"
        }"#;
        let topic = "zigbee2mqtt/bridge/info";
        assert_eq!(on(config, topic, r#"{"permit_join":true}"#), Some(true));
        assert_eq!(on(config, topic, r#"{"permit_join":false}"#), Some(false));
        let parsed = parse(Component::Switch, config.as_bytes()).expect("parses");
        let sent = encode(&parsed.topics, &Service::SwitchTurnOn).expect("encodes");
        assert_eq!(sent[0].payload, br#"{"time": 254}"#);
    }

    #[test]
    fn a_plain_switch_without_a_template_still_reads() {
        let config = r#"{"unique_id": "t1", "name": "Relay",
            "state_topic": "stat/POWER", "command_topic": "cmnd/POWER"}"#;
        assert_eq!(on(config, "stat/POWER", "ON"), Some(true));
        assert_eq!(on(config, "stat/POWER", "OFF"), Some(false));
    }
}
