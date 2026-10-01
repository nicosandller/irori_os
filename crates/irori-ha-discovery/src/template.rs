//! The simple subset of Jinja `value_template` that Z2M and Tasmota actually emit:
//! `{{ value_json.foo }}`, `{{ value_json.foo.bar }}`, and `{{ value_json['foo'] }}`.
//! `docs/specs/protocols.md` is explicit that full Jinja is out of scope; anything past this
//! shape is [`ValueTemplate::Unsupported`], so the entity that used it can be skipped with a
//! reason instead of being silently misread.

/// What a `value_template` says to do with a payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueTemplate {
    /// No template: the payload itself is the value.
    None,
    /// A dotted/bracketed path into the JSON body, e.g. `["energy", "power"]`.
    JsonPath(Vec<String>),
    /// The original template text, kept so the reason an entity is skipped can quote it.
    Unsupported(String),
}

impl ValueTemplate {
    pub fn parse(template: Option<&str>) -> Self {
        let Some(template) = template else {
            return Self::None;
        };
        let unsupported = || Self::Unsupported(template.to_owned());
        let Some(inner) = template
            .trim()
            .strip_prefix("{{")
            .and_then(|s| s.strip_suffix("}}"))
        else {
            return unsupported();
        };
        let Some(mut rest) = inner.trim().strip_prefix("value_json") else {
            return unsupported();
        };
        let mut path = Vec::new();
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix('.') {
                let end = after
                    .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                if end == 0 {
                    return unsupported();
                }
                path.push(after[..end].to_owned());
                rest = &after[end..];
            } else if let Some(after) = rest.strip_prefix('[') {
                let Some(quote @ ('\'' | '"')) = after.chars().next() else {
                    return unsupported();
                };
                let after = &after[1..];
                let Some(end) = after.find(quote) else {
                    return unsupported();
                };
                let Some(after_close) = after[end + quote.len_utf8()..].strip_prefix(']') else {
                    return unsupported();
                };
                path.push(after[..end].to_owned());
                rest = after_close;
            } else {
                return unsupported();
            }
            rest = rest.trim_start();
        }
        if path.is_empty() {
            return unsupported();
        }
        Self::JsonPath(path)
    }

    /// Applies the template to a payload. `None` hands the whole payload back as a JSON string
    /// (bare text, not re-encoded) — the caller decides how to read it (a sensor tries it as a
    /// number first, `state.rs` §5.3).
    pub fn extract(&self, payload: &[u8]) -> Result<serde_json::Value, String> {
        match self {
            Self::None => Ok(serde_json::Value::String(
                String::from_utf8_lossy(payload).into_owned(),
            )),
            Self::JsonPath(path) => {
                let root: serde_json::Value = serde_json::from_slice(payload)
                    .map_err(|e| format!("payload isn't JSON: {e}"))?;
                let mut current = &root;
                for key in path {
                    current = current
                        .get(key)
                        .ok_or_else(|| format!("no `{key}` in the payload"))?;
                }
                Ok(current.clone())
            }
            Self::Unsupported(text) => Err(format!(
                "`{text}` isn't a supported value_template (only {{ value_json.foo }} forms are)"
            )),
        }
    }
}

/// A `command_template` (or `set_position_template`, …) Irori can render: one value put into
/// fixed text, like Zigbee2MQTT's `{ "position": {{ position }} }`. Anything more — filters,
/// arithmetic, `{% %}` — is Jinja, which Irori doesn't run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandTemplate {
    /// No template, or one that is only the value: the value is sent as it is.
    Value,
    /// The value between fixed text.
    Around { before: String, after: String },
}

impl CommandTemplate {
    /// `template` with its one placeholder named `variable` (`value`, `position`). `Err` quotes
    /// what Irori can't render.
    pub fn parse(template: Option<&str>, variable: &str) -> Result<Self, String> {
        let Some(template) = template else {
            return Ok(Self::Value);
        };
        let refuse = || {
            format!(
                "its template {template:?} is more than the {variable}, which Irori can't render"
            )
        };
        if template.contains("{%") || template.matches("{{").count() != 1 {
            return Err(refuse());
        }
        let (before, rest) = template.split_once("{{").ok_or_else(refuse)?;
        let (inside, after) = rest.split_once("}}").ok_or_else(refuse)?;
        if inside.trim() != variable || after.contains("}}") {
            return Err(refuse());
        }
        if before.trim().is_empty() && after.trim().is_empty() {
            Ok(Self::Value)
        } else {
            Ok(Self::Around {
                before: before.to_owned(),
                after: after.to_owned(),
            })
        }
    }

    /// The payload for `value`.
    pub fn render(&self, value: &str) -> String {
        match self {
            Self::Value => value.to_owned(),
            Self::Around { before, after } => format!("{before}{value}{after}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_dotted_and_bracketed_forms() {
        assert_eq!(
            ValueTemplate::parse(Some("{{ value_json.temperature }}")),
            ValueTemplate::JsonPath(vec!["temperature".to_owned()])
        );
        assert_eq!(
            ValueTemplate::parse(Some("{{value_json.energy.power}}")),
            ValueTemplate::JsonPath(vec!["energy".to_owned(), "power".to_owned()])
        );
        assert_eq!(
            ValueTemplate::parse(Some("{{ value_json['battery'] }}")),
            ValueTemplate::JsonPath(vec!["battery".to_owned()])
        );
        assert_eq!(ValueTemplate::parse(None), ValueTemplate::None);
    }

    #[test]
    fn a_command_template_that_only_places_the_value_is_rendered() {
        let position =
            CommandTemplate::parse(Some(r#"{ "position": {{ position }} }"#), "position")
                .expect("renderable");
        assert_eq!(position.render("40"), r#"{ "position": 40 }"#);
        assert_eq!(
            CommandTemplate::parse(Some("{{value}}"), "value"),
            Ok(CommandTemplate::Value)
        );
        assert_eq!(
            CommandTemplate::parse(None, "value"),
            Ok(CommandTemplate::Value)
        );
        for jinja in [
            "{{ value * 10 }}",
            "{% if value %}ON{% endif %}",
            "{{ value }}{{ value }}",
            "{{ position }}",
        ] {
            assert!(
                CommandTemplate::parse(Some(jinja), "value").is_err(),
                "{jinja}"
            );
        }
    }

    #[test]
    fn anything_beyond_the_simple_form_is_unsupported_not_a_crash() {
        for text in [
            "{{ value_json.a if value_json.a else 'x' }}",
            "{{ value_json.a | round(1) }}",
            "{{ 1 + 1 }}",
            "not a template at all",
        ] {
            assert!(matches!(
                ValueTemplate::parse(Some(text)),
                ValueTemplate::Unsupported(_)
            ));
        }
    }

    #[test]
    fn extracts_a_nested_value_and_reports_a_missing_one() {
        let template = ValueTemplate::parse(Some("{{ value_json.energy.power }}"));
        let ok = template.extract(br#"{"energy": {"power": 42.5}}"#);
        assert_eq!(ok, Ok(serde_json::json!(42.5)));

        let missing = template.extract(br#"{"energy": {}}"#);
        assert!(missing.is_err_and(|e| e.contains("no `power`")));
    }

    #[test]
    fn no_template_hands_back_the_raw_payload_as_text() {
        let extracted = ValueTemplate::None.extract(b"21.5").expect("ok");
        assert_eq!(extracted, serde_json::json!("21.5"));
    }
}
