//! Home Assistant's MQTT binary sensor: on or off, as it words them.

use irori_types::{
    BinarySensorCapabilities, BinarySensorClass, BinarySensorState, Capabilities, State,
};

use crate::discovery::{EntityTopics, owned_str, str_field};
use crate::state::{Message, decode_on_off};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinarySensorTopics {
    pub state_topic: String,
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
            payload_on: owned_str(root, "payload_on", "ON"),
            payload_off: owned_str(root, "payload_off", "OFF"),
        }),
    ))
}

pub(crate) fn decode(
    topics: &BinarySensorTopics,
    message: Message,
) -> Option<Result<State, String>> {
    message.on(Some(&topics.state_topic)).then(|| {
        decode_on_off(message.payload, &topics.payload_on, &topics.payload_off)
            .map(|on| State::BinarySensor(BinarySensorState { on }))
    })
}
