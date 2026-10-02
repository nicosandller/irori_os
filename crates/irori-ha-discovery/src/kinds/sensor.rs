//! Home Assistant's MQTT sensor: a reading, a number or a piece of text.

use irori_types::{
    Capabilities, SensorCapabilities, SensorClass, SensorState, SensorValue, SensorValueType,
    State, StateClass,
};

use crate::discovery::{EntityTopics, str_field};
use crate::state::Message;
use crate::template::ValueTemplate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SensorTopics {
    pub state_topic: String,
    pub value_template: ValueTemplate,
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let state_topic = str_field(root, "state_topic")
        .ok_or("a sensor needs a `state_topic`")?
        .to_owned();
    let device_class = str_field(root, "device_class").and_then(SensorClass::from_ha);
    let state_class = str_field(root, "state_class").and_then(|s| match s {
        "measurement" => Some(StateClass::Measurement),
        "total" => Some(StateClass::Total),
        "total_increasing" => Some(StateClass::TotalIncreasing),
        _ => None,
    });
    // HA's `enum` sensors list what they can say; there's no class for that here, only the list.
    let options: Vec<String> = root
        .get("options")
        .and_then(serde_json::Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|o| o.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    // A unit at all is the strongest signal this is a number, not text (HA has no separate
    // "this sensor is numeric" flag) — Tasmota/Z2M numeric sensors always carry one. A class
    // says so too, unless it's one of the text classes (a timestamp) or the sensor lists options.
    let numeric_class = device_class.is_some_and(|class| !class.reports_text());
    let value_type = if options.is_empty()
        && str_field(root, "device_class") != Some("enum")
        && (str_field(root, "unit_of_measurement").is_some() || numeric_class)
    {
        SensorValueType::Number
    } else {
        SensorValueType::Text
    };
    Ok((
        Capabilities::Sensor(SensorCapabilities {
            value_type,
            device_class,
            unit: str_field(root, "unit_of_measurement").map(str::to_owned),
            state_class,
            options,
        }),
        EntityTopics::Sensor(SensorTopics {
            state_topic,
            value_template: ValueTemplate::parse(str_field(root, "value_template")),
        }),
    ))
}

pub(crate) fn decode(topics: &SensorTopics, message: Message) -> Option<Result<State, String>> {
    message
        .on(Some(&topics.state_topic))
        .then(|| read(message.payload, &topics.value_template))
}

fn read(payload: &[u8], value_template: &ValueTemplate) -> Result<State, String> {
    let extracted = value_template.extract(payload)?;
    let value = match &extracted {
        serde_json::Value::Number(n) => n.as_f64().map(SensorValue::Number),
        serde_json::Value::String(s) => s
            .trim()
            .parse::<f64>()
            .ok()
            .map(SensorValue::Number)
            .or_else(|| Some(SensorValue::Text(s.clone()))),
        other => Some(SensorValue::Text(other.to_string())),
    };
    let value = value.ok_or("sensor value couldn't be read")?;
    let state = State::Sensor(SensorState { value });
    state.validate().map_err(|e| e.to_string())?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;
    use irori_types::*;

    /// A Z2M sensor with a nested `value_template`.
    const Z2M_SENSOR: &str = r#"{
        "unique_id": "0x0017880104e45521_power",
        "device": { "identifiers": ["0x0017880104e45521"], "name": "Plug" },
        "device_class": "power",
        "unit_of_measurement": "W",
        "state_class": "measurement",
        "state_topic": "zigbee2mqtt/Plug",
        "value_template": "{{ value_json.power }}"
    }"#;

    #[test]
    fn parses_a_sensor_with_a_nested_value_template_and_maps_device_class() {
        let parsed = parse(Component::Sensor, Z2M_SENSOR.as_bytes()).expect("valid");
        let Capabilities::Sensor(caps) = parsed.capabilities else {
            panic!("expected sensor capabilities")
        };
        assert_eq!(caps.device_class, Some(SensorClass::Power));
        assert_eq!(caps.unit.as_deref(), Some("W"));
        let EntityTopics::Sensor(SensorTopics { value_template, .. }) = parsed.topics else {
            panic!("expected sensor topics")
        };
        assert_eq!(
            value_template,
            ValueTemplate::JsonPath(vec!["power".to_owned()])
        );
    }

    #[test]
    fn enum_and_timestamp_sensors_report_text() {
        let program = parse(
            Component::Sensor,
            br#"{"unique_id": "washer_program", "name": "Program", "state_topic": "washer/state",
                "device_class": "enum", "options": ["wash", "rinse", "spin"]}"#,
        )
        .expect("valid");
        let Capabilities::Sensor(caps) = &program.capabilities else {
            panic!("a sensor");
        };
        assert_eq!(caps.value_type, SensorValueType::Text);
        assert_eq!(caps.options, ["wash", "rinse", "spin"]);
        assert_eq!(caps.device_class, None);

        let last_seen = parse(
            Component::Sensor,
            br#"{"unique_id": "plug_last_seen", "name": "Last seen", "state_topic": "plug/state",
                "device_class": "timestamp", "entity_category": "diagnostic"}"#,
        )
        .expect("valid");
        let Capabilities::Sensor(caps) = &last_seen.capabilities else {
            panic!("a sensor");
        };
        assert_eq!(caps.value_type, SensorValueType::Text);
        assert_eq!(caps.device_class, Some(SensorClass::Timestamp));
        assert_eq!(last_seen.entity_category, Some(EntityCategory::Diagnostic));
        assert_eq!(program.entity_category, None);
    }

    #[test]
    fn ha_carbon_dioxide_maps_to_irori_co2() {
        assert_eq!(
            SensorClass::from_ha("carbon_dioxide"),
            Some(SensorClass::Co2)
        );
        assert_eq!(
            SensorClass::from_ha("co2"),
            None,
            "HA never actually sends this spelling"
        );
    }
}
