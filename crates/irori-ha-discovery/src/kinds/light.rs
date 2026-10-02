//! Home Assistant's MQTT light, in its two schemas: Zigbee2MQTT's JSON and the default one
//! (Tasmota) with a topic per feature.

use irori_types::{
    Capabilities, ColorMode, ColorTempRange, LightCapabilities, LightState, Service, State,
};

use crate::discovery::{EntityTopics, bool_field, owned_str, str_field};
use crate::state::{Message, Publish, decode_on_off, json_publish, text_publish};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LightTopics {
    /// Z2M's `schema: "json"`: one topic each way, JSON body
    /// `{state, brightness, color_temp, color: {r,g,b}}`.
    Json {
        state_topic: String,
        command_topic: String,
    },
    /// The default/plain schema (Tasmota): separate topics per feature, plain-text payloads.
    Default {
        state_topic: Option<String>,
        command_topic: String,
        payload_on: String,
        payload_off: String,
        brightness_state_topic: Option<String>,
        brightness_command_topic: Option<String>,
        /// The scale brightness is published/commanded in, e.g. Tasmota's 100. Irori's own
        /// scale is 1-255.
        brightness_scale: u32,
    },
}

impl LightTopics {
    pub(crate) fn listens(&self) -> Vec<&str> {
        match self {
            Self::Json { state_topic, .. } => vec![state_topic.as_str()],
            Self::Default {
                state_topic,
                brightness_state_topic,
                ..
            } => [state_topic, brightness_state_topic]
                .into_iter()
                .filter_map(Option::as_deref)
                .collect(),
        }
    }
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let modes: Vec<&str> = root
        .get("supported_color_modes")
        .and_then(serde_json::Value::as_array)
        .map(|items| items.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();
    let has_mode = |m: &str| modes.contains(&m);
    // Falls back to the legacy boolean flags when `supported_color_modes` is absent, which
    // Tasmota's default schema still uses.
    let brightness = if modes.is_empty() {
        bool_field(root, "brightness")
    } else {
        has_mode("brightness")
            || has_mode("color_temp")
            || has_mode("rgb")
            || has_mode("xy")
            || has_mode("hs")
    };
    let supports_color_temp = if modes.is_empty() {
        bool_field(root, "color_temp")
    } else {
        has_mode("color_temp")
    };
    // Irori has one generic "color" capability; xy/hs are both treated as it, same as `rgb`
    // (documented simplification — see the crate's README).
    let supports_rgb = if modes.is_empty() {
        bool_field(root, "rgb")
    } else {
        has_mode("rgb") || has_mode("xy") || has_mode("hs")
    };
    let color_temp_kelvin = supports_color_temp
        .then(|| color_temp_range(root))
        .transpose()?;
    let capabilities = Capabilities::Light(LightCapabilities {
        brightness,
        color_temp_kelvin,
        rgb: supports_rgb,
    });

    let command_topic = str_field(root, "command_topic")
        .ok_or("a light needs a `command_topic`")?
        .to_owned();
    let topics = if str_field(root, "schema") == Some("json") {
        LightTopics::Json {
            state_topic: owned_str(root, "state_topic", &command_topic),
            command_topic,
        }
    } else {
        LightTopics::Default {
            state_topic: str_field(root, "state_topic").map(str::to_owned),
            command_topic,
            payload_on: owned_str(root, "payload_on", "ON"),
            payload_off: owned_str(root, "payload_off", "OFF"),
            brightness_state_topic: str_field(root, "brightness_state_topic").map(str::to_owned),
            brightness_command_topic: str_field(root, "brightness_command_topic")
                .map(str::to_owned),
            brightness_scale: root
                .get("brightness_scale")
                .and_then(serde_json::Value::as_u64)
                .map(|n| n as u32)
                .unwrap_or(255),
        }
    };
    Ok((capabilities, EntityTopics::Light(topics)))
}

/// `min_kelvin`/`max_kelvin` if HA's newer form is present; otherwise the older `min_mireds`/
/// `max_mireds`, inverted (mireds and kelvin move opposite ways: `kelvin = 1_000_000 / mireds`),
/// falling back to HA's own defaults (153-500 mireds, i.e. roughly 2000-6535 K) when neither is
/// given at all but color temperature is still declared supported.
fn color_temp_range(root: &serde_json::Value) -> Result<ColorTempRange, String> {
    let as_u32 = |key: &str| {
        root.get(key)
            .and_then(serde_json::Value::as_u64)
            .map(|n| n as u32)
    };
    let (min, max) = match (as_u32("min_kelvin"), as_u32("max_kelvin")) {
        (Some(min), Some(max)) => (min, max),
        _ => {
            let min_mireds = as_u32("max_mireds").unwrap_or(500); // max mireds -> min kelvin
            let max_mireds = as_u32("min_mireds").unwrap_or(153); // min mireds -> max kelvin
            let mireds_to_kelvin = |m: u32| 1_000_000_u32.checked_div(m).unwrap_or(20000);
            (mireds_to_kelvin(min_mireds), mireds_to_kelvin(max_mireds))
        }
    };
    let clamp = |k: u32| k.clamp(1000, 20000) as u16;
    let (min, max) = (clamp(min), clamp(max));
    let range = if min <= max {
        ColorTempRange { min, max }
    } else {
        ColorTempRange { min: max, max: min }
    };
    range.validate().map(|()| range).map_err(|e| e.to_string())
}

/// The default schema splits on/off and brightness across two topics, so a report on either one
/// alone is incomplete: without merging in what the *other* topic last said, an on/off report
/// would erase the remembered brightness, and a brightness report would force the light on even
/// if it's off.
pub(crate) fn decode(
    topics: &LightTopics,
    message: Message,
    previous: Option<&State>,
) -> Option<Result<State, String>> {
    let previous = match previous {
        Some(State::Light(light)) => Some(light),
        _ => None,
    };
    let Message { topic, payload } = message;
    match topics {
        LightTopics::Json { state_topic, .. } if state_topic == topic => {
            Some(decode_light_json(payload))
        }
        LightTopics::Json { .. } => None,
        LightTopics::Default {
            state_topic,
            brightness_state_topic,
            brightness_scale,
            payload_on,
            payload_off,
            ..
        } => {
            if message.on(state_topic.as_deref()) {
                Some(decode_light_default_on_off(
                    payload,
                    payload_on,
                    payload_off,
                    previous,
                ))
            } else if message.on(brightness_state_topic.as_deref()) {
                Some(decode_light_default_brightness(
                    payload,
                    *brightness_scale,
                    previous,
                ))
            } else {
                None
            }
        }
    }
}

pub(crate) fn encode(topics: &LightTopics, service: &Service) -> Result<Vec<Publish>, String> {
    match (topics, service) {
        (LightTopics::Json { command_topic, .. }, Service::LightTurnOff) => Ok(vec![json_publish(
            command_topic,
            &serde_json::json!({ "state": "OFF" }),
        )]),
        (LightTopics::Json { command_topic, .. }, Service::LightTurnOn(turn_on)) => {
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
            LightTopics::Default {
                command_topic,
                payload_off,
                ..
            },
            Service::LightTurnOff,
        ) => Ok(vec![text_publish(command_topic, payload_off)]),
        (
            LightTopics::Default {
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
        (_, service) => Err(super::no_service("a light", service)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::state::{decode, encode};
    use crate::topic::Component;
    use irori_types::*;

    /// A Zigbee2MQTT-shaped light: JSON schema, modern `supported_color_modes`.
    const Z2M_LIGHT: &str = r#"{
        "unique_id": "0x0017880104e45520_light",
        "device": {
            "identifiers": ["0x0017880104e45520"],
            "name": "Living room lamp",
            "manufacturer": "Philips",
            "model": "Hue color lamp"
        },
        "schema": "json",
        "state_topic": "zigbee2mqtt/Living room lamp",
        "command_topic": "zigbee2mqtt/Living room lamp/set",
        "supported_color_modes": ["color_temp", "xy"],
        "min_mireds": 153,
        "max_mireds": 500,
        "availability_topic": "zigbee2mqtt/bridge/state"
    }"#;

    #[test]
    fn parses_a_z2m_json_schema_light() {
        let parsed = parse(Component::Light, Z2M_LIGHT.as_bytes()).expect("valid");
        assert_eq!(parsed.unique_id.as_str(), "0x0017880104e45520_light");
        let device = parsed.device.expect("has a device");
        assert_eq!(device.unique_id.as_str(), "0x0017880104e45520");
        assert_eq!(device.manufacturer.as_deref(), Some("Philips"));
        let Capabilities::Light(caps) = parsed.capabilities else {
            panic!("expected light capabilities")
        };
        assert!(caps.rgb, "xy counts as rgb (documented simplification)");
        let range = caps.color_temp_kelvin.expect("supports color temp");
        assert_eq!(range.min, 2000); // from max_mireds 500
        assert_eq!(range.max, 6535); // from min_mireds 153, rounded down by integer division
        assert!(matches!(
            parsed.topics,
            EntityTopics::Light(LightTopics::Json { .. })
        ));
        assert_eq!(parsed.availability.len(), 1);
        assert_eq!(parsed.availability[0].payload_available, "online");
    }

    #[test]
    fn decodes_a_z2m_json_light_with_rgb_color() {
        let topics = EntityTopics::Light(LightTopics::Json {
            state_topic: "t/state".to_owned(),
            command_topic: "t/set".to_owned(),
        });
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
        let topics = EntityTopics::Light(LightTopics::Json {
            state_topic: "t/state".to_owned(),
            command_topic: "t/set".to_owned(),
        });
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
    fn a_default_schema_light_publishes_two_messages_for_brightness() {
        let topics = EntityTopics::Light(LightTopics::Default {
            state_topic: Some("t/POWER".to_owned()),
            command_topic: "t/cmnd/POWER".to_owned(),
            payload_on: "ON".to_owned(),
            payload_off: "OFF".to_owned(),
            brightness_state_topic: Some("t/RESULT".to_owned()),
            brightness_command_topic: Some("t/cmnd/Dimmer".to_owned()),
            brightness_scale: 100,
        });
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
        let topics = EntityTopics::Light(LightTopics::Default {
            state_topic: Some("t/POWER".to_owned()),
            command_topic: "t/cmnd/POWER".to_owned(),
            payload_on: "ON".to_owned(),
            payload_off: "OFF".to_owned(),
            brightness_state_topic: Some("t/RESULT".to_owned()),
            brightness_command_topic: Some("t/cmnd/Dimmer".to_owned()),
            brightness_scale: 100,
        });
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
        let topics = EntityTopics::Light(LightTopics::Default {
            state_topic: Some("t/POWER".to_owned()),
            command_topic: "t/cmnd/POWER".to_owned(),
            payload_on: "ON".to_owned(),
            payload_off: "OFF".to_owned(),
            brightness_state_topic: Some("t/RESULT".to_owned()),
            brightness_command_topic: Some("t/cmnd/Dimmer".to_owned()),
            brightness_scale: 100,
        });
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
        let topics = EntityTopics::Light(LightTopics::Json {
            state_topic: "t/state".to_owned(),
            command_topic: "t/set".to_owned(),
        });
        let publish = encode(&topics, &Service::LightTurnOff).expect("encodes");
        assert_eq!(publish.len(), 1);
        assert_eq!(publish[0].payload, br#"{"state":"OFF"}"#);
    }
}
