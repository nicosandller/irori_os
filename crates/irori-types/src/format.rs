//! The entity format: what an extension that reads entities and states through the API (an
//! automation engine, its page) can parse. See `docs/specs/extensions.md` §5.
//!
//! Extensions parse what they're sent strictly, as Irori itself does, so one built before a kind
//! or a field existed can't read it: an unknown kind in an entity id, or an unknown field, fails
//! the whole message. Each extension says which format it reads (`entity_format` in its
//! manifest), and Irori sends it entities and states in that format: the kinds it had, without
//! the fields added since.
//!
//! **Changing the format.** Anything that changes what an `Entity` or `EntityState` looks like on
//! the wire (a new kind, a new field) bumps [`ENTITY_FORMAT`] and teaches [`entity_for`] and
//! [`state_for`] to leave it out for older readers.

use crate::{Entity, EntityKind, EntityState};

/// The format this build writes.
///
/// 1. The first: `light`, `switch`, `sensor`, `binary_sensor`.
/// 2. Adds `entity_category` on entities, `options` on text sensors, and Home Assistant's full
///    lists of sensor and binary sensor classes.
/// 3. Adds `number`.
/// 4. Adds `select`.
/// 5. Adds `text`.
/// 6. Adds `button`.
/// 7. Adds `event`.
/// 8. Adds `cover`.
pub const ENTITY_FORMAT: u32 = 8;

pub(crate) fn first() -> u32 {
    1
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `skip_serializing_if` passes a reference
pub(crate) fn is_first(format: &u32) -> bool {
    *format == 1
}

impl EntityKind {
    /// The first entity format that has this kind. A reader of an older one is never sent it.
    pub fn since_format(self) -> u32 {
        match self {
            Self::Light | Self::Switch | Self::Sensor | Self::BinarySensor => 1,
            Self::Number => 3,
            Self::Select => 4,
            Self::Text => 5,
            Self::Button => 6,
            Self::Event => 7,
            Self::Cover => 8,
        }
    }
}

/// Whether a reader of `format` can be sent entities of `kind`.
pub fn readable(format: u32, kind: EntityKind) -> bool {
    kind.since_format() <= format
}

/// The sensor classes the first format had, by name. A reader of it fails on any other.
const FIRST_SENSOR_CLASSES: &[&str] = &[
    "temperature",
    "humidity",
    "illuminance",
    "pressure",
    "power",
    "energy",
    "voltage",
    "current",
    "battery",
    "co2",
    "pm25",
    "signal_strength",
    "distance",
];

/// The binary sensor classes the first format had.
const FIRST_BINARY_SENSOR_CLASSES: &[&str] = &[
    "motion",
    "occupancy",
    "door",
    "window",
    "moisture",
    "smoke",
    "gas",
    "vibration",
    "plug",
    "connectivity",
    "problem",
    "battery",
];

/// A class as a first-format reader knows it: the same, the older one a newer class was split
/// out of (`presence` was `occupancy`), or none.
fn first_format_class(kind: &str, class: &str) -> Option<&'static str> {
    let (known, folded): (&[&str], &[(&str, &str)]) = match kind {
        "sensor" => (
            FIRST_SENSOR_CLASSES,
            &[("atmospheric_pressure", "pressure")],
        ),
        "binary_sensor" => (
            FIRST_BINARY_SENSOR_CLASSES,
            &[
                ("presence", "occupancy"),
                ("garage_door", "door"),
                ("opening", "window"),
                ("safety", "problem"),
            ],
        ),
        // Switch classes haven't changed.
        _ => return None,
    };
    known
        .iter()
        .find(|known| **known == class)
        .copied()
        .or_else(|| {
            folded
                .iter()
                .find(|(new, _)| *new == class)
                .map(|(_, old)| *old)
        })
}

/// `entity` as a reader of `format` can parse it, or `None` if its kind is newer than that.
pub fn entity_for(format: u32, entity: &Entity) -> Option<serde_json::Value> {
    if !readable(format, entity.id.kind()) {
        return None;
    }
    let mut value = serde_json::to_value(entity).ok()?;
    if format < 2
        && let Some(object) = value.as_object_mut()
    {
        object.remove("entity_category");
        if let Some(capabilities) = object
            .get_mut("capabilities")
            .and_then(serde_json::Value::as_object_mut)
        {
            capabilities.remove("options");
            let kind = capabilities
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if kind != "switch"
                && let Some(class) = capabilities
                    .get("device_class")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            {
                match first_format_class(&kind, &class) {
                    Some(old) => {
                        capabilities.insert("device_class".into(), old.into());
                    }
                    None => {
                        capabilities.remove("device_class");
                    }
                }
            }
        }
    }
    Some(value)
}

/// `state` as a reader of `format` can parse it, or `None` if its entity's kind is newer than
/// that. Nothing about a state has changed between formats yet.
pub fn state_for(format: u32, state: &EntityState) -> Option<serde_json::Value> {
    readable(format, state.entity_id.kind())
        .then(|| serde_json::to_value(state).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIAGNOSTIC_TEXT_SENSOR: &str = r#"{
        "id": "sensor.washer_program", "protocol": "esphome", "unique_id": "w-program",
        "name": "Program",
        "capabilities": {"kind": "sensor", "value_type": "text", "options": ["wash", "rinse"]},
        "entity_category": "diagnostic"
    }"#;

    /// What a reader of the first format was built with: these fields and no others.
    #[test]
    fn a_first_format_reader_gets_only_what_it_knows() {
        let entity: Entity = serde_json::from_str(DIAGNOSTIC_TEXT_SENSOR).expect("valid");
        let old = entity_for(1, &entity).expect("a sensor is in the first format");
        assert_eq!(
            old,
            serde_json::json!({
                "id": "sensor.washer_program", "protocol": "esphome", "unique_id": "w-program",
                "name": "Program",
                "capabilities": {"kind": "sensor", "value_type": "text"},
            })
        );
        let now = entity_for(ENTITY_FORMAT, &entity).expect("current");
        assert_eq!(now, serde_json::to_value(&entity).expect("ser"));
    }

    #[test]
    fn a_reader_is_never_sent_a_kind_newer_than_its_format() {
        let timeout: Entity = serde_json::from_str(
            r#"{"id": "number.hallway_timeout", "protocol": "esphome", "unique_id": "t",
                "name": "Timeout", "capabilities": {"kind": "number", "min": 5, "max": 600, "step": 5}}"#,
        )
        .expect("valid");
        assert_eq!(entity_for(2, &timeout), None);
        assert_eq!(entity_for(1, &timeout), None);
        assert!(entity_for(3, &timeout).is_some());
        let mode: Entity = serde_json::from_str(
            r#"{"id": "select.heater_mode", "protocol": "esphome", "unique_id": "m",
                "name": "Mode", "capabilities": {"kind": "select", "options": ["eco", "comfort"]}}"#,
        )
        .expect("valid");
        assert_eq!(entity_for(3, &mode), None);
        assert!(entity_for(4, &mode).is_some());
        let message: Entity = serde_json::from_str(
            r#"{"id": "text.display", "protocol": "esphome", "unique_id": "d",
                "name": "Display", "capabilities": {"kind": "text"}}"#,
        )
        .expect("valid");
        assert_eq!(entity_for(4, &message), None);
        assert!(entity_for(5, &message).is_some());
        let restart: Entity = serde_json::from_str(
            r#"{"id": "button.restart", "protocol": "esphome", "unique_id": "r",
                "name": "Restart", "capabilities": {"kind": "button"}}"#,
        )
        .expect("valid");
        assert_eq!(entity_for(5, &restart), None);
        assert!(entity_for(6, &restart).is_some());
        let remote: Entity = serde_json::from_str(
            r#"{"id": "event.remote", "protocol": "zigbee", "unique_id": "e",
                "name": "Remote", "capabilities": {"kind": "event", "event_types": ["single"]}}"#,
        )
        .expect("valid");
        assert_eq!(entity_for(6, &remote), None);
        assert!(entity_for(7, &remote).is_some());
        let blind: Entity = serde_json::from_str(
            r#"{"id": "cover.blind", "protocol": "esphome", "unique_id": "b",
                "name": "Blind", "capabilities": {"kind": "cover"}}"#,
        )
        .expect("valid");
        assert_eq!(entity_for(7, &blind), None);
        assert!(entity_for(8, &blind).is_some());
    }

    /// Every `const` and `enum` string in a schema: an enum's names however schemars lays them out.
    fn names<'a>(schema: &'a serde_json::Value, into: &mut Vec<&'a str>) {
        match schema {
            serde_json::Value::Object(object) => {
                for (key, value) in object {
                    match (key.as_str(), value) {
                        ("const", serde_json::Value::String(name)) => into.push(name),
                        ("enum", serde_json::Value::Array(list)) => {
                            into.extend(list.iter().filter_map(serde_json::Value::as_str));
                        }
                        _ => names(value, into),
                    }
                }
            }
            serde_json::Value::Array(list) => list.iter().for_each(|v| names(v, into)),
            _ => {}
        }
    }

    /// Every class there is now reaches a first-format reader as one of its own, or not at all.
    #[test]
    fn a_first_format_reader_only_ever_sees_its_own_classes() {
        let schemas = crate::schemas();
        let entity_schema = schemas
            .iter()
            .find(|doc| doc.name == "entity")
            .expect("an entity schema");
        let defs = &entity_schema.schema.as_value()["$defs"];
        for (kind, enum_name, first) in [
            ("sensor", "SensorClass", FIRST_SENSOR_CLASSES),
            (
                "binary_sensor",
                "BinarySensorClass",
                FIRST_BINARY_SENSOR_CLASSES,
            ),
        ] {
            let mut all = Vec::new();
            names(&defs[enum_name], &mut all);
            assert!(all.len() > first.len(), "{enum_name}");
            for class in all {
                let capabilities = if kind == "sensor" {
                    serde_json::json!({"kind": "sensor", "value_type": "number", "device_class": class})
                } else {
                    serde_json::json!({"kind": "binary_sensor", "device_class": class})
                };
                let entity: Entity = serde_json::from_value(serde_json::json!({
                    "id": format!("{kind}.x"), "protocol": "p", "unique_id": "u", "name": "X",
                    "capabilities": capabilities,
                }))
                .expect("valid");
                let old = entity_for(1, &entity).expect("in the first format");
                match old["capabilities"]["device_class"].as_str() {
                    Some(sent) => assert!(first.contains(&sent), "{class} sent as {sent}"),
                    None => assert!(!first.contains(&class), "{class} dropped"),
                }
            }
        }
        // A split-out class goes back to what it was part of.
        let presence: Entity = serde_json::from_value(serde_json::json!({
            "id": "binary_sensor.desk", "protocol": "p", "unique_id": "u", "name": "Desk",
            "capabilities": {"kind": "binary_sensor", "device_class": "presence"},
        }))
        .expect("valid");
        assert_eq!(
            entity_for(1, &presence).expect("sent")["capabilities"]["device_class"],
            "occupancy"
        );
    }

    #[test]
    fn every_kind_is_in_the_current_format() {
        for kind in EntityKind::ALL {
            assert!(readable(ENTITY_FORMAT, *kind), "{kind}");
        }
    }
}
