//! Translating between an entity's wire topics/payloads and Irori's typed `State`/`Service`
//! (`docs/specs/entities.md` §5.3, `docs/specs/protocols.md` §7).

use irori_types::{
    BinarySensorState, ColorMode, LightState, SensorState, SensorValue, Service, State,
    SwitchState, UniqueId,
};

use crate::discovery::EntityTopics;

/// A message to publish: one entity's command can need more than one topic (the default light
/// schema splits on/off and brightness across separate topics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Publish {
    pub topic: String,
    pub payload: Vec<u8>,
}

/// Decodes an incoming `(topic, payload)` against one entity's topics. `None` if `topic` isn't
/// one this entity listens to at all (the caller tries other entities, or ignores it); `Some(Err)`
/// if it matches but the payload can't be read — logged and dropped, never fatal to the protocol.
///
/// `previous` is the entity's last-known state, if any. The default light schema splits on/off
/// and brightness across two topics, so a report on either one alone is incomplete on its own —
/// without merging in what the *other* topic last said, an on/off report would erase the
/// remembered brightness, and a brightness report would force the light on even if it's off.
/// Every other kind of report is already complete by itself, so `previous` only matters here.
pub fn decode(
    topics: &EntityTopics,
    topic: &str,
    payload: &[u8],
    previous: Option<&State>,
) -> Option<Result<State, String>> {
    let previous_light = match previous {
        Some(State::Light(light)) => Some(light),
        _ => None,
    };
    match topics {
        EntityTopics::LightJson { state_topic, .. } if state_topic == topic => {
            Some(decode_light_json(payload))
        }
        EntityTopics::LightDefault {
            state_topic,
            brightness_state_topic,
            brightness_scale,
            payload_on,
            payload_off,
            ..
        } => {
            if state_topic.as_deref() == Some(topic) {
                Some(decode_light_default_on_off(
                    payload,
                    payload_on,
                    payload_off,
                    previous_light,
                ))
            } else if brightness_state_topic.as_deref() == Some(topic) {
                Some(decode_light_default_brightness(
                    payload,
                    *brightness_scale,
                    previous_light,
                ))
            } else {
                None
            }
        }
        EntityTopics::Switch {
            state_topic,
            payload_on,
            payload_off,
            ..
        } if state_topic.as_deref() == Some(topic) => Some(
            decode_on_off(payload, payload_on, payload_off)
                .map(|on| State::Switch(SwitchState { on })),
        ),
        EntityTopics::Sensor {
            state_topic,
            value_template,
        } if state_topic == topic => Some(decode_sensor(payload, value_template)),
        EntityTopics::BinarySensor {
            state_topic,
            payload_on,
            payload_off,
        } if state_topic == topic => Some(
            decode_on_off(payload, payload_on, payload_off)
                .map(|on| State::BinarySensor(BinarySensorState { on })),
        ),
        _ => None,
    }
}

fn decode_on_off(payload: &[u8], payload_on: &str, payload_off: &str) -> Result<bool, String> {
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

fn decode_light_default_on_off(
    payload: &[u8],
    payload_on: &str,
    payload_off: &str,
    previous: Option<&LightState>,
) -> Result<State, String> {
    decode_on_off(payload, payload_on, payload_off).map(|on| {
        State::Light(LightState {
            on,
            brightness: previous.and_then(|p| p.brightness),
            color_mode: previous.and_then(|p| p.color_mode),
            color_temp_kelvin: previous.and_then(|p| p.color_temp_kelvin),
            rgb: previous.and_then(|p| p.rgb),
        })
    })
}

/// Brightness alone doesn't say on/off, so this merges in the entity's last-known `on` (and any
/// other last-known fields) rather than assuming a value — `previous` is threaded all the way
/// from the run loop's own per-entity last state (`docs/specs/protocols.md` §6.3: a report only
/// carries what changed, so the pieces it doesn't carry come from what's already known).
fn decode_light_default_brightness(
    payload: &[u8],
    scale: u32,
    previous: Option<&LightState>,
) -> Result<State, String> {
    let text = String::from_utf8_lossy(payload);
    let raw: f64 = text
        .trim()
        .parse()
        .map_err(|_| format!("`{}` isn't a brightness number", text.trim()))?;
    if raw <= 0.0 {
        return Err(format!("brightness {raw} isn't positive"));
    }
    let scaled = ((raw / f64::from(scale)) * 255.0).round().clamp(1.0, 255.0) as u8;
    Ok(State::Light(LightState {
        on: previous.map(|p| p.on).unwrap_or(true), // no prior report: brightness > 0 implies on
        brightness: Some(scaled),
        color_mode: previous.and_then(|p| p.color_mode),
        color_temp_kelvin: previous.and_then(|p| p.color_temp_kelvin),
        rgb: previous.and_then(|p| p.rgb),
    }))
}

fn decode_light_json(payload: &[u8]) -> Result<State, String> {
    let body: serde_json::Value =
        serde_json::from_slice(payload).map_err(|e| format!("light payload isn't JSON: {e}"))?;
    let on = match body.get("state").and_then(serde_json::Value::as_str) {
        Some("ON") => true,
        Some("OFF") => false,
        Some(other) => return Err(format!("`{other}` isn't a light state (ON/OFF)")),
        None => return Err("light payload has no `state`".to_owned()),
    };
    let brightness = body
        .get("brightness")
        .and_then(serde_json::Value::as_u64)
        .map(|b| b.clamp(1, 255) as u8);
    let color_temp_kelvin = body
        .get("color_temp")
        .and_then(serde_json::Value::as_u64)
        .filter(|m| *m > 0)
        .map(|mireds| (1_000_000 / mireds).clamp(1000, 20000) as u16);
    let (color_mode, rgb) = match body.get("color") {
        Some(color) => match (
            color.get("r").and_then(serde_json::Value::as_u64),
            color.get("g").and_then(serde_json::Value::as_u64),
            color.get("b").and_then(serde_json::Value::as_u64),
        ) {
            (Some(r), Some(g), Some(b)) => {
                (Some(ColorMode::Rgb), Some([r as u8, g as u8, b as u8]))
            }
            _ => match (
                color.get("x").and_then(serde_json::Value::as_f64),
                color.get("y").and_then(serde_json::Value::as_f64),
            ) {
                (Some(x), Some(y)) => (Some(ColorMode::Rgb), Some(xy_to_rgb(x, y))),
                _ => (None, None),
            },
        },
        None => (color_temp_kelvin.map(|_| ColorMode::ColorTemp), None),
    };
    Ok(State::Light(LightState {
        on,
        brightness,
        color_mode,
        color_temp_kelvin,
        rgb,
    }))
}

/// CIE 1931 `(x, y)` chromaticity (what most Zigbee bulbs, including Hue-compatible ones, report
/// their color as) to sRGB, assuming full brightness — Irori's own brightness field carries the
/// level separately. The standard "Wide RGB D65" conversion Philips documents for Hue, and the
/// one Z2M/HA themselves use.
fn xy_to_rgb(x: f64, y: f64) -> [u8; 3] {
    if y <= 0.0 {
        return [0, 0, 0];
    }
    let z = 1.0 - x - y;
    let big_y = 1.0;
    let big_x = (big_y / y) * x;
    let big_z = (big_y / y) * z;

    let r = big_x * 1.656_492 - big_y * 0.354_851 - big_z * 0.255_038;
    let g = -big_x * 0.707_196 + big_y * 1.655_397 + big_z * 0.036_152;
    let b = big_x * 0.051_713 - big_y * 0.121_364 + big_z * 1.011_530;

    let gamma = |c: f64| {
        let c = if c <= 0.0031308 {
            12.92 * c
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (c.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    [gamma(r), gamma(g), gamma(b)]
}

fn decode_sensor(
    payload: &[u8],
    value_template: &crate::template::ValueTemplate,
) -> Result<State, String> {
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

/// Everything needed to publish a service call: the messages to send, in order (usually one;
/// the default light schema sometimes needs two).
pub fn encode(topics: &EntityTopics, service: &Service) -> Result<Vec<Publish>, String> {
    match (topics, service) {
        (EntityTopics::LightJson { command_topic, .. }, Service::LightTurnOff) => {
            Ok(vec![json_publish(
                command_topic,
                &serde_json::json!({ "state": "OFF" }),
            )])
        }
        (EntityTopics::LightJson { command_topic, .. }, Service::LightTurnOn(turn_on)) => {
            let mut body = serde_json::json!({ "state": "ON" });
            if let Some(brightness) = turn_on.brightness {
                body["brightness"] = serde_json::json!(brightness);
            }
            if let Some(kelvin) = turn_on.color_temp_kelvin {
                body["color_temp"] = serde_json::json!((1_000_000 / u32::from(kelvin)).max(1));
            }
            if let Some([r, g, b]) = turn_on.rgb {
                body["color"] = serde_json::json!({ "r": r, "g": g, "b": b });
            }
            Ok(vec![json_publish(command_topic, &body)])
        }
        (
            EntityTopics::LightDefault {
                command_topic,
                payload_off,
                ..
            },
            Service::LightTurnOff,
        ) => Ok(vec![text_publish(command_topic, payload_off)]),
        (
            EntityTopics::LightDefault {
                command_topic,
                payload_on,
                brightness_command_topic,
                brightness_scale,
                ..
            },
            Service::LightTurnOn(turn_on),
        ) => {
            let mut messages = vec![text_publish(command_topic, payload_on)];
            if let (Some(topic), Some(brightness)) = (brightness_command_topic, turn_on.brightness)
            {
                let scaled = ((f64::from(brightness) / 255.0) * f64::from(*brightness_scale))
                    .round()
                    .max(1.0);
                messages.push(text_publish(topic, &format!("{scaled:.0}")));
            }
            Ok(messages)
        }
        (
            EntityTopics::Switch {
                command_topic,
                payload_on,
                ..
            },
            Service::SwitchTurnOn,
        ) => Ok(vec![text_publish(command_topic, payload_on)]),
        (
            EntityTopics::Switch {
                command_topic,
                payload_off,
                ..
            },
            Service::SwitchTurnOff,
        ) => Ok(vec![text_publish(command_topic, payload_off)]),
        _ => Err(format!("this entity has no `{}` service", service.name())),
    }
}

fn json_publish(topic: &str, body: &serde_json::Value) -> Publish {
    Publish {
        topic: topic.to_owned(),
        payload: serde_json::to_vec(body).expect("a json! body always serializes"),
    }
}

fn text_publish(topic: &str, payload: &str) -> Publish {
    Publish {
        topic: topic.to_owned(),
        payload: payload.as_bytes().to_vec(),
    }
}

/// Which entity `unique_id` a topic belongs to, out of a set an entity's own topics carry, for
/// callers that keep a `topic -> unique_id` index rather than scanning every entity per message.
pub fn topics_of(unique_id: &UniqueId, topics: &EntityTopics) -> Vec<(String, UniqueId)> {
    let mut list = Vec::new();
    match topics {
        EntityTopics::LightJson { state_topic, .. } => list.push(state_topic.clone()),
        EntityTopics::LightDefault {
            state_topic,
            brightness_state_topic,
            ..
        } => {
            list.extend(state_topic.clone());
            list.extend(brightness_state_topic.clone());
        }
        EntityTopics::Switch { state_topic, .. } => list.extend(state_topic.clone()),
        EntityTopics::Sensor { state_topic, .. } => list.push(state_topic.clone()),
        EntityTopics::BinarySensor { state_topic, .. } => list.push(state_topic.clone()),
    }
    list.into_iter().map(|t| (t, unique_id.clone())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::LightTurnOn;

    #[test]
    fn decodes_a_z2m_json_light_with_rgb_color() {
        let topics = EntityTopics::LightJson {
            state_topic: "t/state".to_owned(),
            command_topic: "t/set".to_owned(),
        };
        let payload = br#"{"state":"ON","brightness":128,"color":{"r":10,"g":20,"b":30}}"#;
        let state = decode(&topics, "t/state", payload, None)
            .expect("matches")
            .expect("decodes");
        assert_eq!(
            state,
            State::Light(LightState {
                on: true,
                brightness: Some(128),
                color_mode: Some(ColorMode::Rgb),
                color_temp_kelvin: None,
                rgb: Some([10, 20, 30]),
            })
        );
    }

    #[test]
    fn decodes_xy_color_into_rgb() {
        let topics = EntityTopics::LightJson {
            state_topic: "t/state".to_owned(),
            command_topic: "t/set".to_owned(),
        };
        // Roughly a warm white; just checking it lands in a sane RGB region, not an exact triple.
        let payload = br#"{"state":"ON","color":{"x":0.44,"y":0.40}}"#;
        let state = decode(&topics, "t/state", payload, None)
            .expect("matches")
            .expect("decodes");
        let State::Light(light) = state else {
            panic!("expected light")
        };
        let [r, _g, b] = light.rgb.expect("converted from xy");
        assert!(r > b, "a warm color should be redder than blue: {r} vs {b}");
    }

    #[test]
    fn an_unmatched_topic_is_none_not_an_error() {
        let topics = EntityTopics::Switch {
            state_topic: Some("t/state".to_owned()),
            command_topic: "t/set".to_owned(),
            payload_on: "ON".to_owned(),
            payload_off: "OFF".to_owned(),
        };
        assert!(decode(&topics, "unrelated/topic", b"ON", None).is_none());
    }

    #[test]
    fn a_default_schema_light_publishes_two_messages_for_brightness() {
        let topics = EntityTopics::LightDefault {
            state_topic: Some("t/POWER".to_owned()),
            command_topic: "t/cmnd/POWER".to_owned(),
            payload_on: "ON".to_owned(),
            payload_off: "OFF".to_owned(),
            brightness_state_topic: Some("t/RESULT".to_owned()),
            brightness_command_topic: Some("t/cmnd/Dimmer".to_owned()),
            brightness_scale: 100,
        };
        let call = Service::LightTurnOn(LightTurnOn {
            brightness: Some(128),
            color_temp_kelvin: None,
            rgb: None,
        });
        let messages = encode(&topics, &call).expect("encodes");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].topic, "t/cmnd/POWER");
        assert_eq!(messages[0].payload, b"ON");
        assert_eq!(messages[1].topic, "t/cmnd/Dimmer");
        // 128/255 * 100, rounded
        assert_eq!(messages[1].payload, b"50");
    }

    #[test]
    fn a_default_schema_brightness_report_keeps_the_previous_on_state() {
        let topics = EntityTopics::LightDefault {
            state_topic: Some("t/POWER".to_owned()),
            command_topic: "t/cmnd/POWER".to_owned(),
            payload_on: "ON".to_owned(),
            payload_off: "OFF".to_owned(),
            brightness_state_topic: Some("t/RESULT".to_owned()),
            brightness_command_topic: Some("t/cmnd/Dimmer".to_owned()),
            brightness_scale: 100,
        };
        let previous = State::Light(LightState {
            on: true,
            brightness: Some(10),
            color_mode: Some(ColorMode::Rgb),
            color_temp_kelvin: None,
            rgb: Some([1, 2, 3]),
        });
        let state = decode(&topics, "t/RESULT", b"50", Some(&previous))
            .expect("matches")
            .expect("decodes");
        assert_eq!(
            state,
            State::Light(LightState {
                on: true,
                brightness: Some(128), // 50/100 * 255, rounded
                color_mode: Some(ColorMode::Rgb),
                color_temp_kelvin: None,
                rgb: Some([1, 2, 3]),
            })
        );
    }

    #[test]
    fn a_default_schema_on_off_report_keeps_the_previous_brightness() {
        let topics = EntityTopics::LightDefault {
            state_topic: Some("t/POWER".to_owned()),
            command_topic: "t/cmnd/POWER".to_owned(),
            payload_on: "ON".to_owned(),
            payload_off: "OFF".to_owned(),
            brightness_state_topic: Some("t/RESULT".to_owned()),
            brightness_command_topic: Some("t/cmnd/Dimmer".to_owned()),
            brightness_scale: 100,
        };
        let previous = State::Light(LightState {
            on: false,
            brightness: Some(77),
            color_mode: None,
            color_temp_kelvin: None,
            rgb: None,
        });
        let state = decode(&topics, "t/POWER", b"ON", Some(&previous))
            .expect("matches")
            .expect("decodes");
        assert_eq!(
            state,
            State::Light(LightState {
                on: true,
                brightness: Some(77),
                color_mode: None,
                color_temp_kelvin: None,
                rgb: None,
            })
        );
    }

    #[test]
    fn a_json_light_turn_off_ignores_any_data() {
        let topics = EntityTopics::LightJson {
            state_topic: "t/state".to_owned(),
            command_topic: "t/set".to_owned(),
        };
        let publish = encode(&topics, &Service::LightTurnOff).expect("encodes");
        assert_eq!(publish.len(), 1);
        assert_eq!(publish[0].payload, br#"{"state":"OFF"}"#);
    }

    #[test]
    fn a_service_the_entity_cant_do_is_a_named_error() {
        let topics = EntityTopics::Sensor {
            state_topic: "t".to_owned(),
            value_template: crate::template::ValueTemplate::None,
        };
        let error = encode(&topics, &Service::SwitchTurnOn).expect_err("sensors have no services");
        assert!(error.contains("switch.turn_on"));
    }
}
