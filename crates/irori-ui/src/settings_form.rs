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
    /// The schema's own `description` — for a Rust extension, the doc comment on the settings
    /// field, which is where the person who wrote it already explained what it's for.
    help: Option<String>,
    kind: FieldKind,
    default: Option<String>,
    required: bool,
    /// Whether this belongs under "Advanced" rather than in the form's own body; see `advanced`.
    advanced: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FieldKind {
    Bool,
    Number,
    Text,
    Secret,
    /// A fixed set of values, from the schema's `enum` — a list to pick from, not a text box
    /// where a typo becomes a settings error nobody can read.
    Choice(Vec<String>),
}

/// One field per schema property, required ones first, alphabetical within each group.
fn fields_from_schema(schema: &serde_json::Value) -> Vec<Field> {
    let Some(properties) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .map(|items| items.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();
    let mut fields: Vec<Field> = properties
        .iter()
        .map(|(key, field_schema)| {
            let kind = classify(field_schema, schema);
            Field {
                label: label_from_key(key),
                help: help_text(field_schema),
                // A secret is never folded away, however optional the schema says it is: a
                // credential is usually the reason someone opened this form at all.
                advanced: kind != FieldKind::Secret && advanced(field_schema),
                default: default_value(field_schema),
                required: required.contains(&key.as_str()),
                kind,
                key: key.clone(),
            }
        })
        .collect();
    fields.sort_by(|a, b| (!a.required, &a.key).cmp(&(!b.required, &b.key)));
    fields
}

/// Whether a field belongs under "Advanced": one that accepts `null`, which in schemars' output
/// means an `Option<T>` with no default — the extension works without it and decides for itself
/// what to do when it's unset. Anything else is either required or has a default worth showing.
///
/// This is what keeps a form's first screen to the handful of settings that actually need
/// answering: for Zigbee, the dongle's port and kind, rather than the PAN id and which
/// Zigbee2MQTT release to pin.
fn advanced(field_schema: &serde_json::Value) -> bool {
    let nullable = |value: &serde_json::Value| match value.get("type") {
        Some(serde_json::Value::String(name)) => name == "null",
        Some(serde_json::Value::Array(names)) => names
            .iter()
            .filter_map(serde_json::Value::as_str)
            .any(|name| name == "null"),
        _ => false,
    };
    if nullable(field_schema) {
        return true;
    }
    ["anyOf", "oneOf"].into_iter().any(|combinator| {
        field_schema
            .get(combinator)
            .and_then(serde_json::Value::as_array)
            .is_some_and(|branches| branches.iter().any(nullable))
    })
}

/// A schema `description` as one line: doc comments wrap, and the wrapping isn't meaningful.
fn help_text(field_schema: &serde_json::Value) -> Option<String> {
    let text = field_schema.get("description")?.as_str()?;
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!joined.is_empty()).then_some(joined)
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
    if let Some(choices) = field_schema
        .get("enum")
        .and_then(serde_json::Value::as_array)
    {
        let choices: Vec<String> = choices
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect();
        if !choices.is_empty() {
            return FieldKind::Choice(choices);
        }
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
                let parsed = match &field.kind {
                    FieldKind::Bool => serde_json::Value::Bool(text == "true"),
                    FieldKind::Number => match text.trim().parse::<f64>() {
                        Ok(n) => serde_json::json!(n),
                        Err(_) => {
                            trouble.set(Some(format!("\"{}\" needs a number", field.label)));
                            return;
                        }
                    },
                    FieldKind::Text | FieldKind::Secret | FieldKind::Choice(_) => {
                        serde_json::Value::String(text.clone())
                    }
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

    let (plain, folded): (Vec<_>, Vec<_>) = rows
        .clone()
        .into_iter()
        .partition(|(field, _, _)| !field.advanced);

    view! {
        <form class="settings-form" on:submit=submit>
            {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}
            {
                plain
                    .into_iter()
                    .map(|(field, value, touched)| field_row(field, value, touched))
                    .collect_view()
            }
            // Settings the extension is happy to decide for itself, out of the way of the ones
            // that actually need answering — a Zigbee dongle's port has to be given; which
            // Zigbee2MQTT release to pin to is a thing almost nobody ever sets.
            {(!folded.is_empty())
                .then(|| view! {
                    <details class="settings-advanced">
                        <summary>"Advanced"</summary>
                        <p class="muted small">
                            "Everything here has a sensible default. Leave it blank unless you "
                            "have a reason not to."
                        </p>
                        {folded
                            .into_iter()
                            .map(|(field, value, touched)| field_row(field, value, touched))
                            .collect_view()}
                    </details>
                })}
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

/// What the schema says this field is for, under its own input — a line of prose rather than a
/// `title=` tooltip, which a phone has no way to show at all.
fn help(field: &Field) -> Option<impl IntoView> {
    field
        .help
        .clone()
        .map(|text| view! { <span class="settings-help">{text}</span> })
}

fn field_row(field: Field, value: RwSignal<String>, touched: RwSignal<bool>) -> impl IntoView {
    let label = field.label.clone();
    let required = field
        .required
        .then(|| view! { <span class="settings-required">"Required"</span> });
    let note = help(&field);
    match field.kind.clone() {
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
                <span>{label}{required}{note}</span>
            </label>
        }
        .into_any(),
        FieldKind::Secret => view! {
            <label class="settings-field">
                <span>{label}{required}</span>
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
                {note}
            </label>
        }
        .into_any(),
        FieldKind::Number => view! {
            <label class="settings-field">
                <span>{label}{required}</span>
                <input
                    type="number"
                    prop:value=value
                    on:input:target=move |ev| {
                        value.set(ev.target().value());
                        touched.set(true);
                    }
                />
                {note}
            </label>
        }
        .into_any(),
        FieldKind::Choice(choices) => {
            // Nothing picked yet means "don't send this key at all", so the extension's own
            // default stands — which is not the same as the first value in the list.
            let blank = if field.required {
                "Choose one"
            } else {
                "Leave as it is"
            };
            view! {
                <label class="settings-field">
                    <span>{label}{required}</span>
                    <select
                        prop:value=value
                        on:change:target=move |ev| {
                            value.set(ev.target().value());
                            touched.set(true);
                        }
                    >
                        <option value="">{blank}</option>
                        {choices
                            .into_iter()
                            .map(|choice| view! {
                                <option value=choice.clone()>{choice.clone()}</option>
                            })
                            .collect_view()}
                    </select>
                    {note}
                </label>
            }
            .into_any()
        }
        FieldKind::Text => view! {
            <label class="settings-field">
                <span>{label}{required}</span>
                <input
                    type="text"
                    prop:value=value
                    on:input:target=move |ev| {
                        value.set(ev.target().value());
                        touched.set(true);
                    }
                />
                {note}
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
    fn an_enum_becomes_a_list_to_pick_from_not_a_text_box() {
        // schemars' own shape for a fieldless enum, behind the `$ref` it generates for it.
        let schema = serde_json::json!({
            "properties": {
                "adapter": {"$ref": "#/$defs/Adapter"},
            },
            "$defs": {
                "Adapter": {"type": "string", "enum": ["ember", "zstack"]},
            },
        });
        let fields = fields_from_schema(&schema);
        assert_eq!(
            fields[0].kind,
            FieldKind::Choice(vec!["ember".to_owned(), "zstack".to_owned()])
        );
    }

    #[test]
    fn a_fields_doc_comment_becomes_its_help_text_on_one_line() {
        let schema = serde_json::json!({
            "properties": {
                "serial_port": {
                    "type": "string",
                    "description": "The dongle's serial device,\ne.g. `/dev/ttyUSB0`.",
                },
                "port": {"type": "integer"},
            },
        });
        let fields = fields_from_schema(&schema);
        let by_key = |key: &str| fields.iter().find(|f| f.key == key).expect("present");
        assert_eq!(
            by_key("serial_port").help.as_deref(),
            Some("The dongle's serial device, e.g. `/dev/ttyUSB0`.")
        );
        assert_eq!(by_key("port").help, None);
    }

    #[test]
    fn only_settings_the_extension_can_do_without_are_folded_away() {
        let schema = serde_json::json!({
            "properties": {
                "serial_port": {"type": "string"},
                "broker_port": {"type": "integer", "default": 17883},
                "channel": {"type": ["integer", "null"]},
                // Optional, but a credential: a person opening this form is often here for it.
                "network_key": {
                    "anyOf": [{"$ref": "#/$defs/Secret"}, {"type": "null"}],
                },
            },
            "required": ["serial_port"],
            "$defs": {"Secret": {"type": "string", "writeOnly": true}},
        });
        let fields = fields_from_schema(&schema);
        let folded: Vec<&str> = fields
            .iter()
            .filter(|field| field.advanced)
            .map(|field| field.key.as_str())
            .collect();
        assert_eq!(folded, vec!["channel"]);
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
