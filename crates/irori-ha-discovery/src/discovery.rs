//! Parsing an HA MQTT Discovery config payload into what Irori needs: a device, an entity's
//! capabilities, and the topics/schema to read and write it through.
//!
//! Deliberately permissive: real payloads (Z2M, Tasmota, ESPHome-over-MQTT) carry many fields
//! Irori doesn't use, and HA itself tolerates most of them being absent. This reads what it
//! needs from a `serde_json::Value` rather than a strict typed struct, so an unrelated or
//! unexpected field never fails the whole entity — only a field this parser actually depends on
//! being unusable does that, and always with a reason a person could act on.

use irori_types::{
    BinarySensorCapabilities, BinarySensorClass, Capabilities, ColorTempRange, LightCapabilities,
    Name, SensorCapabilities, SensorClass, SensorValueType, StateClass, SwitchCapabilities,
    SwitchClass, UniqueId,
};

use crate::template::ValueTemplate;
use crate::topic::Component;

/// A device as HA discovery describes it. `unique_id` is the first of `device.identifiers`,
/// HA's own permanent handle for the device (ROADMAP D31's same intent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDevice {
    pub unique_id: UniqueId,
    pub name: Name,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub sw_version: Option<String>,
    pub hw_version: Option<String>,
    pub via_device_unique_id: Option<UniqueId>,
}

/// One topic to check an entity's (or its device's) reachability against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailabilityTopic {
    pub topic: String,
    pub payload_available: String,
    pub payload_not_available: String,
}

/// One entity listening on an availability topic, with the words *it* reads as online and
/// offline.
///
/// The pair belongs to the listener rather than to the topic: Home Assistant lets two entities
/// share an availability topic and still disagree about its payloads (Tasmota's `Online`/`Offline`
/// beside a default `online`/`offline`), and keeping one pair per topic applied whichever entity
/// happened to be indexed first to all of them — marking some of them the wrong way round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    pub unique_id: UniqueId,
    pub payload_available: String,
    pub payload_not_available: String,
}

/// Which of `listeners` this payload makes available, and which unavailable, each judged by its
/// own words. A listener whose words the payload matches neither of is in neither list: the
/// message said nothing about it, which is not the same as saying it's offline.
pub fn resolve(listeners: &[Listener], payload: &str) -> (Vec<UniqueId>, Vec<UniqueId>) {
    let mut available = Vec::new();
    let mut unavailable = Vec::new();
    for listener in listeners {
        if payload == listener.payload_available {
            available.push(listener.unique_id.clone());
        } else if payload == listener.payload_not_available {
            unavailable.push(listener.unique_id.clone());
        }
    }
    (available, unavailable)
}

/// The topics and wire schema for one entity, once its kind is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityTopics {
    /// Z2M's `schema: "json"`: one topic each way, JSON body
    /// `{state, brightness, color_temp, color: {r,g,b}}`.
    LightJson {
        state_topic: String,
        command_topic: String,
    },
    /// The default/plain schema (Tasmota): separate topics per feature, plain-text payloads.
    LightDefault {
        state_topic: Option<String>,
        command_topic: String,
        payload_on: String,
        payload_off: String,
        brightness_state_topic: Option<String>,
        brightness_command_topic: Option<String>,
        /// The scale brightness is published/commanded in, e.g. Tasmota's 100. Irori's own
        /// scale is 1-255; conversion happens in `state.rs`.
        brightness_scale: u32,
    },
    Switch {
        state_topic: Option<String>,
        command_topic: String,
        payload_on: String,
        payload_off: String,
    },
    Sensor {
        state_topic: String,
        value_template: ValueTemplate,
    },
    BinarySensor {
        state_topic: String,
        payload_on: String,
        payload_off: String,
    },
}

/// Everything Irori needs from one discovery config payload.
///
/// No `Eq`: `Capabilities` (from `irori-types`) doesn't derive it.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedConfig {
    pub unique_id: UniqueId,
    /// `None` when the entity is a device's main feature and takes the device's name
    /// (`docs/specs/protocols.md` §6.2).
    pub name: Option<Name>,
    pub device: Option<ParsedDevice>,
    pub capabilities: Capabilities,
    pub topics: EntityTopics,
    pub availability: Vec<AvailabilityTopic>,
}

/// Parses one discovery payload. `Err` names what made it unusable — logged and skipped, never
/// a reason to fail every other entity a protocol has already found.
pub fn parse(component: Component, payload: &[u8]) -> Result<ParsedConfig, String> {
    let root: serde_json::Value =
        serde_json::from_slice(payload).map_err(|e| format!("payload isn't JSON: {e}"))?;

    let unique_id = str_field(&root, "unique_id")
        .ok_or("no `unique_id`; nothing safe to key this entity by")?;
    let unique_id =
        UniqueId::try_from(unique_id).map_err(|e| format!("`unique_id` isn't usable: {e}"))?;

    let device = parse_device(&root)?;
    let name = str_field(&root, "name")
        .map(Name::try_from)
        .transpose()
        .map_err(|e| format!("`name` isn't usable: {e}"))?;
    if name.is_none() && device.is_none() {
        return Err("no `name` and no `device`; nothing to call this entity".to_owned());
    }

    let availability = parse_availability(&root);
    let (capabilities, topics) = match component {
        Component::Light => parse_light(&root)?,
        Component::Switch => parse_switch(&root)?,
        Component::Sensor => parse_sensor(&root)?,
        Component::BinarySensor => parse_binary_sensor(&root)?,
    };

    Ok(ParsedConfig {
        unique_id,
        name,
        device,
        capabilities,
        topics,
        availability,
    })
}

fn str_field<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(serde_json::Value::as_str)
}

fn bool_field(value: &serde_json::Value, key: &str) -> bool {
    value.get(key).and_then(serde_json::Value::as_bool) == Some(true)
}

fn owned_str(value: &serde_json::Value, key: &str, default: &str) -> String {
    str_field(value, key).unwrap_or(default).to_owned()
}

fn parse_device(root: &serde_json::Value) -> Result<Option<ParsedDevice>, String> {
    let Some(device) = root.get("device") else {
        return Ok(None);
    };
    // HA allows `identifiers` as a single string or a list; the first entry is the permanent
    // handle either way.
    let first_identifier = match device.get("identifiers") {
        Some(serde_json::Value::String(s)) => Some(s.as_str()),
        Some(serde_json::Value::Array(items)) => items.first().and_then(serde_json::Value::as_str),
        _ => None,
    };
    let Some(identifier) = first_identifier else {
        return Ok(None); // no identifiers at all: treat the entity as deviceless
    };
    let unique_id = UniqueId::try_from(identifier)
        .map_err(|e| format!("device `identifiers` isn't usable: {e}"))?;
    let name = str_field(device, "name")
        .map(Name::try_from)
        .transpose()
        .map_err(|e| format!("device `name` isn't usable: {e}"))?
        .unwrap_or_else(|| Name::try_from(identifier).unwrap_or_else(|_| unnamed_device()));
    let via_device_unique_id = str_field(device, "via_device")
        .map(UniqueId::try_from)
        .transpose()
        .map_err(|e| format!("device `via_device` isn't usable: {e}"))?;
    Ok(Some(ParsedDevice {
        unique_id,
        name,
        manufacturer: str_field(device, "manufacturer").map(str::to_owned),
        model: str_field(device, "model").map(str::to_owned),
        sw_version: str_field(device, "sw_version").map(str::to_owned),
        hw_version: str_field(device, "hw_version").map(str::to_owned),
        via_device_unique_id,
    }))
}

/// `Name` rejects empty text; a device identified only by an id `Name` also rejects (control
/// characters, say) falls back to this rather than failing the whole device over a cosmetic
/// field.
fn unnamed_device() -> Name {
    Name::try_from("Unnamed device").expect("a fixed, valid name")
}

/// Every availability topic an entity lists, with the payload words it reads each one by.
///
/// **Known limit: `availability_mode` is ignored, and every topic is treated as `any`.** Home
/// Assistant also defines `all` (every topic must say online) and `latest` (only the newest
/// message counts); honouring those means remembering each topic's last word per entity, which
/// nothing here does yet. Where an entity lists one availability topic — which is every entity
/// Zigbee2MQTT and Tasmota produce — the three modes agree, so this is a gap for hand-written
/// discovery configs rather than for anything Irori talks to today. Under `all`, one topic saying
/// online will mark the entity available while another still says offline.
fn parse_availability(root: &serde_json::Value) -> Vec<AvailabilityTopic> {
    let default_on = || owned_str(root, "payload_available", "online");
    let default_off = || owned_str(root, "payload_not_available", "offline");
    if let Some(list) = root
        .get("availability")
        .and_then(serde_json::Value::as_array)
    {
        return list
            .iter()
            .filter_map(|item| {
                let topic = str_field(item, "topic")?.to_owned();
                Some(AvailabilityTopic {
                    payload_available: owned_str(item, "payload_available", "online"),
                    payload_not_available: owned_str(item, "payload_not_available", "offline"),
                    topic,
                })
            })
            .collect();
    }
    match str_field(root, "availability_topic") {
        Some(topic) => vec![AvailabilityTopic {
            topic: topic.to_owned(),
            payload_available: default_on(),
            payload_not_available: default_off(),
        }],
        None => Vec::new(),
    }
}

fn parse_light(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
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
        EntityTopics::LightJson {
            state_topic: owned_str(root, "state_topic", &command_topic),
            command_topic,
        }
    } else {
        EntityTopics::LightDefault {
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
    Ok((capabilities, topics))
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

fn parse_switch(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let device_class = str_field(root, "device_class").and_then(switch_class);
    let command_topic = str_field(root, "command_topic")
        .ok_or("a switch needs a `command_topic`")?
        .to_owned();
    Ok((
        Capabilities::Switch(SwitchCapabilities { device_class }),
        EntityTopics::Switch {
            state_topic: str_field(root, "state_topic").map(str::to_owned),
            command_topic,
            payload_on: owned_str(root, "payload_on", "ON"),
            payload_off: owned_str(root, "payload_off", "OFF"),
        },
    ))
}

fn parse_sensor(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let state_topic = str_field(root, "state_topic")
        .ok_or("a sensor needs a `state_topic`")?
        .to_owned();
    let device_class = str_field(root, "device_class").and_then(sensor_class);
    let state_class = str_field(root, "state_class").and_then(|s| match s {
        "measurement" => Some(StateClass::Measurement),
        "total" => Some(StateClass::Total),
        "total_increasing" => Some(StateClass::TotalIncreasing),
        _ => None,
    });
    // A unit at all is the strongest signal this is a number, not text (HA has no separate
    // "this sensor is numeric" flag) — Tasmota/Z2M numeric sensors always carry one.
    let value_type = if str_field(root, "unit_of_measurement").is_some() || device_class.is_some() {
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
        }),
        EntityTopics::Sensor {
            state_topic,
            value_template: ValueTemplate::parse(str_field(root, "value_template")),
        },
    ))
}

fn parse_binary_sensor(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let state_topic = str_field(root, "state_topic")
        .ok_or("a binary_sensor needs a `state_topic`")?
        .to_owned();
    Ok((
        Capabilities::BinarySensor(BinarySensorCapabilities {
            device_class: str_field(root, "device_class").and_then(binary_sensor_class),
        }),
        EntityTopics::BinarySensor {
            state_topic,
            payload_on: owned_str(root, "payload_on", "ON"),
            payload_off: owned_str(root, "payload_off", "OFF"),
        },
    ))
}

fn switch_class(text: &str) -> Option<SwitchClass> {
    match text {
        "outlet" => Some(SwitchClass::Outlet),
        "switch" => Some(SwitchClass::Switch),
        _ => None,
    }
}

/// HA's own device-class strings, mapped to Irori's closed list (`docs/specs/entities.md` §4.4).
/// Anything HA has that Irori doesn't is left absent, never an error (`docs/specs/entities.md`:
/// "a protocol maps what it knows and leaves the rest absent").
fn sensor_class(text: &str) -> Option<SensorClass> {
    match text {
        "temperature" => Some(SensorClass::Temperature),
        "humidity" => Some(SensorClass::Humidity),
        "illuminance" => Some(SensorClass::Illuminance),
        "pressure" | "atmospheric_pressure" => Some(SensorClass::Pressure),
        "power" => Some(SensorClass::Power),
        "energy" => Some(SensorClass::Energy),
        "voltage" => Some(SensorClass::Voltage),
        "current" => Some(SensorClass::Current),
        "battery" => Some(SensorClass::Battery),
        // HA's string is `carbon_dioxide`, not `co2` — Irori's variant is named for the gas, not
        // HA's exact spelling.
        "carbon_dioxide" => Some(SensorClass::Co2),
        "pm25" => Some(SensorClass::Pm25),
        "signal_strength" => Some(SensorClass::SignalStrength),
        "distance" => Some(SensorClass::Distance),
        _ => None,
    }
}

fn binary_sensor_class(text: &str) -> Option<BinarySensorClass> {
    match text {
        "motion" => Some(BinarySensorClass::Motion),
        "occupancy" => Some(BinarySensorClass::Occupancy),
        "door" => Some(BinarySensorClass::Door),
        "window" => Some(BinarySensorClass::Window),
        "moisture" => Some(BinarySensorClass::Moisture),
        "smoke" => Some(BinarySensorClass::Smoke),
        "gas" => Some(BinarySensorClass::Gas),
        "vibration" => Some(BinarySensorClass::Vibration),
        "plug" => Some(BinarySensorClass::Plug),
        "connectivity" => Some(BinarySensorClass::Connectivity),
        "problem" => Some(BinarySensorClass::Problem),
        "battery" => Some(BinarySensorClass::Battery),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn two_entities_sharing_a_topic_are_each_read_by_their_own_words() {
        use super::{Listener, resolve};

        let uid = |s: &str| irori_types::UniqueId::try_from(s).expect("valid");
        let listeners = vec![
            // Tasmota's capitalised pair, and the HA default, on one topic.
            Listener {
                unique_id: uid("tasmota"),
                payload_available: "Online".into(),
                payload_not_available: "Offline".into(),
            },
            Listener {
                unique_id: uid("plain"),
                payload_available: "online".into(),
                payload_not_available: "offline".into(),
            },
        ];

        let (available, unavailable) = resolve(&listeners, "Online");
        assert_eq!(available, vec![uid("tasmota")]);
        assert!(
            unavailable.is_empty(),
            "`Online` says nothing about an entity reading `online`/`offline`, and \
             certainly not that it is offline"
        );

        let (available, unavailable) = resolve(&listeners, "offline");
        assert!(available.is_empty());
        assert_eq!(unavailable, vec![uid("plain")]);

        let (available, unavailable) = resolve(&listeners, "something else entirely");
        assert!(available.is_empty() && unavailable.is_empty());
    }

    use super::*;

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

    /// A Tasmota-shaped switch: default schema, plain on/off.
    const TASMOTA_SWITCH: &str = r#"{
        "unique_id": "tasmota_ABC123_switch",
        "name": "Garage plug",
        "device_class": "outlet",
        "state_topic": "tasmota/garage/POWER",
        "command_topic": "tasmota/garage/cmnd/POWER",
        "payload_on": "ON",
        "payload_off": "OFF",
        "availability_topic": "tasmota/garage/LWT",
        "payload_available": "Online",
        "payload_not_available": "Offline"
    }"#;

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
        assert!(matches!(parsed.topics, EntityTopics::LightJson { .. }));
        assert_eq!(parsed.availability.len(), 1);
        assert_eq!(parsed.availability[0].payload_available, "online");
    }

    #[test]
    fn parses_a_tasmota_switch_with_custom_availability_payloads() {
        let parsed = parse(Component::Switch, TASMOTA_SWITCH.as_bytes()).expect("valid");
        assert_eq!(
            parsed.name.as_ref().map(|n| n.as_str()),
            Some("Garage plug")
        );
        assert!(parsed.device.is_none(), "deviceless: no `device` block");
        let Capabilities::Switch(caps) = parsed.capabilities else {
            panic!("expected switch capabilities")
        };
        assert_eq!(caps.device_class, Some(SwitchClass::Outlet));
        assert_eq!(parsed.availability[0].payload_available, "Online");
        assert_eq!(parsed.availability[0].payload_not_available, "Offline");
    }

    #[test]
    fn parses_a_sensor_with_a_nested_value_template_and_maps_device_class() {
        let parsed = parse(Component::Sensor, Z2M_SENSOR.as_bytes()).expect("valid");
        let Capabilities::Sensor(caps) = parsed.capabilities else {
            panic!("expected sensor capabilities")
        };
        assert_eq!(caps.device_class, Some(SensorClass::Power));
        assert_eq!(caps.unit.as_deref(), Some("W"));
        let EntityTopics::Sensor { value_template, .. } = parsed.topics else {
            panic!("expected sensor topics")
        };
        assert_eq!(
            value_template,
            ValueTemplate::JsonPath(vec!["power".to_owned()])
        );
    }

    #[test]
    fn a_missing_unique_id_is_rejected_with_a_clear_reason() {
        let error = parse(Component::Switch, br#"{"name": "x", "command_topic": "t"}"#)
            .expect_err("no unique_id");
        assert!(error.contains("unique_id"));
    }

    #[test]
    fn ha_carbon_dioxide_maps_to_irori_co2() {
        assert_eq!(sensor_class("carbon_dioxide"), Some(SensorClass::Co2));
        assert_eq!(
            sensor_class("co2"),
            None,
            "HA never actually sends this spelling"
        );
    }
}
