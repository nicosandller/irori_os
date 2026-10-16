//! Home Assistant's MQTT scene: `payload_on` is published to set it, and nothing is ever read
//! back.

use irori_types::{Capabilities, SceneCapabilities, Service};

use crate::discovery::{EntityTopics, owned_str, plain_command};
use crate::state::{Publish, text_publish};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneTopics {
    pub command_topic: String,
    pub payload_on: String,
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    // A scene's payload is `payload_on`; there's no value to template.
    let (command_topic, _) = plain_command(root, "a scene")?;
    Ok((
        Capabilities::Scene(SceneCapabilities {}),
        EntityTopics::Scene(SceneTopics {
            command_topic,
            // Home Assistant's default.
            payload_on: owned_str(root, "payload_on", "ON"),
        }),
    ))
}

pub(crate) fn encode(topics: &SceneTopics, service: &Service) -> Result<Vec<Publish>, String> {
    match service {
        Service::SceneTurnOn => Ok(vec![text_publish(
            &topics.command_topic,
            &topics.payload_on,
        )]),
        service => Err(super::no_service("a scene", service)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;

    #[test]
    fn a_scene_sends_its_payload_and_listens_to_nothing() {
        let payload = br#"{"unique_id": "group_1_scene_2", "name": "Evening",
            "command_topic": "zigbee2mqtt/Living room/set",
            "payload_on": "{\"scene_recall\": 2}"}"#;
        let parsed = parse(Component::Scene, payload).expect("valid");
        assert_eq!(
            parsed.capabilities,
            Capabilities::Scene(SceneCapabilities {})
        );
        assert_eq!(
            crate::state::decode(&parsed.topics, "zigbee2mqtt/Living room/set", b"ON", None),
            None,
            "nothing it says is read"
        );
        let sent = crate::state::encode(&parsed.topics, &Service::SceneTurnOn).expect("sets");
        assert_eq!(sent[0].topic, "zigbee2mqtt/Living room/set");
        assert_eq!(
            String::from_utf8_lossy(&sent[0].payload),
            r#"{"scene_recall": 2}"#
        );
        assert!(crate::state::encode(&parsed.topics, &Service::ButtonPress).is_err());
    }
}
