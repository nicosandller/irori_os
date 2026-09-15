//! JSON Schema generation. `cargo xtask schemas` writes these to `schemas/`.

use schemars::generate::SchemaSettings;
use schemars::{JsonSchema, Schema, json_schema};

use crate::{Area, Device, Entity, EntityKind, EntityState, Floor};

/// One generated schema document.
#[derive(Debug)]
pub struct SchemaDoc {
    /// File stem, e.g. `entity-state` for `schemas/entity-state.schema.json`.
    pub name: &'static str,
    pub schema: Schema,
}

/// Every top-level document type, as draft 2020-12 JSON Schemas.
pub fn schemas() -> Vec<SchemaDoc> {
    fn doc<T: JsonSchema>(name: &'static str) -> SchemaDoc {
        let schema = SchemaSettings::draft2020_12()
            .into_generator()
            .into_root_schema_for::<T>();
        SchemaDoc { name, schema }
    }
    vec![
        doc::<Floor>("floor"),
        doc::<Area>("area"),
        doc::<Device>("device"),
        doc::<Entity>("entity"),
        doc::<EntityState>("entity-state"),
    ]
}

/// Adds "the kind in the entity id must match the kind tag in `tagged_field`" to a schema, so
/// schema validators (editors, LLM tooling) catch it too, not only Rust deserialization.
fn require_kind_match(schema: &mut Schema, id_field: &str, tagged_field: &str) {
    let rules: Vec<_> = EntityKind::ALL
        .iter()
        .map(|kind| {
            json_schema!({
                "if": {
                    "properties": { id_field: { "pattern": format!("^{}\\.", kind.domain()) } },
                    "required": [id_field],
                },
                "then": {
                    "properties": { tagged_field: { "properties": { "kind": { "const": kind.domain() } } } },
                },
            })
        })
        .collect();
    schema.insert(
        "allOf".into(),
        serde_json::to_value(rules).unwrap_or_default(),
    );
}

pub(crate) fn entity_kind_match(schema: &mut Schema) {
    require_kind_match(schema, "id", "capabilities");
}

pub(crate) fn entity_state_kind_match(schema: &mut Schema) {
    require_kind_match(schema, "entity_id", "state");
    // `state` may be null but must be present. schemars' `required` would also drop `null`.
    if let Some(serde_json::Value::Array(required)) = schema.get_mut("required") {
        let state = serde_json::Value::from("state");
        if !required.contains(&state) {
            required.push(state);
        }
    }
}
