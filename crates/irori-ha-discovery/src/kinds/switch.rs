//! Home Assistant's MQTT switch: a plain on and off.

use irori_types::{Capabilities, Service, State, SwitchCapabilities, SwitchClass, SwitchState};

use crate::discovery::{EntityTopics, owned_str, str_field};
use crate::state::{Message, Publish, decode_on_off, text_publish};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchTopics {
    pub state_topic: Option<String>,
    pub command_topic: String,
    pub payload_on: String,
    pub payload_off: String,
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let device_class = str_field(root, "device_class").and_then(SwitchClass::from_ha);
    let command_topic = str_field(root, "command_topic")
        .ok_or("a switch needs a `command_topic`")?
        .to_owned();
    Ok((
        Capabilities::Switch(SwitchCapabilities { device_class }),
        EntityTopics::Switch(SwitchTopics {
            state_topic: str_field(root, "state_topic").map(str::to_owned),
            command_topic,
            payload_on: owned_str(root, "payload_on", "ON"),
            payload_off: owned_str(root, "payload_off", "OFF"),
        }),
    ))
}

pub(crate) fn decode(topics: &SwitchTopics, message: Message) -> Option<Result<State, String>> {
    message.on(topics.state_topic.as_deref()).then(|| {
        decode_on_off(message.payload, &topics.payload_on, &topics.payload_off)
            .map(|on| State::Switch(SwitchState { on }))
    })
}

pub(crate) fn encode(topics: &SwitchTopics, service: &Service) -> Result<Vec<Publish>, String> {
    let payload = match service {
        Service::SwitchTurnOn => &topics.payload_on,
        Service::SwitchTurnOff => &topics.payload_off,
        service => return Err(super::no_service("a switch", service)),
    };
    Ok(vec![text_publish(&topics.command_topic, payload)])
}
