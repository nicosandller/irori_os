//! A settings form generated from an extension's own JSON Schema (`config_schema`) — the gear
//! icon on its Extensions card. Generic on purpose: this page doesn't know MQTT or Zigbee from
//! any other extension, the same way `crate::waiting` doesn't know what a secret is for.
//!
//! There's no way to show what's already configured: secrets never round-trip, and non-secret
//! settings aren't sent back either (kept simple, matching the existing secrets form's own
//! limit) — a field with a schema `default` starts pre-filled with it; everything else starts
//! blank. Only fields the user actually touched are sent on submit — a field left exactly as it
//! started (blank, or at its schema default) is omitted, so saving one field never resets the
//! others back to their defaults; a touched field left blank is also omitted, so this form still
//! has no way to explicitly clear a field back to blank.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;

#[derive(Debug, Clone, PartialEq)]
struct Field {
    key: String,
    label: String,
    kind: FieldKind,
    default: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    Bool,
    Number,
    Text,
    Secret,
}

/// One field per schema property, required ones first, alphabetical within each group.
fn fields_from_schema(schema: &serde_json::Value) -> Vec<Field> {
    let Some(properties) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };
    let mut fields: Vec<Field> = properties
        .iter()
        .map(|(key, field_schema)| Field {
            label: label_from_key(key),
            kind: classify(field_schema, schema),
            default: default_value(field_schema),
            key: key.clone(),
        })
        .collect();
    let required: Vec<&str> = schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .map(|items| items.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();
    fields.sort_by(|a, b| {
        let a_required = required.contains(&a.key.as_str());
        let b_required = required.contains(&b.key.as_str());
        (!a_required, &a.key).cmp(&(!b_required, &b.key))
    });
    fields
}

fn label_from_key(key: &str) -> String {
    let mut label = key.replace('_', " ");
    if let Some(first) = label.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    label
}

/// Resolves `$ref`/`anyOf`'s non-null branch to find the real type — schemars' shape for
/// `Option<Secret>` is `{"anyOf": [{"$ref": "#/$defs/Secret"}, {"type": "null"}]}`.
fn classify(field_schema: &serde_json::Value, root: &serde_json::Value) -> FieldKind {
    if let Some(resolved) = resolve_ref(field_schema, root) {
        return classify(resolved, root);
    }
    if field_schema
        .get("writeOnly")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        return FieldKind::Secret;
    }
    for combinator in ["anyOf", "oneOf"] {
        if let Some(branches) = field_schema
            .get(combinator)
            .and_then(serde_json::Value::as_array)
        {
            for branch in branches {
                if branch.get("type").and_then(serde_json::Value::as_str) == Some("null") {
                    continue;
                }
                return classify(branch, root);
            }
        }
    }
    let type_name = match field_schema.get("type") {
        Some(serde_json::Value::String(s)) => Some(s.as_str()),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(serde_json::Value::as_str)
            .find(|t| *t != "null"),
        _ => None,
    };
    match type_name {
        Some("boolean") => FieldKind::Bool,
        Some("integer" | "number") => FieldKind::Number,
        _ => FieldKind::Text,
    }
}

fn resolve_ref<'a>(
    field_schema: &serde_json::Value,
    root: &'a serde_json::Value,
) -> Option<&'a serde_json::Value> {
    let reference = field_schema.get("$ref")?.as_str()?;
    reference
        .strip_prefix("#/$defs/")
        .and_then(|name| root.get("$defs")?.get(name))
}

fn default_value(field_schema: &serde_json::Value) -> Option<String> {
    match field_schema.get("default")? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A form generated from `schema`; `on_close` fires after a successful save, and on Cancel.
#[component]
pub fn SettingsForm(
    id: String,
    schema: serde_json::Value,
    #[prop(into)] on_close: Callback<()>,
) -> impl IntoView {
    let fields = fields_from_schema(&schema);
    let rows: Vec<(Field, RwSignal<String>, RwSignal<bool>)> = fields
        .into_iter()
        .map(|field| {
            let initial = field.default.clone().unwrap_or_default();
            (field, RwSignal::new(initial), RwSignal::new(false))
        })
        .collect();
    let saving = RwSignal::new(false);
    let trouble = RwSignal::new(None::<String>);

    let submit = {
        let rows = rows.clone();
        move |ev: leptos::ev::SubmitEvent| {
            ev.prevent_default();
            let mut body = serde_json::Map::new();
            for (field, value, touched) in &rows {
                if !touched.get_untracked() {
                    continue;
                }
                let text = value.get_untracked();
                if text.trim().is_empty() {
                    continue;
                }
                let parsed = match field.kind {
                    FieldKind::Bool => serde_json::Value::Bool(text == "true"),
                    FieldKind::Number => match text.trim().parse::<f64>() {
                        Ok(n) => serde_json::json!(n),
                        Err(_) => {
                            trouble.set(Some(format!("\"{}\" needs a number", field.label)));
                            return;
                        }
                    },
                    FieldKind::Text | FieldKind::Secret => serde_json::Value::String(text.clone()),
                };
                body.insert(field.key.clone(), parsed);
            }
            let id = id.clone();
            saving.set(true);
            spawn_local(async move {
                match api::set_extension_settings(&id, &body).await {
                    Ok(()) => {
                        trouble.set(None);
                        on_close.run(());
                    }
                    Err(why) => trouble.set(Some(why)),
                }
                saving.set(false);
            });
        }
    };

    view! {
        <form class="settings-form" on:submit=submit>
            {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}
            {
                rows.clone()
                    .into_iter()
                    .map(|(field, value, touched)| field_row(field, value, touched))
                    .collect_view()
            }
            <div class="settings-form-actions">
                <button type="submit" class="add" disabled=move || saving.get()>
                    {move || if saving.get() { "Saving…" } else { "Save" }}
                </button>
                <button type="button" on:click=move |_| on_close.run(())>
                    "Cancel"
                </button>
            </div>
        </form>
    }
}

fn field_row(field: Field, value: RwSignal<String>, touched: RwSignal<bool>) -> impl IntoView {
    let label = field.label.clone();
    match field.kind {
        FieldKind::Bool => view! {
            <label class="settings-field settings-field-checkbox">
                <input
                    type="checkbox"
                    prop:checked=move || value.get() == "true"
                    on:change:target=move |ev| {
                        value.set(if ev.target().checked() { "true" } else { "false" }.to_owned());
                        touched.set(true);
                    }
                />
                {label}
            </label>
        }
        .into_any(),
        FieldKind::Secret => view! {
            <label class="settings-field">
                <span>{label}</span>
                <input
                    type="password"
                    autocomplete="off"
                    spellcheck="false"
                    prop:value=value
                    on:input:target=move |ev| {
                        value.set(ev.target().value());
                        touched.set(true);
                    }
                />
            </label>
        }
        .into_any(),
        FieldKind::Number => view! {
            <label class="settings-field">
                <span>{label}</span>
                <input
                    type="number"
                    prop:value=value
                    on:input:target=move |ev| {
                        value.set(ev.target().value());
                        touched.set(true);
                    }
                />
            </label>
        }
        .into_any(),
        FieldKind::Text => view! {
            <label class="settings-field">
                <span>{label}</span>
                <input
                    type="text"
                    prop:value=value
                    on:input:target=move |ev| {
                        value.set(ev.target().value());
                        touched.set(true);
                    }
                />
            </label>
        }
        .into_any(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_fields_sort_before_optional_ones_alphabetically_within_each_group() {
        let schema = serde_json::json!({
            "properties": {
                "zebra": {"type": "string"},
                "apple": {"type": "string"},
                "host": {"type": "string"},
            },
            "required": ["zebra", "host"],
        });
        let fields = fields_from_schema(&schema);
        let keys: Vec<&str> = fields.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, vec!["host", "zebra", "apple"]);
    }

    #[test]
    fn an_optional_secret_classifies_through_its_ref_and_anyof_null_branch() {
        // schemars' own shape for `Option<Secret>`.
        let schema = serde_json::json!({
            "properties": {
                "password": {
                    "anyOf": [{"$ref": "#/$defs/Secret"}, {"type": "null"}],
                },
            },
            "$defs": {
                "Secret": {"type": "string", "writeOnly": true},
            },
        });
        let fields = fields_from_schema(&schema);
        assert_eq!(fields[0].kind, FieldKind::Secret);
    }

    #[test]
    fn a_default_prefills_but_a_missing_one_starts_blank() {
        let schema = serde_json::json!({
            "properties": {
                "port": {"type": "integer", "default": 1883},
                "host": {"type": "string"},
            },
        });
        let fields = fields_from_schema(&schema);
        let by_key = |key: &str| fields.iter().find(|f| f.key == key).expect("present");
        assert_eq!(by_key("port").default.as_deref(), Some("1883"));
        assert_eq!(by_key("host").default, None);
    }
}
