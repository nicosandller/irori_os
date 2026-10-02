//! Translating between an entity's wire topics/payloads and Irori's typed `State`/`Service`
//! (`docs/specs/entities.md` §5.3, `docs/specs/protocols.md` §7).

use irori_types::units::{TemperatureUnit, round_to};
use irori_types::{
    BinarySensorState, ClimateState, ColorMode, CoverState, EventState, FanDirection,
    FanPercentage, FanState, HvacAction, HvacMode, LightState, LockState, NumberState, OpenState,
    SelectState, SensorState, SensorValue, Service, SirenState, SirenTurnOn, State, SwitchState,
    TextState, UniqueId, ValveState, percentage_to_speed, speed_to_percentage,
};
use irori_types::{WaterHeaterMode, WaterHeaterState};

use crate::discovery::{ClimateTopics, EntityTopics, FanPart, WaterHeaterTopics};
use crate::template::ValueTemplate;

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
        EntityTopics::Number {
            state_topic,
            value_template,
            ..
        } if state_topic.as_deref() == Some(topic) => Some(decode_number(payload, value_template)),
        EntityTopics::Cover(cover) => decode_cover(cover, topic, payload, previous),
        EntityTopics::Fan(fan) => decode_fan(fan, topic, payload, previous),
        EntityTopics::Climate(climate) => decode_climate(climate, topic, payload, previous),
        EntityTopics::WaterHeater(heater) => decode_water_heater(heater, topic, payload, previous),
        EntityTopics::Siren {
            state_topic,
            value_template,
            state_on,
            state_off,
            ..
        } if state_topic.as_deref() == Some(topic) => Some(
            decode_text(payload, value_template).and_then(|said| match said.trim() {
                said if said == state_on => Ok(State::Siren(SirenState { on: true })),
                said if said == state_off => Ok(State::Siren(SirenState { on: false })),
                said => Err(format!("{said:?} isn't on or off for this siren")),
            }),
        ),
        // Read as a cover, from what it last said as a valve.
        EntityTopics::Valve(valve) => {
            let previous = match previous {
                Some(State::Valve(old)) => Some(State::Cover(CoverState {
                    state: old.state,
                    position: old.position,
                    tilt: None,
                })),
                _ => None,
            };
            decode_cover(valve, topic, payload, previous.as_ref()).map(|decoded| {
                decoded.map(|state| match state {
                    State::Cover(cover) => State::Valve(ValveState {
                        state: cover.state,
                        position: cover.position,
                    }),
                    other => other,
                })
            })
        }
        EntityTopics::Lock(lock) if lock.state_topic.as_deref() == Some(topic) => {
            Some(decode_text(payload, &lock.value_template).and_then(|said| {
                lock.said
                    .iter()
                    .find(|(word, _)| *word == said.trim())
                    .map(|(_, state)| State::Lock(LockState { state: *state }))
                    .ok_or_else(|| format!("{said:?} isn't a state this lock says"))
            }))
        }
        // A message with no event in it (a remote's battery level, on the same topic as its
        // presses) isn't for the event at all.
        EntityTopics::Event {
            state_topic,
            source,
        } if state_topic == topic => decode_event(payload, source)
            .map(|event| event.map(|event_type| State::Event(EventState { event_type }))),
        EntityTopics::Select {
            state_topic,
            value_template,
            ..
        } if state_topic.as_deref() == Some(topic) => Some(
            decode_text(payload, value_template)
                .map(|option| State::Select(SelectState { option })),
        ),
        EntityTopics::Text {
            state_topic,
            value_template,
            ..
        } if state_topic.as_deref() == Some(topic) => {
            Some(decode_text(payload, value_template).map(|value| State::Text(TextState { value })))
        }
        _ => None,
    }
}

/// A climate entity's settings, each on its own topic, often all on one. A message changes what
/// it carries and keeps the rest of `previous`; until its mode is known there's nothing to
/// report, since a climate state has to say its mode.
fn decode_climate(
    climate: &ClimateTopics,
    topic: &str,
    payload: &[u8],
    previous: Option<&State>,
) -> Option<Result<State, String>> {
    let settings: Vec<(&str, &FanPart)> = climate
        .settings()
        .into_iter()
        .filter_map(|(name, part)| {
            part.filter(|part| part.state_topic.as_deref() == Some(topic))
                .map(|part| (name, part))
        })
        .collect();
    let readings: Vec<(&str, &ValueTemplate)> = [
        ("current_temperature", &climate.current_temperature),
        ("current_humidity", &climate.current_humidity),
        ("action", &climate.action),
    ]
    .into_iter()
    .filter_map(|(name, reading)| match reading {
        Some((reading_topic, template)) if reading_topic == topic => Some((name, template)),
        _ => None,
    })
    .collect();
    if settings.is_empty() && readings.is_empty() {
        return None;
    }
    Some((|| {
        let mut state = match previous {
            Some(State::Climate(old)) => old.clone(),
            // A climate entity with no mode topic never says its mode: it's in the first it has.
            _ if climate
                .mode
                .as_ref()
                .is_none_or(|mode| mode.state_topic.is_none()) =>
            {
                ClimateState::in_mode(*climate.modes.first().unwrap_or(&HvacMode::Off))
            }
            _ => ClimateState::in_mode(HvacMode::Off),
        };
        let mut mode_known = matches!(previous, Some(State::Climate(_)))
            || climate
                .mode
                .as_ref()
                .is_none_or(|mode| mode.state_topic.is_none());
        // Its settings often share one topic, and a message that leaves one out (a radiator
        // valve reporting its battery) leaves that setting as it was: `None` is "not in this
        // message", `Some(None)` is "said, and empty".
        let said = |template: &ValueTemplate| -> Option<Option<String>> {
            Some(match template.extract(payload).ok()? {
                serde_json::Value::Null => None,
                serde_json::Value::String(text) => Some(text.trim().to_owned()),
                other => Some(other.to_string()),
            })
        };
        let number = |template: &ValueTemplate| {
            said(template).map(|text| text.and_then(|t| t.parse::<f64>().ok()))
        };
        let celsius = |template: &ValueTemplate| {
            number(template).map(|n| n.map(|v| round_to(climate.unit.to_celsius(v), 2)))
        };
        for (name, part) in settings {
            let template = &part.value_template;
            match name {
                "mode" => {
                    if let Some(Some(text)) = said(template) {
                        state.hvac_mode = HvacMode::parse(&text)
                            .ok_or_else(|| format!("{text:?} isn't a mode this thermostat has"))?;
                        mode_known = true;
                    }
                }
                "temperature" => update(&mut state.target_temperature, celsius(template)),
                "temperature_low" => update(&mut state.target_temp_low, celsius(template)),
                "temperature_high" => update(&mut state.target_temp_high, celsius(template)),
                "target_humidity" => update(&mut state.target_humidity, number(template)),
                "fan_mode" => update(&mut state.fan_mode, said(template)),
                "swing_mode" => update(&mut state.swing_mode, said(template)),
                _ => update(
                    &mut state.preset_mode,
                    said(template).map(|mode| mode.filter(|mode| mode != "none")),
                ),
            }
        }
        for (name, template) in readings {
            match name {
                "current_temperature" => {
                    update(&mut state.current_temperature, celsius(template));
                }
                "current_humidity" => update(&mut state.current_humidity, number(template)),
                _ => update(
                    &mut state.hvac_action,
                    said(template).map(|text| text.as_deref().and_then(hvac_action)),
                ),
            }
        }
        if !mode_known {
            return Err("it hasn't said its mode yet".to_owned());
        }
        Ok(State::Climate(state))
    })())
}

/// A water heater's mode, target and water temperature, read the way a thermostat's are.
fn decode_water_heater(
    heater: &WaterHeaterTopics,
    topic: &str,
    payload: &[u8],
    previous: Option<&State>,
) -> Option<Result<State, String>> {
    let on = |part: &Option<FanPart>| {
        part.as_ref()
            .filter(|part| part.state_topic.as_deref() == Some(topic))
            .map(|part| part.value_template.clone())
    };
    let (mode, target) = (on(&heater.mode), on(&heater.temperature));
    let current = heater
        .current_temperature
        .as_ref()
        .filter(|(reading, _)| reading == topic)
        .map(|(_, template)| template.clone());
    if mode.is_none() && target.is_none() && current.is_none() {
        return None;
    }
    let said = |template: &ValueTemplate| -> Option<Option<String>> {
        Some(match template.extract(payload).ok()? {
            serde_json::Value::Null => None,
            serde_json::Value::String(text) => Some(text.trim().to_owned()),
            other => Some(other.to_string()),
        })
    };
    let celsius = |template: &ValueTemplate| {
        said(template).map(|text| {
            text.and_then(|t| t.parse::<f64>().ok())
                .map(|v| round_to(heater.unit.to_celsius(v), 2))
        })
    };
    let said_mode = mode.as_ref().and_then(said).flatten();
    Some((|| {
        let operation_mode = match (&said_mode, previous) {
            (Some(text), _) => WaterHeaterMode::parse(text)
                .ok_or_else(|| format!("{text:?} isn't a mode this water heater has"))?,
            (None, Some(State::WaterHeater(old))) => old.operation_mode,
            // One with no mode topic never says its mode: it's in the first it has.
            (None, _) if heater.mode.as_ref().is_none_or(|m| m.state_topic.is_none()) => {
                *heater.modes.first().unwrap_or(&WaterHeaterMode::Off)
            }
            (None, _) => return Err("it hasn't said its mode yet".to_owned()),
        };
        let mut state = match previous {
            Some(State::WaterHeater(old)) => old.clone(),
            _ => WaterHeaterState {
                operation_mode,
                current_temperature: None,
                target_temperature: None,
            },
        };
        state.operation_mode = operation_mode;
        if let Some(template) = &target {
            update(&mut state.target_temperature, celsius(template));
        }
        if let Some(template) = &current {
            update(&mut state.current_temperature, celsius(template));
        }
        Ok(State::WaterHeater(state))
    })())
}

/// `field` as a message said it, when the message said it at all.
fn update<T>(field: &mut Option<T>, said: Option<Option<T>>) {
    if let Some(value) = said {
        *field = value;
    }
}

/// What a thermostat says it's doing, as Home Assistant's action, or as Zigbee2MQTT's
/// `running_state` that its action template would have turned into one.
fn hvac_action(said: &str) -> Option<HvacAction> {
    HvacAction::parse(said).or(match said {
        "heat" => Some(HvacAction::Heating),
        "cool" => Some(HvacAction::Cooling),
        "fan_only" => Some(HvacAction::Fan),
        _ => None,
    })
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

fn decode_number(
    payload: &[u8],
    value_template: &crate::template::ValueTemplate,
) -> Result<State, String> {
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

/// A cover's state from a message on any of its topics, merged with what it last said: the
/// position and the state often arrive on different topics, or in one body.
fn decode_cover(
    cover: &crate::discovery::CoverTopics,
    topic: &str,
    payload: &[u8],
    previous: Option<&State>,
) -> Option<Result<State, String>> {
    let for_state = cover.state_topic.as_deref() == Some(topic);
    let for_position = cover.position_topic.as_deref() == Some(topic);
    let for_tilt = cover.tilt_status_topic.as_deref() == Some(topic);
    if !(for_state || for_position || for_tilt) {
        return None;
    }
    Some((|| {
        let mut position = match previous {
            Some(State::Cover(old)) => old.position,
            _ => None,
        };
        let mut tilt = match previous {
            Some(State::Cover(old)) => old.tilt,
            _ => None,
        };
        let percent = |raw: f64, closed: f64, open: f64| {
            let span = open - closed;
            if span == 0.0 {
                return None;
            }
            let share = ((raw - closed) / span * 100.0).round().clamp(0.0, 100.0);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Some(share as u8)
        };
        let number = |template: &crate::template::ValueTemplate| -> Result<Option<f64>, String> {
            Ok(match template.extract(payload)? {
                serde_json::Value::Number(n) => n.as_f64(),
                serde_json::Value::String(s) => s.trim().parse().ok(),
                _ => None,
            })
        };
        if for_position && let Some(raw) = number(&cover.position_template)? {
            position = percent(raw, cover.position_closed, cover.position_open);
        }
        if for_tilt && let Some(raw) = number(&cover.tilt_status_template)? {
            tilt = percent(raw, cover.tilt_min, cover.tilt_max);
        }
        let from_position = || match position {
            Some(0) => OpenState::Closed,
            _ => OpenState::Open,
        };
        let said = match (&cover.value_template, for_state) {
            (Some(template), true) => match template.extract(payload)? {
                serde_json::Value::String(text) => Some(text.trim().to_owned()),
                serde_json::Value::Null => None,
                other => Some(other.to_string()),
            },
            _ => None,
        };
        let state = match said {
            Some(text) if text == cover.state_open => OpenState::Open,
            Some(text) if text == cover.state_opening => OpenState::Opening,
            Some(text) if text == cover.state_closed => OpenState::Closed,
            Some(text) if text == cover.state_closing => OpenState::Closing,
            Some(text) if text == cover.state_stopped => from_position(),
            Some(text) => return Err(format!("{text:?} isn't a state this cover says")),
            // Nothing said about where it is: its position says, else what it said last.
            None => match (position, previous) {
                (Some(_), _) => from_position(),
                (None, Some(State::Cover(old))) => old.state,
                (None, _) => return Err("the cover hasn't said where it is".to_owned()),
            },
        };
        Ok(State::Cover(CoverState {
            state,
            position,
            tilt,
        }))
    })())
}

/// A fan's state from a message on any of its topics, merged with what it last said. Zigbee2MQTT
/// sends them all in one body; others give each setting a topic of its own.
fn decode_fan(
    fan: &crate::discovery::FanTopics,
    topic: &str,
    payload: &[u8],
    previous: Option<&State>,
) -> Option<Result<State, String>> {
    fn on_topic<'a>(
        part: &'a Option<crate::discovery::FanPart>,
        topic: &str,
    ) -> Option<&'a crate::discovery::FanPart> {
        part.as_ref()
            .filter(|part| part.state_topic.as_deref() == Some(topic))
    }
    let power = (fan.power.state_topic.as_deref() == Some(topic)).then_some(&fan.power);
    let (speed, preset, oscillation, direction) = (
        on_topic(&fan.speed, topic),
        on_topic(&fan.preset, topic),
        on_topic(&fan.oscillation, topic),
        on_topic(&fan.direction, topic),
    );
    if power.is_none()
        && speed.is_none()
        && preset.is_none()
        && oscillation.is_none()
        && direction.is_none()
    {
        return None;
    }
    Some((|| {
        let mut state = match previous {
            Some(State::Fan(old)) => old.clone(),
            _ => FanState {
                on: false,
                percentage: None,
                oscillating: None,
                direction: None,
                preset_mode: None,
            },
        };
        let text = |part: &crate::discovery::FanPart| -> Result<Option<String>, String> {
            Ok(match part.value_template.extract(payload)? {
                serde_json::Value::Null => None,
                serde_json::Value::String(text) => Some(text.trim().to_owned()),
                other => Some(other.to_string()),
            })
        };
        if let Some(part) = power {
            match text(part)? {
                Some(said) if said == fan.payload_on => state.on = true,
                Some(said) if said == fan.payload_off => state.on = false,
                Some(said) => return Err(format!("{said:?} isn't on or off for this fan")),
                None => {}
            }
        }
        if let Some(part) = speed
            && let Some(raw) = text(part)?.and_then(|said| said.parse::<f64>().ok())
        {
            let count = fan.speed_max - fan.speed_min + 1;
            #[allow(clippy::cast_possible_truncation)]
            let level = raw.round() as i64 - fan.speed_min + 1;
            state.percentage = Some(if level <= 0 || count <= 0 {
                0
            } else {
                speed_to_percentage(
                    u16::try_from(level).unwrap_or(u16::MAX),
                    u16::try_from(count).unwrap_or(u16::MAX),
                )
            });
        }
        if let Some(part) = preset {
            // Anything not a preset (a speed, `None`) means it isn't in one.
            state.preset_mode = text(part)?;
        }
        if let Some(part) = oscillation {
            match text(part)? {
                Some(said) if said == fan.payload_oscillation_on => state.oscillating = Some(true),
                Some(said) if said == fan.payload_oscillation_off => {
                    state.oscillating = Some(false);
                }
                _ => {}
            }
        }
        if let Some(part) = direction {
            state.direction = match text(part)?.as_deref() {
                Some("forward") => Some(FanDirection::Forward),
                Some("reverse") => Some(FanDirection::Reverse),
                _ => state.direction,
            };
        }
        Ok(State::Fan(state))
    })())
}

/// The event type a message names, `None` if it names none.
fn decode_event(
    payload: &[u8],
    source: &crate::discovery::EventSource,
) -> Option<Result<String, String>> {
    use crate::discovery::EventSource;
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

/// The text a select or a text entity reported, through its value template.
fn decode_text(
    payload: &[u8],
    value_template: &crate::template::ValueTemplate,
) -> Result<String, String> {
    match value_template.extract(payload)? {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Null => Err("it reported nothing".to_owned()),
        other => Ok(other.to_string()),
    }
}

/// Everything needed to publish a service call: the messages to send, in order (usually one;
/// the default light schema sometimes needs two).
/// [`encode`], for an entity whose `turn_on` goes back to how it was: `last_on` is the last
/// state it reported while on (a thermostat's last mode other than `off`).
pub fn encode_with(
    topics: &EntityTopics,
    service: &Service,
    last_on: Option<&State>,
) -> Result<Vec<Publish>, String> {
    match topics {
        EntityTopics::Climate(climate) => encode_climate(climate, service, last_on),
        EntityTopics::WaterHeater(heater) => encode_water_heater(heater, service, last_on),
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

pub fn encode(topics: &EntityTopics, service: &Service) -> Result<Vec<Publish>, String> {
    match (topics, service) {
        (EntityTopics::Climate(climate), service) => encode_climate(climate, service, None),
        (EntityTopics::WaterHeater(heater), service) => encode_water_heater(heater, service, None),
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
        (
            EntityTopics::Number {
                command_topic,
                command_template,
                ..
            },
            Service::NumberSetValue(data),
        ) => {
            // `120`, not `120.0`: the plain value, as a person would type it.
            let value = data.value.to_string();
            let value = value.strip_suffix(".0").unwrap_or(&value);
            Ok(vec![text_publish(
                command_topic,
                &command_template.render(value),
            )])
        }
        (
            EntityTopics::Select {
                command_topic,
                command_template,
                ..
            },
            Service::SelectSelectOption(data),
        ) => Ok(vec![text_publish(
            command_topic,
            &command_template.render(&data.option),
        )]),
        (
            EntityTopics::Text {
                command_topic,
                command_template,
                ..
            },
            Service::TextSetValue(data),
        ) => Ok(vec![text_publish(
            command_topic,
            &command_template.render(&data.value),
        )]),
        (
            EntityTopics::Button {
                command_topic,
                payload_press,
            },
            Service::ButtonPress,
        ) => Ok(vec![text_publish(command_topic, payload_press)]),
        (EntityTopics::Cover(cover), service) => encode_cover(cover, service),
        (EntityTopics::Fan(fan), service) => encode_fan(fan, service),
        (
            EntityTopics::Siren {
                command_topic,
                command_template,
                payload_off,
                ..
            },
            Service::SirenTurnOff,
        ) => Ok(vec![text_publish(
            command_topic,
            &command_template.render(payload_off),
        )]),
        (
            EntityTopics::Siren {
                command_topic,
                command_template,
                payload_on,
                ..
            },
            Service::SirenTurnOn(data),
        ) => {
            // Plain `ON`; told how, Home Assistant's JSON body with the state and what it was told.
            if *data == SirenTurnOn::default() {
                return Ok(vec![text_publish(
                    command_topic,
                    &command_template.render(payload_on),
                )]);
            }
            let mut body = serde_json::json!({ "state": payload_on });
            if let Some(tone) = &data.tone {
                body["tone"] = tone.clone().into();
            }
            if let Some(volume) = data.volume_level {
                body["volume_level"] = volume.into();
            }
            if let Some(duration) = data.duration {
                body["duration"] = duration.into();
            }
            Ok(vec![json_publish(command_topic, &body)])
        }
        (EntityTopics::Valve(valve), service) => {
            let as_cover = match service {
                Service::ValveOpen => Service::CoverOpen,
                Service::ValveClose => Service::CoverClose,
                Service::ValveStop => Service::CoverStop,
                Service::ValveSetPosition(data) => Service::CoverSetPosition(data.clone()),
                other => return Err(format!("a valve has no `{}` service", other.name())),
            };
            encode_cover(valve, &as_cover).map_err(|e| e.replace("cover", "valve"))
        }
        (EntityTopics::Lock(lock), service) => {
            let payload = match service {
                Service::LockLock(_) => &lock.payload_lock,
                Service::LockUnlock(_) => &lock.payload_unlock,
                Service::LockOpen(_) => lock
                    .payload_open
                    .as_ref()
                    .ok_or("this lock can't open the door")?,
                other => return Err(format!("a lock has no `{}` service", other.name())),
            };
            Ok(vec![text_publish(
                &lock.command_topic,
                &lock.command_template.render(payload),
            )])
        }
        _ => Err(format!("this entity has no `{}` service", service.name())),
    }
}

fn encode_fan(
    fan: &crate::discovery::FanTopics,
    service: &Service,
) -> Result<Vec<Publish>, String> {
    let send = |part: &crate::discovery::FanPart, value: &str| {
        text_publish(&part.command_topic, &part.command_template.render(value))
    };
    let speed = |percentage: u8| -> Result<Publish, String> {
        let part = fan.speed.as_ref().ok_or("this fan has no speeds to set")?;
        let count = u16::try_from(fan.speed_max - fan.speed_min + 1).unwrap_or(u16::MAX);
        let level = i64::from(percentage_to_speed(percentage, count)) + fan.speed_min - 1;
        Ok(send(part, &level.to_string()))
    };
    let preset = |mode: &str| -> Result<Publish, String> {
        let part = fan.preset.as_ref().ok_or("this fan has no preset modes")?;
        Ok(send(part, mode))
    };
    match service {
        Service::FanTurnOff | Service::FanSetPercentage(FanPercentage { percentage: 0 }) => {
            Ok(vec![send(&fan.power, &fan.payload_off)])
        }
        Service::FanTurnOn(data) => {
            let mut messages = vec![send(&fan.power, &fan.payload_on)];
            if let Some(percentage) = data.percentage {
                messages.push(speed(percentage)?);
            }
            if let Some(mode) = &data.preset_mode {
                messages.push(preset(mode)?);
            }
            Ok(messages)
        }
        Service::FanSetPercentage(data) => Ok(vec![speed(data.percentage)?]),
        Service::FanSetPresetMode(data) => Ok(vec![preset(&data.preset_mode)?]),
        Service::FanOscillate(data) => {
            let part = fan.oscillation.as_ref().ok_or("this fan doesn't swing")?;
            let payload = if data.oscillating {
                &fan.payload_oscillation_on
            } else {
                &fan.payload_oscillation_off
            };
            Ok(vec![send(part, payload)])
        }
        Service::FanSetDirection(data) => {
            let part = fan
                .direction
                .as_ref()
                .ok_or("this fan only turns one way")?;
            let direction = match data.direction {
                FanDirection::Forward => "forward",
                FanDirection::Reverse => "reverse",
            };
            Ok(vec![send(part, direction)])
        }
        other => Err(format!("a fan has no `{}` service", other.name())),
    }
}

fn encode_climate(
    climate: &ClimateTopics,
    service: &Service,
    last_on: Option<&State>,
) -> Result<Vec<Publish>, String> {
    let send = |part: &FanPart, value: &str| {
        text_publish(&part.command_topic, &part.command_template.render(value))
    };
    let mode = |mode: HvacMode| -> Result<Publish, String> {
        let part = climate
            .mode
            .as_ref()
            .ok_or("this thermostat's mode can't be set")?;
        Ok(send(part, mode.as_str()))
    };
    let temperature = |part: &Option<FanPart>, celsius: f64| -> Result<Publish, String> {
        let part = part
            .as_ref()
            .ok_or("this thermostat doesn't take that target")?;
        Ok(send(part, &device_temperature(climate.unit, celsius)))
    };
    let word = |part: &Option<FanPart>, value: &str, what: &str| -> Result<Vec<Publish>, String> {
        let part = part
            .as_ref()
            .ok_or_else(|| format!("this thermostat has no {what}"))?;
        Ok(vec![send(part, value)])
    };
    match service {
        Service::ClimateSetHvacMode(data) => Ok(vec![mode(data.hvac_mode)?]),
        Service::ClimateTurnOff => match &climate.power {
            Some(power) => Ok(vec![text_publish(
                &power.command_topic,
                &power.command_template.render(&power.payload_off),
            )]),
            None => Ok(vec![mode(HvacMode::Off)?]),
        },
        Service::ClimateTurnOn => match &climate.power {
            Some(power) => Ok(vec![text_publish(
                &power.command_topic,
                &power.command_template.render(&power.payload_on),
            )]),
            None => {
                let last = match last_on {
                    Some(State::Climate(state)) => Some(state.hvac_mode),
                    _ => None,
                };
                let on = last
                    .filter(|m| *m != HvacMode::Off && climate.modes.contains(m))
                    .or_else(|| climate.modes.iter().copied().find(|m| *m != HvacMode::Off))
                    .ok_or("this thermostat has no mode to turn on to")?;
                Ok(vec![mode(on)?])
            }
        },
        Service::ClimateSetTemperature(data) => {
            let mut messages = Vec::new();
            if let Some(hvac_mode) = data.hvac_mode {
                messages.push(mode(hvac_mode)?);
            }
            if let Some(value) = data.temperature {
                messages.push(temperature(&climate.temperature, value)?);
            }
            if let Some(low) = data.target_temp_low {
                messages.push(temperature(&climate.temperature_low, low)?);
            }
            if let Some(high) = data.target_temp_high {
                messages.push(temperature(&climate.temperature_high, high)?);
            }
            Ok(messages)
        }
        Service::ClimateSetHumidity(data) => word(
            &climate.target_humidity,
            &number_text(data.humidity),
            "target humidity",
        ),
        Service::ClimateSetFanMode(data) => word(&climate.fan_mode, &data.fan_mode, "fan modes"),
        Service::ClimateSetSwingMode(data) => {
            word(&climate.swing_mode, &data.swing_mode, "swing modes")
        }
        Service::ClimateSetPresetMode(data) => {
            word(&climate.preset_mode, &data.preset_mode, "presets")
        }
        other => Err(format!("a thermostat has no `{}` service", other.name())),
    }
}

fn encode_water_heater(
    heater: &WaterHeaterTopics,
    service: &Service,
    last_on: Option<&State>,
) -> Result<Vec<Publish>, String> {
    let send = |part: &FanPart, value: &str| {
        text_publish(&part.command_topic, &part.command_template.render(value))
    };
    let mode = |mode: WaterHeaterMode| -> Result<Publish, String> {
        let part = heater
            .mode
            .as_ref()
            .ok_or("this water heater's mode can't be set")?;
        Ok(send(part, mode.as_str()))
    };
    let power = |on: bool| {
        heater.power.as_ref().map(|power| {
            let payload = if on {
                &power.payload_on
            } else {
                &power.payload_off
            };
            text_publish(
                &power.command_topic,
                &power.command_template.render(payload),
            )
        })
    };
    match service {
        Service::WaterHeaterSetOperationMode(data) => Ok(vec![mode(data.operation_mode)?]),
        Service::WaterHeaterSetTemperature(data) => {
            let mut messages = Vec::new();
            if let Some(operation_mode) = data.operation_mode {
                messages.push(mode(operation_mode)?);
            }
            let part = heater
                .temperature
                .as_ref()
                .ok_or("this water heater doesn't take a target")?;
            messages.push(send(
                part,
                &device_temperature(heater.unit, data.temperature),
            ));
            Ok(messages)
        }
        Service::WaterHeaterTurnOff => match power(false) {
            Some(message) => Ok(vec![message]),
            None => Ok(vec![mode(WaterHeaterMode::Off)?]),
        },
        Service::WaterHeaterTurnOn => match power(true) {
            Some(message) => Ok(vec![message]),
            None => {
                let last = match last_on {
                    Some(State::WaterHeater(state)) => Some(state.operation_mode),
                    _ => None,
                };
                let on = last
                    .filter(|m| *m != WaterHeaterMode::Off && heater.modes.contains(m))
                    .or_else(|| {
                        heater
                            .modes
                            .iter()
                            .copied()
                            .find(|m| *m != WaterHeaterMode::Off)
                    })
                    .ok_or("this water heater has no mode to turn on to")?;
                Ok(vec![mode(on)?])
            }
        },
        other => Err(format!("a water heater has no `{}` service", other.name())),
    }
}

/// A °C temperature as the device writes it: to a tenth in °F or K, `21` rather than `21.0`.
fn device_temperature(unit: TemperatureUnit, celsius: f64) -> String {
    number_text(round_to(unit.from_celsius(celsius), 1))
}

fn number_text(value: f64) -> String {
    let text = value.to_string();
    text.strip_suffix(".0").map_or(text.clone(), str::to_owned)
}

fn encode_cover(
    cover: &crate::discovery::CoverTopics,
    service: &Service,
) -> Result<Vec<Publish>, String> {
    let scale = |percent: u8, closed: f64, open: f64| {
        let raw = closed + f64::from(percent) / 100.0 * (open - closed);
        let raw = raw.round().to_string();
        raw.strip_suffix(".0").map_or(raw.clone(), str::to_owned)
    };
    let command = |payload: &str| match &cover.command_topic {
        Some(topic) => Ok(vec![text_publish(topic, payload)]),
        None => Err("this cover takes no commands".to_owned()),
    };
    match service {
        Service::CoverOpen => command(&cover.payload_open),
        Service::CoverClose => command(&cover.payload_close),
        Service::CoverStop => match &cover.payload_stop {
            Some(stop) => command(stop),
            None => Err("this cover can't be stopped".to_owned()),
        },
        Service::CoverSetPosition(data) => match &cover.set_position_topic {
            Some(topic) => Ok(vec![text_publish(
                topic,
                &cover.set_position_template.render(&scale(
                    data.position,
                    cover.position_closed,
                    cover.position_open,
                )),
            )]),
            None => Err("this cover can't go to a position".to_owned()),
        },
        Service::CoverSetTilt(data) => match &cover.tilt_command_topic {
            Some(topic) => Ok(vec![text_publish(
                topic,
                &cover.tilt_command_template.render(&scale(
                    data.tilt,
                    cover.tilt_min,
                    cover.tilt_max,
                )),
            )]),
            None => Err("this cover has nothing to tilt".to_owned()),
        },
        other => Err(format!("a cover has no `{}` service", other.name())),
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
        EntityTopics::Number { state_topic, .. }
        | EntityTopics::Select { state_topic, .. }
        | EntityTopics::Text { state_topic, .. } => {
            list.extend(state_topic.clone());
        }
        // Nothing to listen to: a press leaves no state.
        EntityTopics::Button { .. } => {}
        EntityTopics::Event { state_topic, .. } => list.push(state_topic.clone()),
        EntityTopics::Lock(lock) => list.extend(lock.state_topic.clone()),
        EntityTopics::Siren { state_topic, .. } => list.extend(state_topic.clone()),
        EntityTopics::WaterHeater(heater) => {
            for part in [&heater.mode, &heater.temperature].into_iter().flatten() {
                list.extend(part.state_topic.clone());
            }
            list.extend(heater.current_temperature.as_ref().map(|(t, _)| t.clone()));
        }
        EntityTopics::Climate(climate) => {
            for (_, part) in climate.settings() {
                list.extend(part.and_then(|part| part.state_topic.clone()));
            }
            for (reading, _) in [
                &climate.current_temperature,
                &climate.current_humidity,
                &climate.action,
            ]
            .into_iter()
            .flatten()
            {
                list.push(reading.clone());
            }
        }
        EntityTopics::Fan(fan) => {
            list.extend(fan.power.state_topic.clone());
            for part in [&fan.speed, &fan.preset, &fan.oscillation, &fan.direction]
                .into_iter()
                .flatten()
            {
                list.extend(part.state_topic.clone());
            }
        }
        EntityTopics::Cover(cover) | EntityTopics::Valve(cover) => {
            list.extend(cover.state_topic.clone());
            list.extend(cover.position_topic.clone());
            list.extend(cover.tilt_status_topic.clone());
        }
    }
    // A cover's state and position usually share one topic; it's listened to once.
    list.sort();
    list.dedup();
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
