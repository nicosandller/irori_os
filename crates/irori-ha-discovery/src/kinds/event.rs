//! Home Assistant's MQTT event: each message on `state_topic` that names an event type is one
//! happening.

use irori_types::{Capabilities, EventCapabilities, EventClass, EventState, State};

use crate::discovery::{EntityTopics, str_field};
use crate::state::Message;
use crate::template::ValueTemplate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventTopics {
    pub state_topic: String,
    pub source: EventSource,
}

/// Where in a message an event's type is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventSource {
    /// Home Assistant's own shape: a JSON body with `event_type`.
    EventType,
    /// What a simple `value_template` picks out: the type itself, or an object with `event_type`.
    Template(ValueTemplate),
    /// Zigbee2MQTT's `action`. Its template is a Jinja program that splits a prefix off some
    /// actions; Irori reads the action as it is, so a plain remote's `single`, `double` and
    /// `hold` arrive, and a prefixed one (`1_single`) is refused for not being one of its types.
    Z2mAction,
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let state_topic = str_field(root, "state_topic")
        .ok_or("an event needs a `state_topic`")?
        .to_owned();
    let event_types = root
        .get("event_types")
        .and_then(serde_json::Value::as_array)
        .map(|types| {
            types
                .iter()
                .filter_map(|t| t.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let capabilities = EventCapabilities {
        event_types,
        device_class: str_field(root, "device_class").and_then(EventClass::from_ha),
    };
    capabilities.validate().map_err(|e| e.to_string())?;
    let source = match str_field(root, "value_template") {
        None => EventSource::EventType,
        Some(template) => match ValueTemplate::parse(Some(template)) {
            ValueTemplate::Unsupported(_) if template.contains("value_json.action") => {
                EventSource::Z2mAction
            }
            ValueTemplate::Unsupported(_) => {
                return Err(format!(
                    "its `value_template` {template:?} needs Jinja, which Irori doesn't run"
                ));
            }
            simple => EventSource::Template(simple),
        },
    };
    Ok((
        Capabilities::Event(capabilities),
        EntityTopics::Event(EventTopics {
            state_topic,
            source,
        }),
    ))
}

/// A message with no event in it (a remote's battery level, on the same topic as its presses)
/// isn't for the event at all.
pub(crate) fn decode(topics: &EventTopics, message: Message) -> Option<Result<State, String>> {
    if !message.on(Some(&topics.state_topic)) {
        return None;
    }
    event_type(message.payload, &topics.source)
        .map(|event| event.map(|event_type| State::Event(EventState { event_type })))
}

/// The event type a message names, `None` if it names none.
fn event_type(payload: &[u8], source: &EventSource) -> Option<Result<String, String>> {
    let body = || serde_json::from_slice::<serde_json::Value>(payload).ok();
    let named = |value: Option<&serde_json::Value>| {
        value
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.is_empty())
            .map(|text| Ok(text.to_owned()))
    };
    match source {
        EventSource::EventType => named(body()?.get("event_type")),
        EventSource::Z2mAction => named(body()?.get("action")),
        EventSource::Template(template) => match template.extract(payload) {
            Ok(serde_json::Value::Object(object)) => named(object.get("event_type")),
            Ok(serde_json::Value::String(text)) if !text.is_empty() => Some(Ok(text)),
            Ok(_) => None,
            Err(why) => Some(Err(why)),
        },
    }
}

#[cfg(test)]
mod tests {
    use crate::discovery::ParsedConfig;
    use crate::discovery::parse;
    use crate::topic::Component;

    #[test]
    fn an_event_reads_home_assistants_shape_and_zigbee2mqtts_action() {
        let decode = |parsed: &ParsedConfig, payload: &[u8]| {
            crate::state::decode(&parsed.topics, "remote/state", payload, None)
        };
        let pressed = |t: &str| {
            Some(Ok(irori_types::State::Event(irori_types::EventState {
                event_type: t.into(),
            })))
        };
        // Home Assistant's own: a JSON body naming the event type.
        let plain = parse(
            Component::Event,
            br#"{"unique_id": "r", "name": "Remote", "state_topic": "remote/state",
                "event_types": ["press", "hold"], "device_class": "button"}"#,
        )
        .expect("valid");
        assert_eq!(
            decode(&plain, br#"{"event_type": "hold"}"#),
            pressed("hold")
        );
        assert_eq!(
            decode(&plain, br#"{"battery": 90}"#),
            None,
            "no event in it"
        );

        // Zigbee2MQTT's: its Jinja template is read as the action it picks out.
        let z2m = parse(
            Component::Event,
            br#"{"unique_id": "r2", "name": "Action", "state_topic": "remote/state",
                "event_types": ["single", "double", "hold"],
                "value_template": "{% set action_value = value_json.action|default('') %}{{ ... }}"}"#,
        )
        .expect("valid");
        assert_eq!(
            decode(&z2m, br#"{"action": "double", "battery": 90}"#),
            pressed("double")
        );
        assert_eq!(decode(&z2m, br#"{"action": "", "battery": 90}"#), None);

        // Any other Jinja is left out with a reason.
        let jinja = parse(
            Component::Event,
            br#"{"unique_id": "r3", "name": "X", "state_topic": "x",
                "event_types": ["a"], "value_template": "{{ value_json.k | upper }}"}"#,
        );
        assert!(jinja.is_err_and(|e| e.contains("Jinja")));
    }
}
