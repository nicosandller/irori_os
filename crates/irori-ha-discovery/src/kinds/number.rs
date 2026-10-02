//! Home Assistant's MQTT number: a value set within a range. The plain value is published to
//! `command_topic` (what Zigbee2MQTT's `<device>/set/<property>` takes), and read back through
//! `value_template`.

use irori_types::{
    Capabilities, NumberCapabilities, NumberMode, NumberState, SensorClass, Service, State,
};

use crate::discovery::{EntityTopics, plain_command, str_field};
use crate::state::{Message, Publish, number_text, text_publish};
use crate::template::{CommandTemplate, ValueTemplate};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberTopics {
    pub state_topic: Option<String>,
    pub command_topic: String,
    pub command_template: CommandTemplate,
    pub value_template: ValueTemplate,
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let (command_topic, command_template) = plain_command(root, "a number")?;
    let number = |key: &str, default: f64| {
        root.get(key)
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(default)
    };
    // Home Assistant's own defaults.
    let capabilities = NumberCapabilities {
        min: number("min", 1.0),
        max: number("max", 100.0),
        step: number("step", 1.0),
        unit: str_field(root, "unit_of_measurement").map(str::to_owned),
        device_class: str_field(root, "device_class").and_then(SensorClass::from_ha),
        mode: match str_field(root, "mode") {
            Some("box") => NumberMode::Box,
            Some("slider") => NumberMode::Slider,
            _ => NumberMode::Auto,
        },
    };
    capabilities.validate().map_err(|e| e.to_string())?;
    Ok((
        Capabilities::Number(capabilities),
        EntityTopics::Number(NumberTopics {
            state_topic: str_field(root, "state_topic").map(str::to_owned),
            command_topic,
            command_template,
            value_template: ValueTemplate::parse(str_field(root, "value_template")),
        }),
    ))
}

pub(crate) fn decode(topics: &NumberTopics, message: Message) -> Option<Result<State, String>> {
    message
        .on(topics.state_topic.as_deref())
        .then(|| read(message.payload, &topics.value_template))
}

pub(crate) fn encode(topics: &NumberTopics, service: &Service) -> Result<Vec<Publish>, String> {
    match service {
        Service::NumberSetValue(data) => Ok(vec![text_publish(
            &topics.command_topic,
            &topics.command_template.render(&number_text(data.value)),
        )]),
        service => Err(super::no_service("a number", service)),
    }
}

fn read(payload: &[u8], value_template: &ValueTemplate) -> Result<State, String> {
    let value = match value_template.extract(payload)? {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
    .ok_or("number value isn't a number")?;
    let state = State::Number(NumberState { value });
    state.validate().map_err(|e| e.to_string())?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;
    use irori_types::*;

    /// What Zigbee2MQTT 2.x publishes for a numeric expose it can set
    /// (`lib/extension/homeassistant.ts`, `case "numeric"`), after it fills in its topics.
    const Z2M_NUMBER: &[u8] = br#"{
        "unique_id": "0x0211000000000002_occupancy_timeout_zigbee2mqtt",
        "name": "Occupancy timeout",
        "device": {"identifiers": ["zigbee2mqtt_0x0211000000000002"], "name": "Presence sensor"},
        "state_topic": "zigbee2mqtt/Presence sensor",
        "value_template": "{{ value_json[\"occupancy_timeout\"] }}",
        "command_topic": "zigbee2mqtt/Presence sensor/set/occupancy_timeout",
        "unit_of_measurement": "s", "step": 1, "min": 0, "max": 65535,
        "entity_category": "config"
    }"#;

    #[test]
    fn parses_a_z2m_number_and_sends_it_the_plain_value() {
        let parsed = parse(Component::Number, Z2M_NUMBER).expect("valid");
        let Capabilities::Number(caps) = &parsed.capabilities else {
            panic!("a number");
        };
        assert_eq!((caps.min, caps.max, caps.step), (0.0, 65535.0, 1.0));
        assert_eq!(caps.unit.as_deref(), Some("s"));
        assert_eq!(parsed.entity_category, Some(EntityCategory::Config));

        let state = crate::state::decode(
            &parsed.topics,
            "zigbee2mqtt/Presence sensor",
            br#"{"occupancy": true, "occupancy_timeout": 90}"#,
            None,
        );
        assert_eq!(
            state,
            Some(Ok(irori_types::State::Number(irori_types::NumberState {
                value: 90.0
            })))
        );
        let sent = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::NumberSetValue(irori_types::NumberSetValue { value: 120.0 }),
        )
        .expect("a number takes set_value");
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0].topic,
            "zigbee2mqtt/Presence sensor/set/occupancy_timeout"
        );
        assert_eq!(sent[0].payload, b"120");
    }

    #[test]
    fn a_number_whose_command_needs_jinja_is_left_out_with_a_reason() {
        let payload = br#"{"unique_id": "n", "name": "Level", "command_topic": "x/set",
            "command_template": "{\"level\": {{ value * 10 }}}"}"#;
        let error = parse(Component::Number, payload).expect_err("can't render it");
        assert!(error.contains("command_template"), "{error}");
        // A template that only passes the value through is fine.
        let plain = br#"{"unique_id": "n", "name": "Level", "command_topic": "x/set",
            "command_template": "{{ value }}"}"#;
        assert!(parse(Component::Number, plain).is_ok());
    }
}
