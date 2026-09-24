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
