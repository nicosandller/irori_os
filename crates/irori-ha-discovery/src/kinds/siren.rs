//! Home Assistant's MQTT siren: `ON` and `OFF`, or a JSON body when it's told a tone, a volume
//! or a duration.

use irori_types::{Capabilities, Service, SirenCapabilities, SirenState, SirenTurnOn, State};

use crate::discovery::{EntityTopics, owned_str, plain_command, str_field};
use crate::state::{Message, Publish, decode_text, json_publish, text_publish};
use crate::template::{CommandTemplate, ValueTemplate};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SirenTopics {
    pub command_topic: String,
    pub command_template: CommandTemplate,
    pub payload_on: String,
    pub payload_off: String,
    pub state_topic: Option<String>,
    pub value_template: ValueTemplate,
    pub state_on: String,
    pub state_off: String,
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let (command_topic, command_template) = plain_command(root, "a siren")?;
    let payload_on = owned_str(root, "payload_on", "ON");
    let payload_off = owned_str(root, "payload_off", "OFF");
    let supported = |key: &str| {
        root.get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true)
    };
    let capabilities = SirenCapabilities {
        tones: root
            .get("available_tones")
            .and_then(serde_json::Value::as_array)
            .map(|tones| {
                tones
                    .iter()
                    .filter_map(|t| t.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        // Home Assistant's defaults: both, unless it says not.
        volume: supported("support_volume_set"),
        duration: supported("support_duration"),
    };
    capabilities.validate().map_err(|e| e.to_string())?;
    Ok((
        Capabilities::Siren(capabilities),
        EntityTopics::Siren(SirenTopics {
            state_on: owned_str(root, "state_on", &payload_on),
            state_off: owned_str(root, "state_off", &payload_off),
            command_topic,
            command_template,
            payload_on,
            payload_off,
            state_topic: str_field(root, "state_topic").map(str::to_owned),
            value_template: ValueTemplate::parse(str_field(root, "state_value_template")),
        }),
    ))
}

pub(crate) fn decode(topics: &SirenTopics, message: Message) -> Option<Result<State, String>> {
    message.on(topics.state_topic.as_deref()).then(|| {
        decode_text(message.payload, &topics.value_template).and_then(|said| match said.trim() {
            said if said == topics.state_on => Ok(State::Siren(SirenState { on: true })),
            said if said == topics.state_off => Ok(State::Siren(SirenState { on: false })),
            said => Err(format!("{said:?} isn't on or off for this siren")),
        })
    })
}

pub(crate) fn encode(topics: &SirenTopics, service: &Service) -> Result<Vec<Publish>, String> {
    let plain = |payload: &str| {
        vec![text_publish(
            &topics.command_topic,
            &topics.command_template.render(payload),
        )]
    };
    match service {
        Service::SirenTurnOff => Ok(plain(&topics.payload_off)),
        // Plain `ON`; told how, Home Assistant's JSON body with the state and what it was told.
        Service::SirenTurnOn(data) if *data == SirenTurnOn::default() => {
            Ok(plain(&topics.payload_on))
        }
        Service::SirenTurnOn(data) => {
            let mut body = serde_json::json!({ "state": topics.payload_on });
            if let Some(tone) = &data.tone {
                body["tone"] = tone.clone().into();
            }
            if let Some(volume) = data.volume_level {
                body["volume_level"] = volume.into();
            }
            if let Some(duration) = data.duration {
                body["duration"] = duration.into();
            }
            Ok(vec![json_publish(&topics.command_topic, &body)])
        }
        service => Err(super::no_service("a siren", service)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;

    #[test]
    fn a_siren_says_on_or_sends_what_it_was_told() {
        let parsed = parse(
            Component::Siren,
            br#"{"unique_id": "s", "name": "Hall siren", "command_topic": "s/set",
                "state_topic": "s/state", "available_tones": ["alarm", "chime"],
                "support_duration": false}"#,
        )
        .expect("valid");
        assert_eq!(
            parsed.capabilities,
            Capabilities::Siren(SirenCapabilities {
                tones: vec!["alarm".into(), "chime".into()],
                volume: true,
                duration: false,
            })
        );
        assert_eq!(
            crate::state::decode(&parsed.topics, "s/state", b"ON", None),
            Some(Ok(irori_types::State::Siren(irori_types::SirenState {
                on: true
            })))
        );
        let plain = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::SirenTurnOn(irori_types::SirenTurnOn::default()),
        )
        .expect("on");
        assert_eq!(plain[0].payload, b"ON");
        let told = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::SirenTurnOn(irori_types::SirenTurnOn {
                tone: Some("chime".into()),
                volume_level: Some(0.5),
                duration: None,
            }),
        )
        .expect("on, told how");
        let body: serde_json::Value = serde_json::from_slice(&told[0].payload).expect("JSON");
        assert_eq!(
            body,
            serde_json::json!({"state": "ON", "tone": "chime", "volume_level": 0.5})
        );
    }
}
