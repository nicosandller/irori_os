//! JSON Schema generation. `cargo xtask schemas` writes these to `schemas/`.

use schemars::generate::SchemaSettings;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};

use crate::{
    Area, AttributeKey, Device, DeviceDescription, Entity, EntityDescription, EntityKind,
    EntityState, ExtensionManifest, Floor, ServiceCall, StateReport,
};

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
        doc::<ExtensionManifest>("extension-manifest"),
        doc::<DeviceDescription>("device-description"),
        doc::<EntityDescription>("entity-description"),
        doc::<StateReport>("state-report"),
        doc::<ServiceCall>("service-call"),
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
    require_state_key(schema);
}

pub(crate) fn state_report_requires_state(schema: &mut Schema) {
    require_state_key(schema);
}

/// `state` may be null but must be present. schemars' `required` would also drop `null`.
fn require_state_key(schema: &mut Schema) {
    let state = serde_json::Value::from("state");
    match schema.get_mut("required") {
        Some(serde_json::Value::Array(required)) => {
            if !required.contains(&state) {
                required.push(state);
            }
        }
        _ => {
            schema.insert("required".into(), serde_json::Value::Array(vec![state]));
        }
    }
}

/// `[contributes]`: kinds this version doesn't know are allowed (and ignored with a warning),
/// but must still be lists of tables, as Rust reads them.
pub(crate) fn contributions_other_kinds(schema: &mut Schema) {
    schema.insert(
        "additionalProperties".into(),
        serde_json::json!({ "type": "array", "items": { "type": "object" } }),
    );
}

/// A nameless entity uses its device's name, so it must have a device. `null` counts as absent,
/// as it does in Rust.
pub(crate) fn entity_description_name_or_device(schema: &mut Schema) {
    schema.insert(
        "anyOf".into(),
        serde_json::json!([
            { "required": ["name"], "properties": { "name": { "type": "string" } } },
            {
                "required": ["device_unique_id"],
                "properties": { "device_unique_id": { "type": "string" } },
            },
        ]),
    );
}

/// `light.turn_on` takes a color temperature or an RGB color, not both. `null` counts as absent,
/// as it does in Rust.
pub(crate) fn one_color_setting(schema: &mut Schema) {
    schema.insert(
        "not".into(),
        serde_json::json!({
            "properties": {
                "color_temp_kelvin": { "type": "integer" },
                "rgb": { "type": "array" },
            },
            "required": ["color_temp_kelvin", "rgb"],
        }),
    );
}

/// `attributes`: keys follow the full `AttributeKey` rule (pattern *and* length). The default map
/// schema only carries the key pattern.
pub(crate) fn attributes_schema(generator: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "object",
        "description": "Free-form extras from the integration, e.g. Zigbee link quality. Readable by rules but not type-checked.",
        "propertyNames": generator.subschema_for::<AttributeKey>(),
    })
}
