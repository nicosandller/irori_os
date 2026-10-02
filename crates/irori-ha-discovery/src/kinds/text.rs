//! Home Assistant's MQTT text: a piece of text, published as it is and read back through
//! `value_template`.

use irori_types::{Capabilities, Service, State, TextCapabilities, TextMode, TextState};

use crate::discovery::{EntityTopics, plain_command, str_field};
use crate::state::{Message, Publish, decode_text, text_publish};
use crate::template::{CommandTemplate, ValueTemplate};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextTopics {
    pub state_topic: Option<String>,
    pub command_topic: String,
    pub command_template: CommandTemplate,
    pub value_template: ValueTemplate,
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let (command_topic, command_template) = plain_command(root, "a text")?;
    let length = |key: &str, default: u32| {
        root.get(key)
            .and_then(serde_json::Value::as_u64)
            .map_or(default, |n| u32::try_from(n).unwrap_or(u32::MAX))
    };
    // Home Assistant's own defaults, and its 255-character cap.
    let max_length = length("max", 255).min(255);
    let capabilities = TextCapabilities {
        min_length: length("min", 0).min(max_length),
        max_length,
        pattern: str_field(root, "pattern").map(str::to_owned),
        mode: match str_field(root, "mode") {
            Some("password") => TextMode::Password,
            _ => TextMode::Text,
        },
    };
    Ok((
        Capabilities::Text(capabilities),
        EntityTopics::Text(TextTopics {
            state_topic: str_field(root, "state_topic").map(str::to_owned),
            command_topic,
            command_template,
            value_template: ValueTemplate::parse(str_field(root, "value_template")),
        }),
    ))
}

pub(crate) fn decode(topics: &TextTopics, message: Message) -> Option<Result<State, String>> {
    message.on(topics.state_topic.as_deref()).then(|| {
        decode_text(message.payload, &topics.value_template)
            .map(|value| State::Text(TextState { value }))
    })
}

pub(crate) fn encode(topics: &TextTopics, service: &Service) -> Result<Vec<Publish>, String> {
    match service {
        Service::TextSetValue(data) => Ok(vec![text_publish(
            &topics.command_topic,
            &topics.command_template.render(&data.value),
        )]),
        service => Err(super::no_service("a text", service)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;

    #[test]
    fn parses_a_text_with_home_assistants_defaults() {
        let payload = br#"{"unique_id": "msg", "name": "Message", "command_topic": "panel/msg/set",
            "state_topic": "panel/msg", "max": 1000, "mode": "password"}"#;
        let parsed = parse(Component::Text, payload).expect("valid");
        let Capabilities::Text(caps) = &parsed.capabilities else {
            panic!("a text");
        };
        assert_eq!(
            (caps.min_length, caps.max_length),
            (0, 255),
            "capped at 255"
        );
        assert_eq!(caps.mode, TextMode::Password);
        assert_eq!(
            crate::state::decode(&parsed.topics, "panel/msg", b"hello", None),
            Some(Ok(irori_types::State::Text(irori_types::TextState {
                value: "hello".into()
            })))
        );
        let sent = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::TextSetValue(irori_types::TextSetValue {
                value: "Dinner at 7".into(),
            }),
        )
        .expect("a text takes set_value");
        assert_eq!(sent[0].payload, b"Dinner at 7");
    }
}
