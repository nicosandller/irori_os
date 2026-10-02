//! Translating between an entity's wire topics/payloads and Irori's typed `State`/`Service`
//! (`docs/specs/entities.md` §5.3, `docs/specs/protocols.md` §7). Each component's own module
//! under [`crate::kinds`] does the reading and writing; this hands each entity to it.

use irori_types::units::{TemperatureUnit, round_to};
use irori_types::{HvacMode, Service, State, UniqueId, WaterHeaterMode};

use crate::discovery::EntityTopics;
use crate::kinds::setting::Reading;
use crate::kinds::{
    binary_sensor, button, climate, cover, event, fan, humidifier, light, lock, number, select,
    sensor, siren, switch, text, valve, water_heater,
};
use crate::template::ValueTemplate;

/// A message to publish: one entity's command can need more than one topic (the default light
/// schema splits on/off and brightness across separate topics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Publish {
    pub topic: String,
    pub payload: Vec<u8>,
}

/// One incoming message, as a component reads it.
#[derive(Debug, Clone, Copy)]
pub struct Message<'a> {
    pub topic: &'a str,
    pub payload: &'a [u8],
}

impl Message<'_> {
    /// Whether it came in on `topic`.
    pub fn on(&self, topic: Option<&str>) -> bool {
        topic == Some(self.topic)
    }

    /// Whether it came in on any of `readings`' topics.
    pub fn on_any<'r>(&self, readings: impl IntoIterator<Item = &'r Reading>) -> bool {
        readings
            .into_iter()
            .any(|reading| reading.topic == self.topic)
    }

    /// What it says through `reading`. `None` when it isn't on that reading's topic, or leaves
    /// that value out (a radiator valve reporting its battery on the topic its settings share);
    /// `Some(None)` when it says it, empty.
    pub fn said(&self, reading: Option<&Reading>) -> Option<Option<String>> {
        let reading = reading.filter(|reading| reading.topic == self.topic)?;
        self.said_through(&reading.template)
    }

    /// What it says through `template`, whichever topic it came in on.
    pub fn said_through(&self, template: &ValueTemplate) -> Option<Option<String>> {
        Some(match template.extract(self.payload).ok()? {
            serde_json::Value::Null => None,
            serde_json::Value::String(text) => Some(text.trim().to_owned()),
            other => Some(other.to_string()),
        })
    }

    /// A number it says through `reading`, as [`Message::said`].
    pub fn number(&self, reading: Option<&Reading>) -> Option<Option<f64>> {
        self.said(reading)
            .map(|said| said.and_then(|text| text.parse().ok()))
    }

    /// A temperature it says through `reading` in `unit`, as °C.
    pub fn temperature(
        &self,
        reading: Option<&Reading>,
        unit: TemperatureUnit,
    ) -> Option<Option<f64>> {
        self.number(reading)
            .map(|said| said.map(|value| round_to(unit.to_celsius(value), 2)))
    }
}

/// `field` as a message said it, when the message said it at all.
pub(crate) fn update<T>(field: &mut Option<T>, said: Option<Option<T>>) {
    if let Some(value) = said {
        *field = value;
    }
}

/// Decodes an incoming `(topic, payload)` against one entity's topics. `None` if `topic` isn't
/// one this entity listens to at all (the caller tries other entities, or ignores it); `Some(Err)`
/// if it matches but the payload can't be read — logged and dropped, never fatal to the protocol.
///
/// `previous` is the entity's last-known state, if any. A component whose state is spread over
/// several topics (the default light schema's on/off and brightness, a thermostat's settings)
/// gets only part of it in one message, and keeps the rest from `previous`.
pub fn decode(
    topics: &EntityTopics,
    topic: &str,
    payload: &[u8],
    previous: Option<&State>,
) -> Option<Result<State, String>> {
    let message = Message { topic, payload };
    match topics {
        EntityTopics::Light(topics) => light::decode(topics, message, previous),
        EntityTopics::Switch(topics) => switch::decode(topics, message),
        EntityTopics::Sensor(topics) => sensor::decode(topics, message),
        EntityTopics::BinarySensor(topics) => binary_sensor::decode(topics, message),
        EntityTopics::Number(topics) => number::decode(topics, message),
        EntityTopics::Select(topics) => select::decode(topics, message),
        EntityTopics::Text(topics) => text::decode(topics, message),
        EntityTopics::Button(_) => None,
        EntityTopics::Event(topics) => event::decode(topics, message),
        EntityTopics::Cover(topics) => cover::decode(topics, message, previous),
        EntityTopics::Valve(topics) => valve::decode(topics, message, previous),
        EntityTopics::Lock(topics) => lock::decode(topics, message),
        EntityTopics::Fan(topics) => fan::decode(topics, message, previous),
        EntityTopics::Siren(topics) => siren::decode(topics, message),
        EntityTopics::Climate(topics) => climate::decode(topics, message, previous),
        EntityTopics::WaterHeater(topics) => water_heater::decode(topics, message, previous),
        EntityTopics::Humidifier(topics) => humidifier::decode(topics, message, previous),
    }
}

/// [`encode`], for an entity whose `turn_on` goes back to how it was: `last_on` is the last
/// state it reported while on (a thermostat's last mode other than `off`).
pub fn encode_with(
    topics: &EntityTopics,
    service: &Service,
    last_on: Option<&State>,
) -> Result<Vec<Publish>, String> {
    match topics {
        EntityTopics::Climate(climate) => climate::encode(climate, service, last_on),
        EntityTopics::WaterHeater(heater) => water_heater::encode(heater, service, last_on),
        _ => encode(topics, service),
    }
}

/// Whether `state` is one to go back to when the entity is turned on again: a thermostat in any
/// mode but `off`.
pub fn is_on(state: &State) -> bool {
    match state {
        State::Climate(climate) => climate.hvac_mode != HvacMode::Off,
        State::WaterHeater(heater) => heater.operation_mode != WaterHeaterMode::Off,
        _ => false,
    }
}

/// Everything needed to publish a service call: the messages to send, in order (usually one;
/// the default light schema sometimes needs two).
pub fn encode(topics: &EntityTopics, service: &Service) -> Result<Vec<Publish>, String> {
    if service.name().kind() != topics.kind() {
        return Err(format!("this entity has no `{}` service", service.name()));
    }
    match topics {
        EntityTopics::Light(topics) => light::encode(topics, service),
        EntityTopics::Switch(topics) => switch::encode(topics, service),
        EntityTopics::Number(topics) => number::encode(topics, service),
        EntityTopics::Select(topics) => select::encode(topics, service),
        EntityTopics::Text(topics) => text::encode(topics, service),
        EntityTopics::Button(topics) => button::encode(topics, service),
        EntityTopics::Cover(topics) => cover::encode(topics, service),
        EntityTopics::Valve(topics) => valve::encode(topics, service),
        EntityTopics::Lock(topics) => lock::encode(topics, service),
        EntityTopics::Fan(topics) => fan::encode(topics, service),
        EntityTopics::Siren(topics) => siren::encode(topics, service),
        EntityTopics::Climate(topics) => climate::encode(topics, service, None),
        EntityTopics::WaterHeater(topics) => water_heater::encode(topics, service, None),
        EntityTopics::Humidifier(topics) => humidifier::encode(topics, service),
        EntityTopics::Sensor(_) | EntityTopics::BinarySensor(_) | EntityTopics::Event(_) => {
            Err(format!("this entity has no `{}` service", service.name()))
        }
    }
}

/// Which entity `unique_id` a topic belongs to, out of a set an entity's own topics carry, for
/// callers that keep a `topic -> unique_id` index rather than scanning every entity per message.
pub fn topics_of(unique_id: &UniqueId, topics: &EntityTopics) -> Vec<(String, UniqueId)> {
    let mut list: Vec<&str> = match topics {
        EntityTopics::Light(topics) => topics.listens(),
        EntityTopics::Switch(topics) => topics.state_topic.as_deref().into_iter().collect(),
        EntityTopics::Sensor(topics) => vec![topics.state_topic.as_str()],
        EntityTopics::BinarySensor(topics) => vec![topics.state_topic.as_str()],
        EntityTopics::Number(topics) => topics.state_topic.as_deref().into_iter().collect(),
        EntityTopics::Select(topics) => topics.state_topic.as_deref().into_iter().collect(),
        EntityTopics::Text(topics) => topics.state_topic.as_deref().into_iter().collect(),
        // Nothing to listen to: a press leaves no state.
        EntityTopics::Button(_) => Vec::new(),
        EntityTopics::Event(topics) => vec![topics.state_topic.as_str()],
        EntityTopics::Lock(topics) => topics.state_topic.as_deref().into_iter().collect(),
        EntityTopics::Siren(topics) => topics.state_topic.as_deref().into_iter().collect(),
        EntityTopics::Cover(topics) => topics.listens(),
        EntityTopics::Valve(topics) => topics.listens(),
        EntityTopics::Fan(topics) => readings(topics.readings()),
        EntityTopics::Climate(topics) => readings(topics.readings()),
        EntityTopics::WaterHeater(topics) => readings(topics.readings()),
        EntityTopics::Humidifier(topics) => readings(topics.readings()),
    };
    // A cover's state and position usually share one topic; it's listened to once.
    list.sort_unstable();
    list.dedup();
    list.into_iter()
        .map(|t| (t.to_owned(), unique_id.clone()))
        .collect()
}

fn readings<'a>(readings: impl IntoIterator<Item = &'a Reading>) -> Vec<&'a str> {
    readings
        .into_iter()
        .map(|reading| reading.topic.as_str())
        .collect()
}

/// On or off, by the words this entity uses for each.
pub(crate) fn decode_on_off(
    payload: &[u8],
    payload_on: &str,
    payload_off: &str,
) -> Result<bool, String> {
    let text = String::from_utf8_lossy(payload);
    let text = text.trim();
    if text == payload_on {
        Ok(true)
    } else if text == payload_off {
        Ok(false)
    } else {
        Err(format!(
            "`{text}` is neither the on payload (`{payload_on}`) nor the off payload (`{payload_off}`)"
        ))
    }
}

/// The text an entity reported, through its value template.
pub(crate) fn decode_text(
    payload: &[u8],
    value_template: &ValueTemplate,
) -> Result<String, String> {
    match value_template.extract(payload)? {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Null => Err("it reported nothing".to_owned()),
        other => Ok(other.to_string()),
    }
}

/// A °C temperature as the device writes it: to a tenth in °F or K, `21` rather than `21.0`.
pub(crate) fn device_temperature(unit: TemperatureUnit, celsius: f64) -> String {
    number_text(round_to(unit.from_celsius(celsius), 1))
}

/// `120`, not `120.0`: a number as a person would type it.
pub(crate) fn number_text(value: f64) -> String {
    let text = value.to_string();
    text.strip_suffix(".0").map_or(text.clone(), str::to_owned)
}

pub(crate) fn json_publish(topic: &str, body: &serde_json::Value) -> Publish {
    Publish {
        topic: topic.to_owned(),
        payload: serde_json::to_vec(body).expect("a json! body always serializes"),
    }
}

pub(crate) fn text_publish(topic: &str, payload: &str) -> Publish {
    Publish {
        topic: topic.to_owned(),
        payload: payload.as_bytes().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{SensorTopics, SwitchTopics};

    #[test]
    fn an_unmatched_topic_is_none_not_an_error() {
        let topics = EntityTopics::Switch(SwitchTopics {
            state_topic: Some("t/state".to_owned()),
            command_topic: "t/set".to_owned(),
            payload_on: "ON".to_owned(),
            payload_off: "OFF".to_owned(),
        });
        assert!(decode(&topics, "unrelated/topic", b"ON", None).is_none());
    }

    #[test]
    fn a_service_the_entity_cant_do_is_a_named_error() {
        let topics = EntityTopics::Sensor(SensorTopics {
            state_topic: "t".to_owned(),
            value_template: crate::template::ValueTemplate::None,
        });
        let error = encode(&topics, &Service::SwitchTurnOn).expect_err("sensors have no services");
        assert!(error.contains("switch.turn_on"));
    }
}
