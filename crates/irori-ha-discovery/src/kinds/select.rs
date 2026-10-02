//! Home Assistant's MQTT select: one choice out of a list. The option itself is published to
//! `command_topic`, and read back through `value_template`.

use irori_types::{Capabilities, SelectCapabilities, SelectState, Service, State};

use crate::discovery::{EntityTopics, plain_command, str_field};
use crate::state::{Message, Publish, decode_text, text_publish};
use crate::template::{CommandTemplate, ValueTemplate};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectTopics {
    pub state_topic: Option<String>,
    pub command_topic: String,
    pub command_template: CommandTemplate,
    pub value_template: ValueTemplate,
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let (command_topic, command_template) = plain_command(root, "a select")?;
    let options = root
        .get("options")
        .and_then(serde_json::Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|o| o.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let capabilities = SelectCapabilities { options };
    capabilities.validate().map_err(|e| e.to_string())?;
    Ok((
        Capabilities::Select(capabilities),
        EntityTopics::Select(SelectTopics {
            state_topic: str_field(root, "state_topic").map(str::to_owned),
            command_topic,
            command_template,
            value_template: ValueTemplate::parse(str_field(root, "value_template")),
        }),
    ))
}

pub(crate) fn decode(topics: &SelectTopics, message: Message) -> Option<Result<State, String>> {
    message.on(topics.state_topic.as_deref()).then(|| {
        decode_text(message.payload, &topics.value_template)
            .map(|option| State::Select(SelectState { option }))
    })
}

pub(crate) fn encode(topics: &SelectTopics, service: &Service) -> Result<Vec<Publish>, String> {
    match service {
        Service::SelectSelectOption(data) => Ok(vec![text_publish(
            &topics.command_topic,
            &topics.command_template.render(&data.option),
        )]),
        service => Err(super::no_service("a select", service)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;

    #[test]
    fn parses_a_z2m_select_and_sends_it_the_option() {
        // Zigbee2MQTT's `case "enum"` with set access.
        let payload = br#"{
            "unique_id": "0x0211000000000002_sensitivity_zigbee2mqtt",
            "name": "Sensitivity",
            "device": {"identifiers": ["zigbee2mqtt_0x0211000000000002"], "name": "Presence sensor"},
            "state_topic": "zigbee2mqtt/Presence sensor",
            "value_template": "{{ value_json[\"sensitivity\"] }}",
            "command_topic": "zigbee2mqtt/Presence sensor/set/sensitivity",
            "options": ["low", "medium", "high"],
            "entity_category": "config"
        }"#;
        let parsed = parse(Component::Select, payload).expect("valid");
        assert_eq!(
            parsed.capabilities,
            Capabilities::Select(SelectCapabilities {
                options: vec!["low".into(), "medium".into(), "high".into()]
            })
        );
        let state = crate::state::decode(
            &parsed.topics,
            "zigbee2mqtt/Presence sensor",
            br#"{"occupancy": false, "sensitivity": "medium"}"#,
            None,
        );
        assert_eq!(
            state,
            Some(Ok(irori_types::State::Select(irori_types::SelectState {
                option: "medium".into()
            })))
        );
        let sent = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::SelectSelectOption(irori_types::SelectOption {
                option: "high".into(),
            }),
        )
        .expect("a select takes select_option");
        assert_eq!(sent[0].topic, "zigbee2mqtt/Presence sensor/set/sensitivity");
        assert_eq!(sent[0].payload, b"high");

        let no_options = br#"{"unique_id": "s", "name": "Mode", "command_topic": "x/set"}"#;
        assert!(parse(Component::Select, no_options).is_err());
    }
}
