//! Parsing an HA MQTT Discovery config payload into what Irori needs: a device, an entity's
//! capabilities, and the topics/schema to read and write it through.
//!
//! Deliberately permissive: real payloads (Z2M, Tasmota, ESPHome-over-MQTT) carry many fields
//! Irori doesn't use, and HA itself tolerates most of them being absent. This reads what it
//! needs from a `serde_json::Value` rather than a strict typed struct, so an unrelated or
//! unexpected field never fails the whole entity — only a field this parser actually depends on
//! being unusable does that, and always with a reason a person could act on.

use irori_types::{
    BinarySensorCapabilities, BinarySensorClass, ButtonCapabilities, ButtonClass, Capabilities,
    ColorTempRange, CoverCapabilities, CoverClass, EntityCategory, EventCapabilities, EventClass,
    FanCapabilities, LightCapabilities, LockCapabilities, LockStatus, Name, NumberCapabilities,
    NumberMode, SelectCapabilities, SensorCapabilities, SensorClass, SensorValueType, StateClass,
    SwitchCapabilities, SwitchClass, TextCapabilities, TextMode, UniqueId, ValveCapabilities,
    ValveClass,
};

use crate::template::{CommandTemplate, ValueTemplate};
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
#[derive(Debug, Clone, PartialEq)]
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
    /// A value set within a range: the plain value is published to `command_topic` (what
    /// Zigbee2MQTT's `<device>/set/<property>` takes), and read back through `value_template`.
    Number {
        state_topic: Option<String>,
        command_topic: String,
        command_template: CommandTemplate,
        value_template: ValueTemplate,
    },
    /// One choice out of a list: the option itself is published to `command_topic`, and read back
    /// through `value_template`.
    Select {
        state_topic: Option<String>,
        command_topic: String,
        command_template: CommandTemplate,
        value_template: ValueTemplate,
    },
    /// A piece of text: published as it is, read back through `value_template`.
    Text {
        state_topic: Option<String>,
        command_topic: String,
        command_template: CommandTemplate,
        value_template: ValueTemplate,
    },
    /// Something to press: `payload_press` is published, and nothing is ever read back.
    Button {
        command_topic: String,
        payload_press: String,
    },
    /// Something that happens: each message on `state_topic` that names an event type is one
    /// happening.
    Event {
        state_topic: String,
        source: EventSource,
    },
    /// Something that opens and closes. Boxed: Home Assistant lets nearly every part of it vary.
    Cover(Box<CoverTopics>),
    /// A lock, as Home Assistant's MQTT lock lets it vary.
    Lock(Box<LockTopics>),
    /// A fan, as Home Assistant's MQTT fan lets it vary.
    Fan(Box<FanTopics>),
    /// A valve: a cover with fewer settings, read and sent the same way.
    Valve(Box<CoverTopics>),
}

/// One setting of a fan: where it's sent and how, and where it's read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanPart {
    pub command_topic: String,
    pub command_template: CommandTemplate,
    pub state_topic: Option<String>,
    pub value_template: ValueTemplate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanTopics {
    /// On and off.
    pub power: FanPart,
    pub payload_on: String,
    pub payload_off: String,
    /// Its speed, as the device numbers it from `speed_min` to `speed_max`.
    pub speed: Option<FanPart>,
    pub speed_min: i64,
    pub speed_max: i64,
    pub preset: Option<FanPart>,
    pub oscillation: Option<FanPart>,
    pub payload_oscillation_on: String,
    pub payload_oscillation_off: String,
    pub direction: Option<FanPart>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockTopics {
    pub command_topic: String,
    pub command_template: CommandTemplate,
    pub payload_lock: String,
    pub payload_unlock: String,
    /// `None` when it can't open the door.
    pub payload_open: Option<String>,
    pub state_topic: Option<String>,
    pub value_template: ValueTemplate,
    /// What it says for each state, in Irori's order: locked, unlocked, locking, unlocking,
    /// jammed, open, opening.
    pub said: [(String, LockStatus); 7],
}

/// How a cover is told what to do and says where it is, all as Home Assistant's MQTT cover lets
/// it vary. A state can come in on any of its topics, which are often the same one.
#[derive(Debug, Clone, PartialEq)]
pub struct CoverTopics {
    pub command_topic: Option<String>,
    pub payload_open: String,
    pub payload_close: String,
    /// `None` when it can't be stopped.
    pub payload_stop: Option<String>,
    pub state_topic: Option<String>,
    /// `None` when its template needs Jinja: the state is then worked out from the position.
    pub value_template: Option<ValueTemplate>,
    pub state_open: String,
    pub state_opening: String,
    pub state_closed: String,
    pub state_closing: String,
    /// Stopped somewhere: open, unless the position says it's at the closed end.
    pub state_stopped: String,
    pub position_topic: Option<String>,
    pub position_template: ValueTemplate,
    /// The device's own numbers for fully open and fully closed; Irori's are 100 and 0.
    pub position_open: f64,
    pub position_closed: f64,
    pub set_position_topic: Option<String>,
    pub set_position_template: CommandTemplate,
    pub tilt_command_topic: Option<String>,
    pub tilt_command_template: CommandTemplate,
    pub tilt_status_topic: Option<String>,
    pub tilt_status_template: ValueTemplate,
    pub tilt_min: f64,
    pub tilt_max: f64,
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
    /// HA's `entity_category`: one of the device's settings or diagnostics.
    pub entity_category: Option<EntityCategory>,
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
        Component::Number => parse_number(&root)?,
        Component::Select => parse_select(&root)?,
        Component::Text => parse_text(&root)?,
        Component::Button => parse_button(&root)?,
        Component::Event => parse_event(&root)?,
        Component::Cover => parse_cover(&root)?,
        Component::Lock => parse_lock(&root)?,
        Component::Fan => parse_fan(&root)?,
        Component::Valve => parse_valve(&root)?,
    };

    Ok(ParsedConfig {
        unique_id,
        name,
        device,
        capabilities,
        // Same names as Irori's; anything else is left out.
        entity_category: str_field(&root, "entity_category").and_then(|c| match c {
            "config" => Some(EntityCategory::Config),
            "diagnostic" => Some(EntityCategory::Diagnostic),
            _ => None,
        }),
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

pub(crate) fn parse_device(root: &serde_json::Value) -> Result<Option<ParsedDevice>, String> {
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

fn parse_number(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
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
        EntityTopics::Number {
            state_topic: str_field(root, "state_topic").map(str::to_owned),
            command_topic,
            command_template,
            value_template: ValueTemplate::parse(str_field(root, "value_template")),
        },
    ))
}

/// The `command_topic` of an entity whose command is its value, and how to put the value in.
///
/// A `command_template` that does more than place the value would need Jinja, which Irori doesn't
/// run (`docs/specs/protocols.md`): an entity that needs one is better refused than sent the
/// wrong thing.
fn plain_command(
    root: &serde_json::Value,
    what: &str,
) -> Result<(String, CommandTemplate), String> {
    let command_topic = str_field(root, "command_topic")
        .ok_or_else(|| format!("{what} needs a `command_topic`"))?
        .to_owned();
    let template = CommandTemplate::parse(str_field(root, "command_template"), "value")
        .map_err(|why| format!("`command_template`: {why}"))?;
    Ok((command_topic, template))
}

fn parse_select(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let (command_topic, command_template) = plain_command(root, "a select")?;
    let options = root
        .get("options")
        .and_then(serde_json::Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|o| o.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let capabilities = SelectCapabilities { options };
    capabilities.validate().map_err(|e| e.to_string())?;
    Ok((
        Capabilities::Select(capabilities),
        EntityTopics::Select {
            state_topic: str_field(root, "state_topic").map(str::to_owned),
            command_topic,
            command_template,
            value_template: ValueTemplate::parse(str_field(root, "value_template")),
        },
    ))
}

/// Home Assistant's MQTT valve. One that `reports_position` says a number on its state topic and
/// takes one on its command topic; otherwise it opens and closes like a cover.
fn parse_valve(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let (command_topic, command_template) = plain_command(root, "a valve")?;
    let owned = |key: &str| str_field(root, key).map(str::to_owned);
    let number = |key: &str, default: f64| {
        root.get(key)
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(default)
    };
    let reports_position = bool_field(root, "reports_position");
    let state_topic = owned("state_topic");
    let value_template = ValueTemplate::parse(str_field(root, "value_template"));
    let topics = CoverTopics {
        command_topic: Some(command_topic.clone()),
        payload_open: owned_str(root, "payload_open", "OPEN"),
        payload_close: owned_str(root, "payload_close", "CLOSE"),
        // A valve stops only when it says how.
        payload_stop: owned("payload_stop"),
        state_topic: state_topic.clone(),
        value_template: (!reports_position).then(|| value_template.clone()),
        state_open: owned_str(root, "state_open", "open"),
        state_opening: owned_str(root, "state_opening", "opening"),
        state_closed: owned_str(root, "state_closed", "closed"),
        state_closing: owned_str(root, "state_closing", "closing"),
        state_stopped: "stopped".to_owned(),
        position_topic: reports_position.then(|| state_topic.clone()).flatten(),
        position_template: value_template,
        position_open: number("position_open", 100.0),
        position_closed: number("position_closed", 0.0),
        set_position_topic: reports_position.then_some(command_topic),
        set_position_template: command_template,
        tilt_command_topic: None,
        tilt_command_template: CommandTemplate::Value,
        tilt_status_topic: None,
        tilt_status_template: ValueTemplate::None,
        tilt_min: 0.0,
        tilt_max: 100.0,
    };
    let capabilities = ValveCapabilities {
        device_class: str_field(root, "device_class").and_then(ValveClass::from_ha),
        position: reports_position,
        stop: topics.payload_stop.is_some(),
    };
    Ok((
        Capabilities::Valve(capabilities),
        EntityTopics::Valve(Box::new(topics)),
    ))
}

fn parse_fan(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let (command_topic, command_template) = plain_command(root, "a fan")?;
    // One optional setting, `<name>_command_topic` and friends. A command template Irori can't
    // render leaves that setting out, rather than the whole fan: on and off still work.
    let part = |name: &str, value_key: &str| -> Option<FanPart> {
        let command_topic = str_field(root, &format!("{name}_command_topic"))?.to_owned();
        let command_template = match CommandTemplate::parse(
            str_field(root, &format!("{name}_command_template")),
            "value",
        ) {
            Ok(template) => template,
            // The fan still works without this setting; the rest of it is described.
            Err(_) => return None,
        };
        let template = str_field(root, value_key);
        let value_template = match ValueTemplate::parse(template) {
            // Only presets are read this way: a value outside the list is dropped anyway.
            ValueTemplate::Unsupported(text) if name == "preset_mode" => {
                ValueTemplate::first_path(&text).unwrap_or(ValueTemplate::Unsupported(text))
            }
            other => other,
        };
        Some(FanPart {
            command_topic,
            command_template,
            state_topic: str_field(root, &format!("{name}_state_topic")).map(str::to_owned),
            value_template,
        })
    };
    let speed_min = root
        .get("speed_range_min")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(1);
    let speed_max = root
        .get("speed_range_max")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(100);
    let speed = part("percentage", "percentage_value_template");
    let preset = part("preset_mode", "preset_mode_value_template");
    let preset_modes: Vec<String> = if preset.is_some() {
        root.get("preset_modes")
            .and_then(serde_json::Value::as_array)
            .map(|modes| {
                modes
                    .iter()
                    .filter_map(|m| m.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let topics = FanTopics {
        power: FanPart {
            command_topic,
            command_template,
            state_topic: str_field(root, "state_topic").map(str::to_owned),
            value_template: ValueTemplate::parse(str_field(root, "state_value_template")),
        },
        payload_on: owned_str(root, "payload_on", "ON"),
        payload_off: owned_str(root, "payload_off", "OFF"),
        speed_min,
        speed_max,
        oscillation: part("oscillation", "oscillation_value_template"),
        payload_oscillation_on: owned_str(root, "payload_oscillation_on", "oscillate_on"),
        payload_oscillation_off: owned_str(root, "payload_oscillation_off", "oscillate_off"),
        direction: part("direction", "direction_value_template"),
        speed,
        preset,
    };
    let capabilities = FanCapabilities {
        speed_count: if topics.speed.is_some() && speed_max >= speed_min {
            u16::try_from(speed_max - speed_min + 1).unwrap_or(u16::MAX)
        } else {
            0
        },
        oscillate: topics.oscillation.is_some(),
        direction: topics.direction.is_some(),
        preset_modes,
    };
    capabilities.validate().map_err(|e| e.to_string())?;
    Ok((
        Capabilities::Fan(capabilities),
        EntityTopics::Fan(Box::new(topics)),
    ))
}

fn parse_lock(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let (command_topic, command_template) = plain_command(root, "a lock")?;
    let word = |key: &str, default: &str| owned_str(root, key, default);
    let payload_open = str_field(root, "payload_open").map(str::to_owned);
    let topics = LockTopics {
        command_topic,
        command_template,
        payload_lock: word("payload_lock", "LOCK"),
        payload_unlock: word("payload_unlock", "UNLOCK"),
        payload_open: payload_open.clone(),
        state_topic: str_field(root, "state_topic").map(str::to_owned),
        value_template: ValueTemplate::parse(str_field(root, "value_template")),
        said: [
            (word("state_locked", "LOCKED"), LockStatus::Locked),
            (word("state_unlocked", "UNLOCKED"), LockStatus::Unlocked),
            (word("state_locking", "LOCKING"), LockStatus::Locking),
            (word("state_unlocking", "UNLOCKING"), LockStatus::Unlocking),
            (word("state_jammed", "JAMMED"), LockStatus::Jammed),
            (word("state_open", "OPEN"), LockStatus::Open),
            (word("state_opening", "OPENING"), LockStatus::Opening),
        ],
    };
    // Home Assistant checks an MQTT lock's `code_format` itself and never sends the code to the
    // device. Irori has no regular expressions to check it with, so it's kept for pages to show
    // and not demanded; the page asks before unlocking either way.
    let capabilities = LockCapabilities {
        open: payload_open.is_some(),
        requires_code: false,
        code_format: str_field(root, "code_format").map(str::to_owned),
    };
    Ok((
        Capabilities::Lock(capabilities),
        EntityTopics::Lock(Box::new(topics)),
    ))
}

fn parse_cover(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let owned = |key: &str| str_field(root, key).map(str::to_owned);
    let number = |key: &str, default: f64| {
        root.get(key)
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(default)
    };
    let template = |key: &str, variable: &str| {
        CommandTemplate::parse(str_field(root, key), variable)
            .map_err(|why| format!("`{key}`: {why}"))
    };
    let state_topic = owned("state_topic");
    // A state template that needs Jinja (Zigbee2MQTT's for covers that report their motor) isn't
    // run: the state is worked out from the position instead, if it has one.
    let value_template = match ValueTemplate::parse(str_field(root, "value_template")) {
        ValueTemplate::Unsupported(_) => None,
        template => Some(template),
    };
    let position_topic = owned("position_topic");
    if state_topic.is_some() && value_template.is_none() && position_topic.is_none() {
        return Err("its state template needs Jinja, and it has no position to go by".to_owned());
    }
    // `payload_stop: null` says it can't be stopped.
    let payload_stop = match root.get("payload_stop") {
        Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(stop)) => Some(stop.clone()),
        _ => Some("STOP".to_owned()),
    };
    let command_topic = owned("command_topic");
    let topics = CoverTopics {
        payload_open: owned("payload_open").unwrap_or_else(|| "OPEN".to_owned()),
        payload_close: owned("payload_close").unwrap_or_else(|| "CLOSE".to_owned()),
        payload_stop: payload_stop.filter(|_| command_topic.is_some()),
        command_topic,
        state_topic,
        value_template,
        state_open: owned("state_open").unwrap_or_else(|| "open".to_owned()),
        state_opening: owned("state_opening").unwrap_or_else(|| "opening".to_owned()),
        state_closed: owned("state_closed").unwrap_or_else(|| "closed".to_owned()),
        state_closing: owned("state_closing").unwrap_or_else(|| "closing".to_owned()),
        state_stopped: owned("state_stopped").unwrap_or_else(|| "stopped".to_owned()),
        position_template: ValueTemplate::parse(str_field(root, "position_template")),
        position_topic,
        position_open: number("position_open", 100.0),
        position_closed: number("position_closed", 0.0),
        set_position_topic: owned("set_position_topic"),
        set_position_template: template("set_position_template", "position")?,
        tilt_command_topic: owned("tilt_command_topic"),
        tilt_command_template: template("tilt_command_template", "tilt_position")?,
        tilt_status_topic: owned("tilt_status_topic"),
        tilt_status_template: ValueTemplate::parse(str_field(root, "tilt_status_template")),
        tilt_min: number("tilt_min", 0.0),
        tilt_max: number("tilt_max", 100.0),
    };
    let capabilities = CoverCapabilities {
        device_class: str_field(root, "device_class").and_then(CoverClass::from_ha),
        position: topics.set_position_topic.is_some(),
        tilt: topics.tilt_command_topic.is_some(),
        stop: topics.payload_stop.is_some(),
    };
    Ok((
        Capabilities::Cover(capabilities),
        EntityTopics::Cover(Box::new(topics)),
    ))
}

fn parse_event(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
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
        EntityTopics::Event {
            state_topic,
            source,
        },
    ))
}

fn parse_button(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    // A button's payload is `payload_press`; there's no value to template.
    let (command_topic, _) = plain_command(root, "a button")?;
    Ok((
        Capabilities::Button(ButtonCapabilities {
            device_class: str_field(root, "device_class").and_then(ButtonClass::from_ha),
        }),
        EntityTopics::Button {
            command_topic,
            // Home Assistant's default.
            payload_press: owned_str(root, "payload_press", "PRESS"),
        },
    ))
}

fn parse_text(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let (command_topic, command_template) = plain_command(root, "a text")?;
    let length = |key: &str, default: u32| {
        root.get(key)
            .and_then(serde_json::Value::as_u64)
            .map_or(default, |n| u32::try_from(n).unwrap_or(u32::MAX))
    };
    // Home Assistant's own defaults, and its 255-character cap.
    let max_length = length("max", 255).min(255);
    let capabilities = TextCapabilities {
        min_length: length("min", 0).min(max_length),
        max_length,
        pattern: str_field(root, "pattern").map(str::to_owned),
        mode: match str_field(root, "mode") {
            Some("password") => TextMode::Password,
            _ => TextMode::Text,
        },
    };
    Ok((
        Capabilities::Text(capabilities),
        EntityTopics::Text {
            state_topic: str_field(root, "state_topic").map(str::to_owned),
            command_topic,
            command_template,
            value_template: ValueTemplate::parse(str_field(root, "value_template")),
        },
    ))
}

fn parse_switch(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let device_class = str_field(root, "device_class").and_then(SwitchClass::from_ha);
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
            device_class: str_field(root, "device_class").and_then(BinarySensorClass::from_ha),
        }),
        EntityTopics::BinarySensor {
            state_topic,
            payload_on: owned_str(root, "payload_on", "ON"),
            payload_off: owned_str(root, "payload_off", "OFF"),
        },
    ))
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
    fn parses_a_z2m_select_and_sends_it_the_option() {
        // Zigbee2MQTT's `case "enum"` with set access.
        let payload = br#"{
            "unique_id": "0x0211000000000002_sensitivity_zigbee2mqtt",
            "name": "Sensitivity",
            "device": {"identifiers": ["zigbee2mqtt_0x0211000000000002"], "name": "Presence sensor"},
            "state_topic": "zigbee2mqtt/Presence sensor",
            "value_template": "{{ value_json[\"sensitivity\"] }}",
            "command_topic": "zigbee2mqtt/Presence sensor/set/sensitivity",
            "options": ["low", "medium", "high"],
            "entity_category": "config"
        }"#;
        let parsed = parse(Component::Select, payload).expect("valid");
        assert_eq!(
            parsed.capabilities,
            Capabilities::Select(SelectCapabilities {
                options: vec!["low".into(), "medium".into(), "high".into()]
            })
        );
        let state = crate::state::decode(
            &parsed.topics,
            "zigbee2mqtt/Presence sensor",
            br#"{"occupancy": false, "sensitivity": "medium"}"#,
            None,
        );
        assert_eq!(
            state,
            Some(Ok(irori_types::State::Select(irori_types::SelectState {
                option: "medium".into()
            })))
        );
        let sent = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::SelectSelectOption(irori_types::SelectOption {
                option: "high".into(),
            }),
        )
        .expect("a select takes select_option");
        assert_eq!(sent[0].topic, "zigbee2mqtt/Presence sensor/set/sensitivity");
        assert_eq!(sent[0].payload, b"high");

        let no_options = br#"{"unique_id": "s", "name": "Mode", "command_topic": "x/set"}"#;
        assert!(parse(Component::Select, no_options).is_err());
    }

    /// What Zigbee2MQTT publishes for a blind with a position (`case "cover"`, no motor state).
    const Z2M_COVER: &[u8] = br#"{
        "unique_id": "0x0211000000000010_cover_zigbee2mqtt", "name": null,
        "device": {"identifiers": ["zigbee2mqtt_0x0211000000000010"], "name": "Office blind"},
        "command_topic": "zigbee2mqtt/Office blind/set",
        "state_topic": "zigbee2mqtt/Office blind",
        "value_template": "{{ value_json[\"state\"] }}",
        "state_open": "OPEN", "state_closed": "CLOSE", "state_stopped": "STOP",
        "position_template": "{{ value_json[\"position\"] }}",
        "set_position_template": "{ \"position\": {{ position }} }",
        "set_position_topic": "zigbee2mqtt/Office blind/set",
        "position_topic": "zigbee2mqtt/Office blind"
    }"#;

    /// Zigbee2MQTT's mode-controlled fan (`case "fan"`, a ZCL hvacFanCtrl): speeds are the
    /// modes low, medium and high, translated through lookup tables, plus preset modes.
    #[test]
    fn a_z2m_mode_fan_turns_speeds_into_modes_and_back() {
        let payload = br#"{
            "unique_id": "0x03_fan", "name": null,
            "device": {"identifiers": ["zigbee2mqtt_0x03"], "name": "Ceiling fan"},
            "command_topic": "zigbee2mqtt/Ceiling fan/set/fan_state",
            "state_topic": "zigbee2mqtt/Ceiling fan",
            "state_value_template": "{{ value_json.fan_state }}",
            "percentage_state_topic": "zigbee2mqtt/Ceiling fan",
            "percentage_command_topic": "zigbee2mqtt/Ceiling fan/set/fan_mode",
            "percentage_value_template": "{{ {'off':0, 'low':1, 'medium':2, 'high':3}[value_json[\"fan_mode\"]] | default('None') }}",
            "percentage_command_template": "{{ {0:'off', 1:'low', 2:'medium', 3:'high'}[value] | default('') }}",
            "speed_range_min": 1, "speed_range_max": 3,
            "preset_mode_state_topic": "zigbee2mqtt/Ceiling fan",
            "preset_mode_command_topic": "zigbee2mqtt/Ceiling fan/set/fan_mode",
            "preset_mode_value_template": "{{ value_json[\"fan_mode\"] if value_json[\"fan_mode\"] in ['on', 'auto'] else 'None' | default('None') }}",
            "preset_modes": ["on", "auto"]
        }"#;
        let parsed = parse(Component::Fan, payload).expect("valid");
        assert_eq!(
            parsed.capabilities,
            Capabilities::Fan(FanCapabilities {
                speed_count: 3,
                oscillate: false,
                direction: false,
                preset_modes: vec!["on".into(), "auto".into()],
            })
        );
        let decoded = |body: &[u8]| {
            crate::state::decode(&parsed.topics, "zigbee2mqtt/Ceiling fan", body, None)
        };
        let Some(Ok(irori_types::State::Fan(medium))) =
            decoded(br#"{"fan_state": "ON", "fan_mode": "medium"}"#)
        else {
            panic!("a fan state");
        };
        assert!(medium.on);
        assert_eq!(medium.percentage, Some(66));
        let Some(Ok(irori_types::State::Fan(auto))) =
            decoded(br#"{"fan_state": "ON", "fan_mode": "auto"}"#)
        else {
            panic!("a fan state");
        };
        assert_eq!(auto.preset_mode.as_deref(), Some("auto"));
        let sent = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::FanSetPercentage(irori_types::FanPercentage { percentage: 100 }),
        )
        .expect("a speed");
        assert_eq!(sent[0].topic, "zigbee2mqtt/Ceiling fan/set/fan_mode");
        assert_eq!(sent[0].payload, b"high");
        let off = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::FanSetPercentage(irori_types::FanPercentage { percentage: 0 }),
        )
        .expect("off");
        assert_eq!(off[0].payload, b"OFF");
    }

    /// Zigbee2MQTT's speed-controlled fan: a number with `| default`, sent as it is.
    #[test]
    fn a_z2m_speed_fan_sends_its_speed() {
        let payload = br#"{"unique_id": "f", "name": "Fan", "command_topic": "z/Fan/set/state",
            "state_topic": "z/Fan", "state_value_template": "{{ value_json.state }}",
            "percentage_state_topic": "z/Fan", "percentage_command_topic": "z/Fan/set/speed",
            "percentage_value_template": "{{ value_json[\"speed\"] | default('None') }}",
            "percentage_command_template": "{{ value | default('') }}",
            "speed_range_min": 1, "speed_range_max": 10}"#;
        let parsed = parse(Component::Fan, payload).expect("valid");
        let Some(Ok(irori_types::State::Fan(fan))) = crate::state::decode(
            &parsed.topics,
            "z/Fan",
            br#"{"state": "ON", "speed": 5}"#,
            None,
        ) else {
            panic!("a fan state");
        };
        assert_eq!((fan.on, fan.percentage), (true, Some(50)));
        let sent = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::FanSetPercentage(irori_types::FanPercentage { percentage: 75 }),
        )
        .expect("a speed");
        assert_eq!(sent[0].payload, b"8");
    }

    #[test]
    fn a_valve_opens_and_closes_or_says_how_far() {
        let plain = parse(
            Component::Valve,
            br#"{"unique_id": "v", "name": "Main water", "command_topic": "v/set",
                "state_topic": "v/state", "device_class": "water"}"#,
        )
        .expect("valid");
        assert_eq!(
            plain.capabilities,
            Capabilities::Valve(ValveCapabilities {
                device_class: Some(ValveClass::Water),
                position: false,
                stop: false,
            })
        );
        assert_eq!(
            crate::state::decode(&plain.topics, "v/state", b"closed", None),
            Some(Ok(irori_types::State::Valve(irori_types::ValveState {
                state: irori_types::OpenState::Closed,
                position: None,
            })))
        );
        let sent =
            crate::state::encode(&plain.topics, &irori_types::Service::ValveOpen).expect("open");
        assert_eq!(sent[0].payload, b"OPEN");

        let zone = parse(
            Component::Valve,
            br#"{"unique_id": "z", "name": "Garden zone", "command_topic": "z/set",
                "state_topic": "z/state", "reports_position": true, "payload_stop": "STOP"}"#,
        )
        .expect("valid");
        assert_eq!(
            crate::state::decode(&zone.topics, "z/state", b"40", None),
            Some(Ok(irori_types::State::Valve(irori_types::ValveState {
                state: irori_types::OpenState::Open,
                position: Some(40),
            })))
        );
        let sent = crate::state::encode(
            &zone.topics,
            &irori_types::Service::ValveSetPosition(irori_types::SetPosition { position: 25 }),
        )
        .expect("a position");
        assert_eq!(
            (sent[0].topic.as_str(), sent[0].payload.as_slice()),
            ("z/set", &b"25"[..])
        );
    }

    #[test]
    fn a_z2m_lock_says_lock_and_unlock() {
        // Zigbee2MQTT's `case "lock"`: its own words for the two states.
        let payload = br#"{"unique_id": "0x02_lock", "name": null,
            "device": {"identifiers": ["zigbee2mqtt_0x02"], "name": "Front door"},
            "command_topic": "zigbee2mqtt/Front door/set", "state_topic": "zigbee2mqtt/Front door",
            "value_template": "{{ value_json[\"state\"] }}",
            "state_locked": "LOCK", "state_unlocked": "UNLOCK"}"#;
        let parsed = parse(Component::Lock, payload).expect("valid");
        assert_eq!(
            parsed.capabilities,
            Capabilities::Lock(LockCapabilities::default()),
            "no open, no code"
        );
        assert_eq!(
            crate::state::decode(
                &parsed.topics,
                "zigbee2mqtt/Front door",
                br#"{"state": "UNLOCK", "battery": 80}"#,
                None
            ),
            Some(Ok(irori_types::State::Lock(irori_types::LockState {
                state: LockStatus::Unlocked
            })))
        );
        let sent = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::LockLock(irori_types::LockCode::default()),
        )
        .expect("lock");
        assert_eq!(sent[0].topic, "zigbee2mqtt/Front door/set");
        assert_eq!(sent[0].payload, b"LOCK");
        assert!(
            crate::state::encode(
                &parsed.topics,
                &irori_types::Service::LockOpen(irori_types::LockCode::default())
            )
            .is_err()
        );
    }

    #[test]
    fn a_z2m_cover_is_read_and_sent_to_a_position() {
        let parsed = parse(Component::Cover, Z2M_COVER).expect("valid");
        assert_eq!(
            parsed.capabilities,
            Capabilities::Cover(CoverCapabilities {
                device_class: None,
                position: true,
                tilt: false,
                stop: true,
            })
        );
        let unique_id = UniqueId::try_from("0x0211000000000010_cover_zigbee2mqtt").expect("valid");
        assert_eq!(
            crate::state::topics_of(&unique_id, &parsed.topics).len(),
            1,
            "state and position share one topic, listened to once"
        );
        let decoded = crate::state::decode(
            &parsed.topics,
            "zigbee2mqtt/Office blind",
            br#"{"state": "OPEN", "position": 40}"#,
            None,
        );
        assert_eq!(
            decoded,
            Some(Ok(irori_types::State::Cover(irori_types::CoverState {
                state: irori_types::OpenState::Open,
                position: Some(40),
                tilt: None,
            })))
        );
        // Stopped at the bottom is closed.
        let stopped = crate::state::decode(
            &parsed.topics,
            "zigbee2mqtt/Office blind",
            br#"{"state": "STOP", "position": 0}"#,
            None,
        );
        assert!(matches!(
            stopped,
            Some(Ok(irori_types::State::Cover(irori_types::CoverState {
                state: irori_types::OpenState::Closed,
                ..
            })))
        ));
        let sent = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::CoverSetPosition(irori_types::SetPosition { position: 75 }),
        )
        .expect("a position");
        assert_eq!(sent[0].topic, "zigbee2mqtt/Office blind/set");
        assert_eq!(sent[0].payload, br#"{ "position": 75 }"#);
        let sent =
            crate::state::encode(&parsed.topics, &irori_types::Service::CoverClose).expect("close");
        assert_eq!(sent[0].payload, b"CLOSE");
    }

    #[test]
    fn a_cover_whose_state_needs_jinja_goes_by_its_position() {
        let payload = br#"{"unique_id": "c", "name": "Curtain", "command_topic": "c/set",
            "state_topic": "c", "position_topic": "c",
            "value_template": "{% if value_json.motor_state == 'opening' %}opening{% endif %}",
            "position_template": "{{ value_json.position }}", "set_position_topic": "c/set"}"#;
        let parsed = parse(Component::Cover, payload).expect("valid");
        let decoded = crate::state::decode(&parsed.topics, "c", br#"{"position": 0}"#, None);
        assert!(matches!(
            decoded,
            Some(Ok(irori_types::State::Cover(irori_types::CoverState {
                state: irori_types::OpenState::Closed,
                position: Some(0),
                ..
            })))
        ));
        // With nothing else to go by, it's refused, and listed with why.
        let blind = br#"{"unique_id": "d", "name": "Blind", "command_topic": "d/set",
            "state_topic": "d", "value_template": "{% if x %}open{% endif %}"}"#;
        assert!(parse(Component::Cover, blind).is_err_and(|e| e.contains("Jinja")));
    }

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

    #[test]
    fn a_button_sends_its_press_payload_and_listens_to_nothing() {
        let payload = br#"{"unique_id": "0x1234_identify", "name": "Identify",
            "command_topic": "zigbee2mqtt/Lamp/set/identify", "payload_press": "identify",
            "device_class": "identify", "entity_category": "config"}"#;
        let parsed = parse(Component::Button, payload).expect("valid");
        assert_eq!(
            parsed.capabilities,
            Capabilities::Button(ButtonCapabilities {
                device_class: Some(ButtonClass::Identify)
            })
        );
        let unique_id = UniqueId::try_from("0x1234_identify").expect("valid");
        assert!(crate::state::topics_of(&unique_id, &parsed.topics).is_empty());
        let sent = crate::state::encode(&parsed.topics, &irori_types::Service::ButtonPress)
            .expect("a button takes press");
        assert_eq!(sent[0].topic, "zigbee2mqtt/Lamp/set/identify");
        assert_eq!(sent[0].payload, b"identify");
        let plain = br#"{"unique_id": "b", "name": "Go", "command_topic": "x/set"}"#;
        let parsed = parse(Component::Button, plain).expect("valid");
        let sent = crate::state::encode(&parsed.topics, &irori_types::Service::ButtonPress)
            .expect("press");
        assert_eq!(sent[0].payload, b"PRESS", "Home Assistant's default");
    }

    #[test]
    fn parses_a_text_with_home_assistants_defaults() {
        let payload = br#"{"unique_id": "msg", "name": "Message", "command_topic": "panel/msg/set",
            "state_topic": "panel/msg", "max": 1000, "mode": "password"}"#;
        let parsed = parse(Component::Text, payload).expect("valid");
        let Capabilities::Text(caps) = &parsed.capabilities else {
            panic!("a text");
        };
        assert_eq!(
            (caps.min_length, caps.max_length),
            (0, 255),
            "capped at 255"
        );
        assert_eq!(caps.mode, TextMode::Password);
        assert_eq!(
            crate::state::decode(&parsed.topics, "panel/msg", b"hello", None),
            Some(Ok(irori_types::State::Text(irori_types::TextState {
                value: "hello".into()
            })))
        );
        let sent = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::TextSetValue(irori_types::TextSetValue {
                value: "Dinner at 7".into(),
            }),
        )
        .expect("a text takes set_value");
        assert_eq!(sent[0].payload, b"Dinner at 7");
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
