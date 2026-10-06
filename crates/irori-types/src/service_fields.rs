//! What each service's data is made of, field by field: a cover's `set_position` takes a
//! `position` from 0 to 100, a fan's `set_direction` takes one of `forward` and `reverse`.
//!
//! For whatever builds a call without knowing the kind: an automation editor draws its form
//! from this, and an engine brings a worked-out number into range with it. Read off
//! [`ServiceCall`]'s own JSON Schema, which already says all of it, so a service added to a
//! kind is described here without anything to keep in step.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde_json::Value;

use crate::{ServiceCall, ServiceName};

/// One field of a service's data.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceField {
    pub name: String,
    /// Whether the service can't be called without it.
    pub required: bool,
    pub shape: FieldShape,
    /// What its own type says it's for, if it says.
    pub description: Option<String>,
}

/// What a field holds.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldShape {
    Number {
        min: Option<f64>,
        max: Option<f64>,
        /// Whole numbers only.
        integer: bool,
    },
    Bool,
    /// Free text. An entity may narrow it to its own list (a select's options, a fan's
    /// preset modes), which its capabilities hold.
    Text,
    /// One of these words.
    Choice(Vec<String>),
    /// Something with a shape of its own (a colour's three numbers), written as JSON.
    Other,
}

impl ServiceName {
    /// The fields of this service's data, in name order. Empty for a service that takes none.
    pub fn fields(self) -> &'static [ServiceField] {
        static ALL: OnceLock<BTreeMap<&'static str, Vec<ServiceField>>> = OnceLock::new();
        ALL.get_or_init(read_all)
            .get(self.as_str())
            .map_or(&[], Vec::as_slice)
    }

    /// One field of this service's data, by name.
    pub fn field(self, name: &str) -> Option<&'static ServiceField> {
        self.fields().iter().find(|field| field.name == name)
    }
}

fn read_all() -> BTreeMap<&'static str, Vec<ServiceField>> {
    let schema = serde_json::to_value(schemars::schema_for!(ServiceCall)).unwrap_or_default();
    let defs = schema.get("$defs").cloned().unwrap_or_default();
    let rules = schema
        .get("allOf")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    ServiceName::ALL
        .iter()
        .map(|name| {
            let data = rules
                .iter()
                .find(|rule| {
                    rule.pointer("/if/properties/service/const")
                        .and_then(Value::as_str)
                        == Some(name.as_str())
                })
                .and_then(|rule| rule.pointer("/then/properties/data"))
                .map(|data| resolve(data, &defs))
                .unwrap_or_default();
            let required: Vec<&str> = data
                .get("required")
                .and_then(Value::as_array)
                .map(|all| all.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let fields = data
                .get("properties")
                .and_then(Value::as_object)
                .map(|properties| {
                    properties
                        .iter()
                        .map(|(field, described)| {
                            let described = resolve(described, &defs);
                            ServiceField {
                                name: field.clone(),
                                required: required.contains(&field.as_str()),
                                shape: shape(&described, &defs),
                                description: described
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .map(|text| text.replace('\n', " ")),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            (name.as_str(), fields)
        })
        .collect()
}

/// A schema with its `$ref` followed, and an "or nothing" (`anyOf` with `null`) unwrapped to
/// the part that says something.
fn resolve(schema: &Value, defs: &Value) -> Value {
    if let Some(name) = schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|path| path.strip_prefix("#/$defs/"))
    {
        let mut found = defs.get(name).map_or(Value::Null, |def| resolve(def, defs));
        // What's said beside the reference (a field's own description) wins over the type's.
        if let (Some(found), Some(beside)) = (found.as_object_mut(), schema.as_object()) {
            for (key, value) in beside {
                if key != "$ref" {
                    found.insert(key.clone(), value.clone());
                }
            }
        }
        return found;
    }
    if let Some(options) = schema.get("anyOf").and_then(Value::as_array) {
        let real: Vec<&Value> = options
            .iter()
            .filter(|option| option.get("type").and_then(Value::as_str) != Some("null"))
            .collect();
        if let [only] = real.as_slice() {
            let mut found = resolve(only, defs);
            if let (Some(found), Some(description)) =
                (found.as_object_mut(), schema.get("description"))
            {
                found.insert("description".into(), description.clone());
            }
            return found;
        }
    }
    schema.clone()
}

fn shape(schema: &Value, defs: &Value) -> FieldShape {
    // An enum of words, written either as `enum` or as one `const` per documented variant.
    let words: Vec<String> = schema
        .get("enum")
        .and_then(Value::as_array)
        .map(|all| all.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .or_else(|| {
            schema.get("oneOf").and_then(Value::as_array).map(|all| {
                all.iter()
                    .flat_map(|one| {
                        let one = resolve(one, defs);
                        let single = one.get("const").and_then(Value::as_str).map(str::to_owned);
                        let several = one.get("enum").and_then(Value::as_array).map(|all| {
                            all.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect::<Vec<_>>()
                        });
                        single.into_iter().chain(several.into_iter().flatten())
                    })
                    .collect()
            })
        })
        .unwrap_or_default();
    if !words.is_empty() {
        return FieldShape::Choice(words);
    }
    let types: Vec<&str> = match schema.get("type") {
        Some(Value::String(one)) => vec![one.as_str()],
        Some(Value::Array(several)) => several.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let bound = |key: &str| schema.get(key).and_then(Value::as_f64);
    let unsigned = schema
        .get("format")
        .and_then(Value::as_str)
        .is_some_and(|format| format.starts_with("uint"));
    if types.contains(&"integer") || types.contains(&"number") {
        FieldShape::Number {
            min: bound("minimum").or(unsigned.then_some(0.0)),
            max: bound("maximum"),
            integer: types.contains(&"integer"),
        }
    } else if types.contains(&"boolean") {
        FieldShape::Bool
    } else if types.contains(&"string") {
        FieldShape::Text
    } else {
        FieldShape::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: ServiceName, field: &str) -> ServiceField {
        name.field(field)
            .unwrap_or_else(|| panic!("{name} has a `{field}`: {:?}", name.fields()))
            .clone()
    }

    #[test]
    fn a_service_says_what_its_data_is_made_of() {
        assert!(ServiceName::SwitchTurnOn.fields().is_empty());
        assert!(ServiceName::CoverOpen.fields().is_empty());

        let tilt = field(ServiceName::CoverSetTilt, "tilt");
        assert!(tilt.required);
        assert_eq!(
            tilt.shape,
            FieldShape::Number {
                min: Some(0.0),
                max: Some(100.0),
                integer: true
            }
        );

        let brightness = field(ServiceName::LightTurnOn, "brightness");
        assert!(!brightness.required);
        assert!(
            matches!(brightness.shape, FieldShape::Number { integer: true, .. }),
            "{brightness:?}"
        );
        assert_eq!(field(ServiceName::LightTurnOn, "rgb").shape, FieldShape::Other);

        let direction = field(ServiceName::FanSetDirection, "direction");
        assert_eq!(
            direction.shape,
            FieldShape::Choice(vec!["forward".into(), "reverse".into()])
        );

        let temperature = field(ServiceName::ClimateSetTemperature, "temperature");
        assert!(matches!(
            temperature.shape,
            FieldShape::Number { integer: false, .. }
        ));
        let mode = field(ServiceName::ClimateSetTemperature, "hvac_mode");
        assert!(
            matches!(&mode.shape, FieldShape::Choice(modes) if modes.contains(&"heat".to_owned())),
            "{mode:?}"
        );

        assert_eq!(
            field(ServiceName::SelectSelectOption, "option").shape,
            FieldShape::Text
        );
        assert_eq!(
            field(ServiceName::FanOscillate, "oscillating").shape,
            FieldShape::Bool
        );
    }

    /// Whatever a service's own parser insists on, this says is required: the two are read
    /// from different places, and an editor that trusted this would otherwise build a call
    /// the core refuses.
    #[test]
    fn required_fields_agree_with_what_each_service_refuses() {
        for name in ServiceName::ALL {
            let any_required = name.fields().iter().any(|field| field.required);
            // One way only: a thermostat's `set_temperature` needs one of several fields,
            // without any single one of them being required.
            assert!(!any_required || name.requires_data(), "{name}");
            assert_eq!(!name.fields().is_empty(), name.takes_data(), "{name}");
        }
    }
}
