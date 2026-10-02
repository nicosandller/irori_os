//! The simple subset of Jinja `value_template` that Z2M and Tasmota actually emit:
//! `{{ value_json.foo }}`, `{{ value_json.foo.bar }}`, and `{{ value_json['foo'] }}`, optionally
//! ending in `| default(…)`, and a fixed lookup table in front of one,
//! `{{ {'off': 0, 'low': 1}[value_json["fan_mode"]] }}` (Zigbee2MQTT's fan speeds).
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
    /// The same, ending in `| default(…)`: a body without it is no value rather than an error.
    JsonPathOrNothing(Vec<String>),
    /// What's at `path`, looked up in a fixed table; anything not in it is no value.
    Lookup {
        path: Vec<String>,
        table: Vec<(String, serde_json::Value)>,
    },
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
        let (expression, defaulted) = without_default(inner.trim());
        if expression.starts_with('{') {
            let Some((table, rest)) = literal_table(expression) else {
                return unsupported();
            };
            let Some(path) = rest
                .trim()
                .strip_prefix('[')
                .and_then(|r| r.strip_suffix(']'))
                .and_then(|r| json_path(r.trim()))
            else {
                return unsupported();
            };
            return Self::Lookup {
                path,
                table: table
                    .into_iter()
                    .map(|(key, value)| (key.as_text(), value.into_json()))
                    .collect(),
            };
        }
        match json_path(expression) {
            Some(path) if defaulted => Self::JsonPathOrNothing(path),
            Some(path) => Self::JsonPath(path),
            None => unsupported(),
        }
    }

    /// The first `value_json` path in a template too complex to run, for a value that is only
    /// kept when it's one of a known list anyway: Zigbee2MQTT's fan preset template is "the mode,
    /// if it's one of these presets", and reading the mode and checking it against the presets is
    /// the same thing.
    pub fn first_path(template: &str) -> Option<Self> {
        let start = template.find("value_json")?;
        let rest = &template[start..];
        let end = rest
            .char_indices()
            .scan((0_i32, false), |(depth, quoted), (i, c)| {
                match c {
                    '[' if !*quoted => *depth += 1,
                    ']' if !*quoted => *depth -= 1,
                    '"' | '\'' => *quoted = !*quoted,
                    _ => {}
                }
                let stop = *depth == 0 && !*quoted && (c.is_whitespace() || c == '}' || c == '|');
                Some((i, stop))
            })
            .find(|(_, stop)| *stop)
            .map_or(rest.len(), |(i, _)| i);
        json_path(&rest[..end]).map(Self::JsonPathOrNothing)
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
            Self::JsonPathOrNothing(path) => Ok(at(payload, path)?.unwrap_or_default()),
            Self::Lookup { path, table } => {
                let Some(found) = at(payload, path)? else {
                    return Ok(serde_json::Value::Null);
                };
                let key = match found {
                    serde_json::Value::String(text) => text,
                    other => other.to_string(),
                };
                Ok(table
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, value)| value.clone())
                    .unwrap_or_default())
            }
            Self::Unsupported(text) => Err(format!(
                "`{text}` isn't a supported value_template (only {{ value_json.foo }} forms are)"
            )),
        }
    }
}

/// What's at `path` in a JSON body, `None` if it isn't there.
fn at(payload: &[u8], path: &[String]) -> Result<Option<serde_json::Value>, String> {
    let root: serde_json::Value =
        serde_json::from_slice(payload).map_err(|e| format!("payload isn't JSON: {e}"))?;
    let mut current = &root;
    for key in path {
        match current.get(key) {
            Some(next) => current = next,
            None => return Ok(None),
        }
    }
    Ok(Some(current.clone()))
}

/// `value_json.a.b` or `value_json["a"]` as its keys.
fn json_path(expression: &str) -> Option<Vec<String>> {
    let mut rest = expression.strip_prefix("value_json")?;
    let mut path = Vec::new();
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('.') {
            let end = after
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            if end == 0 {
                return None;
            }
            path.push(after[..end].to_owned());
            rest = &after[end..];
        } else {
            let after = rest.strip_prefix('[')?;
            let quote @ ('\'' | '"') = after.chars().next()? else {
                return None;
            };
            let after = &after[1..];
            let end = after.find(quote)?;
            let after_close = after[end + quote.len_utf8()..].strip_prefix(']')?;
            path.push(after[..end].to_owned());
            rest = after_close;
        }
        rest = rest.trim_start();
    }
    (!path.is_empty()).then_some(path)
}

/// `x | default(…)` as `(x, true)`; anything else as it is. `default` only says what a missing
/// value becomes, so dropping it loses nothing Irori needs.
fn without_default(expression: &str) -> (&str, bool) {
    match expression.rsplit_once('|') {
        Some((before, filter))
            if filter.trim().starts_with("default(") && filter.trim().ends_with(')') =>
        {
            (before.trim(), true)
        }
        _ => (expression, false),
    }
}

/// One literal in a lookup table: a quoted string or a whole number.
#[derive(Debug, Clone, PartialEq)]
enum Literal {
    Text(String),
    Number(i64),
}

impl Literal {
    fn as_text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Number(n) => n.to_string(),
        }
    }

    fn into_json(self) -> serde_json::Value {
        match self {
            Self::Text(text) => serde_json::Value::String(text),
            Self::Number(n) => n.into(),
        }
    }
}

/// A Jinja dict of literals at the start of `text`, `{'off': 0, 'low': 1}`, and what follows it.
fn literal_table(text: &str) -> Option<(Vec<(Literal, Literal)>, &str)> {
    let mut rest = text.strip_prefix('{')?.trim_start();
    let mut table = Vec::new();
    loop {
        if let Some(after) = rest.strip_prefix('}') {
            return Some((table, after));
        }
        let (key, after) = literal(rest)?;
        let after = after.trim_start().strip_prefix(':')?.trim_start();
        let (value, after) = literal(after)?;
        table.push((key, value));
        rest = after.trim_start();
        if let Some(after) = rest.strip_prefix(',') {
            rest = after.trim_start();
        }
    }
}

fn literal(text: &str) -> Option<(Literal, &str)> {
    if let Some(quote @ ('\'' | '"')) = text.chars().next() {
        let body = &text[1..];
        let end = body.find(quote)?;
        return Some((Literal::Text(body[..end].to_owned()), &body[end + 1..]));
    }
    let end = text
        .find(|c: char| !(c.is_ascii_digit() || c == '-'))
        .unwrap_or(text.len());
    let number = text[..end].parse().ok()?;
    Some((Literal::Number(number), &text[end..]))
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
    /// The value looked up in a fixed table, `{0: 'off', 1: 'low'}[value]`: Zigbee2MQTT's fan
    /// speeds. A value not in it sends nothing.
    Lookup(Vec<(String, String)>),
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
        let (inside, after) = rest.rsplit_once("}}").ok_or_else(refuse)?;
        let (inside, _) = without_default(inside.trim());
        if inside.starts_with('{') && before.trim().is_empty() && after.trim().is_empty() {
            let (table, rest) = literal_table(inside).ok_or_else(refuse)?;
            if rest.trim() != format!("[{variable}]") {
                return Err(refuse());
            }
            return Ok(Self::Lookup(
                table
                    .into_iter()
                    .map(|(key, value)| (key.as_text(), value.as_text()))
                    .collect(),
            ));
        }
        if inside != variable || after.contains("}}") {
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
            Self::Lookup(table) => table
                .iter()
                .find(|(key, _)| key == value)
                .map(|(_, text)| text.clone())
                .unwrap_or_default(),
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

    /// Zigbee2MQTT's mode-controlled fans: speeds named by mode, translated both ways.
    #[test]
    fn a_fixed_lookup_table_is_read_both_ways() {
        let reading = ValueTemplate::parse(Some(
            "{{ {'off':0, 'low':1, 'medium':2, 'high':3}[value_json[\"fan_mode\"]] | default('None') }}",
        ));
        assert_eq!(reading.extract(br#"{"fan_mode": "medium"}"#), Ok(2.into()));
        assert_eq!(
            reading.extract(br#"{"fan_mode": "auto"}"#),
            Ok(serde_json::Value::Null),
            "a mode that isn't a speed"
        );
        let command = CommandTemplate::parse(
            Some("{{ {0:'off', 1:'low', 2:'medium', 3:'high'}[value] | default('') }}"),
            "value",
        )
        .expect("a lookup");
        assert_eq!(command.render("3"), "high");
        assert_eq!(command.render("9"), "");

        let speed = ValueTemplate::parse(Some("{{ value_json[\"speed\"] | default('None') }}"));
        assert_eq!(speed.extract(br#"{"speed": 4}"#), Ok(4.into()));
        assert_eq!(
            speed.extract(br#"{"state": "ON"}"#),
            Ok(serde_json::Value::Null)
        );
        assert_eq!(
            CommandTemplate::parse(Some("{{ value | default('') }}"), "value"),
            Ok(CommandTemplate::Value)
        );
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
