//! Home Assistant's MQTT button: `payload_press` is published, and nothing is ever read back.

use irori_types::{ButtonCapabilities, ButtonClass, Capabilities, Service};

use crate::discovery::{EntityTopics, owned_str, plain_command, str_field};
use crate::state::{Publish, text_publish};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonTopics {
    pub command_topic: String,
    pub payload_press: String,
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    // A button's payload is `payload_press`; there's no value to template.
    let (command_topic, _) = plain_command(root, "a button")?;
    Ok((
        Capabilities::Button(ButtonCapabilities {
            device_class: str_field(root, "device_class").and_then(ButtonClass::from_ha),
        }),
        EntityTopics::Button(ButtonTopics {
            command_topic,
            // Home Assistant's default.
            payload_press: owned_str(root, "payload_press", "PRESS"),
        }),
    ))
}

pub(crate) fn encode(topics: &ButtonTopics, service: &Service) -> Result<Vec<Publish>, String> {
    match service {
        Service::ButtonPress => Ok(vec![text_publish(
            &topics.command_topic,
            &topics.payload_press,
        )]),
        service => Err(super::no_service("a button", service)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;
    use irori_types::*;

    #[test]
    fn a_button_sends_its_press_payload_and_listens_to_nothing() {
        let payload = br#"{"unique_id": "0x1234_identify", "name": "Identify",
            "command_topic": "zigbee2mqtt/Lamp/set/identify", "payload_press": "identify",
            "device_class": "identify", "entity_category": "config"}"#;
        let parsed = parse(Component::Button, payload).expect("valid");
        assert_eq!(
            parsed.capabilities,
            Capabilities::Button(ButtonCapabilities {
                device_class: Some(ButtonClass::Identify)
            })
        );
        let unique_id = UniqueId::try_from("0x1234_identify").expect("valid");
        assert!(crate::state::topics_of(&unique_id, &parsed.topics).is_empty());
        let sent = crate::state::encode(&parsed.topics, &irori_types::Service::ButtonPress)
            .expect("a button takes press");
        assert_eq!(sent[0].topic, "zigbee2mqtt/Lamp/set/identify");
        assert_eq!(sent[0].payload, b"identify");
        let plain = br#"{"unique_id": "b", "name": "Go", "command_topic": "x/set"}"#;
        let parsed = parse(Component::Button, plain).expect("valid");
        let sent = crate::state::encode(&parsed.topics, &irori_types::Service::ButtonPress)
            .expect("press");
        assert_eq!(sent[0].payload, b"PRESS", "Home Assistant's default");
    }
}
