//! Parsing an HA MQTT Discovery config payload into what Irori needs: a device, an entity's
//! capabilities, and the topics/schema to read and write it through. Each component's own module
//! under [`crate::kinds`] reads its part of the config; this reads what every one shares.
//!
//! Deliberately permissive: real payloads (Z2M, Tasmota, ESPHome-over-MQTT) carry many fields
//! Irori doesn't use, and HA itself tolerates most of them being absent. This reads what it
//! needs from a `serde_json::Value` rather than a strict typed struct, so an unrelated or
//! unexpected field never fails the whole entity — only a field this parser actually depends on
//! being unusable does that, and always with a reason a person could act on.

use irori_types::{Capabilities, EntityCategory, EntityKind, Name, UniqueId};

use crate::kinds::{
    binary_sensor, button, climate, cover, event, fan, humidifier, light, lock, number, select,
    sensor, siren, switch, text, valve, water_heater,
};
use crate::template::CommandTemplate;
use crate::topic::Component;

pub use crate::kinds::binary_sensor::BinarySensorTopics;
pub use crate::kinds::button::ButtonTopics;
pub use crate::kinds::climate::ClimateTopics;
pub use crate::kinds::cover::CoverTopics;
pub use crate::kinds::event::{EventSource, EventTopics};
pub use crate::kinds::fan::FanTopics;
pub use crate::kinds::humidifier::HumidifierTopics;
pub use crate::kinds::light::LightTopics;
pub use crate::kinds::lock::LockTopics;
pub use crate::kinds::number::NumberTopics;
pub use crate::kinds::opening::OpeningTopics;
pub use crate::kinds::select::SelectTopics;
pub use crate::kinds::sensor::SensorTopics;
pub use crate::kinds::setting::{Power, Reading, Setting};
pub use crate::kinds::siren::SirenTopics;
pub use crate::kinds::switch::SwitchTopics;
pub use crate::kinds::text::TextTopics;
pub use crate::kinds::water_heater::WaterHeaterTopics;

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

/// The topics and wire schema for one entity, once its kind is known: each component's own,
/// from its module under [`crate::kinds`].
#[derive(Debug, Clone, PartialEq)]
pub enum EntityTopics {
    Light(LightTopics),
    Switch(SwitchTopics),
    Sensor(SensorTopics),
    BinarySensor(BinarySensorTopics),
    Number(NumberTopics),
    Select(SelectTopics),
    Text(TextTopics),
    Button(ButtonTopics),
    Event(EventTopics),
    /// Boxed: Home Assistant lets nearly every part of a cover vary.
    Cover(Box<CoverTopics>),
    Lock(Box<LockTopics>),
    Fan(Box<FanTopics>),
    Valve(Box<OpeningTopics>),
    Siren(SirenTopics),
    Climate(Box<ClimateTopics>),
    WaterHeater(Box<WaterHeaterTopics>),
    Humidifier(Box<HumidifierTopics>),
}

impl EntityTopics {
    /// The kind of entity these are the topics of.
    pub fn kind(&self) -> EntityKind {
        match self {
            Self::Light(_) => EntityKind::Light,
            Self::Switch(_) => EntityKind::Switch,
            Self::Sensor(_) => EntityKind::Sensor,
            Self::BinarySensor(_) => EntityKind::BinarySensor,
            Self::Number(_) => EntityKind::Number,
            Self::Select(_) => EntityKind::Select,
            Self::Text(_) => EntityKind::Text,
            Self::Button(_) => EntityKind::Button,
            Self::Event(_) => EntityKind::Event,
            Self::Cover(_) => EntityKind::Cover,
            Self::Lock(_) => EntityKind::Lock,
            Self::Fan(_) => EntityKind::Fan,
            Self::Valve(_) => EntityKind::Valve,
            Self::Siren(_) => EntityKind::Siren,
            Self::Climate(_) => EntityKind::Climate,
            Self::WaterHeater(_) => EntityKind::WaterHeater,
            Self::Humidifier(_) => EntityKind::Humidifier,
        }
    }
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
        Component::Light => light::parse(&root)?,
        Component::Switch => switch::parse(&root)?,
        Component::Sensor => sensor::parse(&root)?,
        Component::BinarySensor => binary_sensor::parse(&root)?,
        Component::Number => number::parse(&root)?,
        Component::Select => select::parse(&root)?,
        Component::Text => text::parse(&root)?,
        Component::Button => button::parse(&root)?,
        Component::Event => event::parse(&root)?,
        Component::Cover => cover::parse(&root)?,
        Component::Lock => lock::parse(&root)?,
        Component::Fan => fan::parse(&root)?,
        Component::Valve => valve::parse(&root)?,
        Component::Siren => siren::parse(&root)?,
        Component::Climate => climate::parse(&root)?,
        Component::WaterHeater => water_heater::parse(&root)?,
        Component::Humidifier => humidifier::parse(&root)?,
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

pub(crate) fn str_field<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(serde_json::Value::as_str)
}

pub(crate) fn bool_field(value: &serde_json::Value, key: &str) -> bool {
    value.get(key).and_then(serde_json::Value::as_bool) == Some(true)
}

pub(crate) fn owned_str(value: &serde_json::Value, key: &str, default: &str) -> String {
    str_field(value, key).unwrap_or(default).to_owned()
}

/// `key` as the word a device reports: text as it is, and `true`, `false` or a number written
/// out. Zigbee2MQTT's on and off for a sensor are whatever the device sends, often not text.
pub(crate) fn word_field(value: &serde_json::Value, key: &str, default: &str) -> String {
    match value.get(key) {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(word @ (serde_json::Value::Bool(_) | serde_json::Value::Number(_))) => {
            word.to_string()
        }
        _ => default.to_owned(),
    }
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

/// The `command_topic` of an entity whose command is its value, and how to put the value in.
///
/// A `command_template` that does more than place the value would need Jinja, which Irori doesn't
/// run (`docs/specs/protocols.md`): an entity that needs one is better refused than sent the
/// wrong thing.
pub(crate) fn plain_command(
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

/// A number from a discovery config, written as a number or (Zigbee2MQTT's `min_temp`) as text.
pub(crate) fn number_field(root: &serde_json::Value, key: &str) -> Option<f64> {
    match root.get(key)? {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

pub(crate) fn string_list(root: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    root.get(key)
        .and_then(serde_json::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::SwitchClass;

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
    fn a_missing_unique_id_is_rejected_with_a_clear_reason() {
        let error = parse(Component::Switch, br#"{"name": "x", "command_topic": "t"}"#)
            .expect_err("no unique_id");
        assert!(error.contains("unique_id"));
    }
}
