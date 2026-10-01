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
/// 2. Adds `entity_category` on entities and `options` on text sensors.
/// 3. Adds `number`.
/// 4. Adds `select`.
pub const ENTITY_FORMAT: u32 = 4;

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
        }
    }
}

/// Whether a reader of `format` can be sent entities of `kind`.
pub fn readable(format: u32, kind: EntityKind) -> bool {
    kind.since_format() <= format
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
    }

    #[test]
    fn every_kind_is_in_the_current_format() {
        for kind in EntityKind::ALL {
            assert!(readable(ENTITY_FORMAT, *kind), "{kind}");
        }
    }
}
