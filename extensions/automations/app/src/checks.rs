//! A condition to fill in rather than write: things checked against values, joined by "and" or
//! "or". It's written as the condition it stands for — `state` for on/off and text, an
//! expression for numbers, `all`/`any` around several — so a run's trace shows every value it
//! read, and an unavailable device counts the way a condition on it always has. A condition
//! that isn't one of these is edited as an expression, or as JSON.

use irori_types::{EntityId, SensorValueType};
use leptos::prelude::*;
use serde_json::{Value, json};

use crate::inspector::{EntityPicker, WATCHABLE};
use crate::widgets::{Choice, Combo};
use crate::{Home, model};

/// How one thing is checked.
#[derive(Debug, Clone, PartialEq)]
pub enum Test {
    /// A number: `num('…') < 30`.
    Num { op: &'static str, value: f64 },
    /// On or off, open or closed…
    Flag(bool),
    /// A text, or not that text.
    Text { equal: bool, value: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Clause {
    pub entity: String,
    pub test: Test,
}

/// Every check, and whether any one holding is enough.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Checks {
    pub clauses: Vec<Clause>,
    pub any: bool,
}

pub const NUM_OPS: [(&str, &str); 6] = [
    ("<", "is below"),
    ("<=", "is at most"),
    ("==", "is exactly"),
    ("!=", "is not"),
    (">=", "is at least"),
    (">", "is above"),
];

/// The id inside `f('…')` at the start of `text`, and what follows it.
fn call<'a>(text: &'a str, f: &str) -> Option<(&'a str, &'a str)> {
    let rest = text.strip_prefix(f)?.strip_prefix('(')?.trim_start();
    let quote = rest.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let rest = &rest[1..];
    let end = rest.find(quote)?;
    let id = &rest[..end];
    let after = rest[end + 1..].trim_start().strip_prefix(')')?;
    id.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        .then_some((id, after.trim()))
}

fn quoted(text: &str) -> Option<String> {
    let quote = text.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let inner = text[1..].strip_suffix(quote)?;
    (!inner.contains(['\'', '"', '\\'])).then(|| inner.to_owned())
}

impl Clause {
    /// One check written as an expression: `num('…') < 30`, `on('…')`, `!on('…')`,
    /// `text('…') == 'x'`.
    fn parse_expr(text: &str) -> Option<Self> {
        let text = text.trim();
        let clause = |id: &str, test| {
            Some(Self {
                entity: id.into(),
                test,
            })
        };
        if let Some(rest) = text.strip_prefix('!')
            && let Some((id, "")) = call(rest.trim_start(), "on")
        {
            return clause(id, Test::Flag(false));
        }
        if let Some((id, "")) = call(text, "on") {
            return clause(id, Test::Flag(true));
        }
        if let Some((id, rest)) = call(text, "num") {
            // Longest first, so `<=` isn't read as `<`.
            let op = ["<=", ">=", "==", "!=", "<", ">"]
                .into_iter()
                .find(|op| rest.starts_with(op))?;
            let value: f64 = rest[op.len()..].trim().parse().ok()?;
            let op = NUM_OPS.iter().find(|(o, _)| *o == op)?.0;
            return clause(id, Test::Num { op, value });
        }
        if let Some((id, rest)) = call(text, "text") {
            let (equal, value) = match rest.strip_prefix("==") {
                Some(value) => (true, value),
                None => (false, rest.strip_prefix("!=")?),
            };
            return clause(
                id,
                Test::Text {
                    equal,
                    value: quoted(value.trim())?,
                },
            );
        }
        None
    }

    /// One check from a condition: a `state` without availability, the `not` of one, or an
    /// expression that's one check.
    fn from_condition(condition: &Value) -> Option<Self> {
        match condition["type"].as_str()? {
            "state" if condition.get("availability").is_none() => {
                let entity = condition["entity"].as_str()?.to_owned();
                let test = match &condition["is"] {
                    Value::Bool(on) => Test::Flag(*on),
                    Value::String(text) => Test::Text {
                        equal: true,
                        value: text.clone(),
                    },
                    Value::Number(n) => Test::Num {
                        op: "==",
                        value: n.as_f64()?,
                    },
                    _ => return None,
                };
                Some(Self { entity, test })
            }
            "not" => {
                let inner = Self::from_condition(&condition["condition"])?;
                let test = match inner.test {
                    Test::Flag(on) => Test::Flag(!on),
                    Test::Text { equal, value } => Test::Text {
                        equal: !equal,
                        value,
                    },
                    // `not num < 30` isn't `num >= 30` when the sensor can't be read.
                    Test::Num { .. } => return None,
                };
                Some(Self {
                    entity: inner.entity,
                    test,
                })
            }
            "expr" => Self::parse_expr(condition["expr"].as_str()?),
            _ => None,
        }
    }

    /// The condition this check is.
    fn render(&self) -> Value {
        let entity = &self.entity;
        match &self.test {
            Test::Flag(on) => json!({ "type": "state", "entity": entity, "is": on }),
            Test::Text { equal: true, value } => {
                json!({ "type": "state", "entity": entity, "is": value })
            }
            Test::Text {
                equal: false,
                value,
            } => json!({ "type": "not", "condition":
                { "type": "state", "entity": entity, "is": value } }),
            Test::Num { op, value } => {
                json!({ "type": "expr", "expr": format!("num('{entity}') {op} {value}") })
            }
        }
    }

    /// A first check for `entity`, fitting what kind of thing it is.
    pub fn fresh(entity: &str, home: &Home) -> Self {
        let test = match sensor_type(entity, home) {
            Some(SensorValueType::Number) => Test::Num {
                op: "<",
                value: 30.0,
            },
            Some(SensorValueType::Text) => Test::Text {
                equal: true,
                value: entity
                    .parse::<EntityId>()
                    .ok()
                    .and_then(|id| home.texts(&id).into_iter().next())
                    .unwrap_or_default(),
            },
            None => Test::Flag(true),
        };
        Self {
            entity: entity.to_owned(),
            test,
        }
    }

    fn words(&self, home: &Home) -> String {
        let id = self.entity.parse::<EntityId>().ok();
        let name = id
            .as_ref()
            .map_or_else(|| self.entity.clone(), |id| home.name(id));
        match &self.test {
            Test::Flag(on) => {
                let (yes, no) = id.as_ref().map_or(("on", "off"), |id| home.flag_words(id));
                format!("{name} {}", if *on { yes } else { no })
            }
            Test::Num { op, value } => {
                let op = match *op {
                    "<" => "below",
                    "<=" => "at most",
                    "==" => "is",
                    "!=" => "isn't",
                    ">=" => "at least",
                    _ => "above",
                };
                format!("{name} {op} {value}")
            }
            Test::Text { equal, value } => {
                format!("{name} {} “{value}”", if *equal { "is" } else { "isn't" })
            }
        }
    }
}

fn sensor_type(entity: &str, home: &Home) -> Option<SensorValueType> {
    home.entities.with_untracked(|entities| {
        entities
            .iter()
            .find(|e| e.id.as_str() == entity)
            .and_then(|e| match &e.capabilities {
                irori_types::Capabilities::Sensor(s) => Some(s.value_type),
                _ => None,
            })
    })
}

impl Checks {
    /// The checks a condition is, or `None` if it's more than checks can say.
    pub fn from_condition(condition: &Value) -> Option<Self> {
        match condition["type"].as_str()? {
            kind @ ("all" | "any") => {
                let clauses = condition["conditions"]
                    .as_array()?
                    .iter()
                    .map(Clause::from_condition)
                    .collect::<Option<Vec<_>>>()?;
                (!clauses.is_empty()).then_some(Self {
                    clauses,
                    any: kind == "any",
                })
            }
            // Written before there were checks: several joined in one expression.
            "expr" => Self::parse_expr(condition["expr"].as_str()?),
            _ => Clause::from_condition(condition).map(|clause| Self {
                clauses: vec![clause],
                any: false,
            }),
        }
    }

    /// Checks joined in one expression, all by `&&` or all by `||`.
    fn parse_expr(expr: &str) -> Option<Self> {
        let expr = expr.trim();
        let (any, parts): (bool, Vec<&str>) = match (expr.contains("&&"), expr.contains("||")) {
            (true, true) => return None,
            (false, true) => (true, expr.split("||").collect()),
            _ => (false, expr.split("&&").collect()),
        };
        let clauses = parts
            .into_iter()
            .map(Clause::parse_expr)
            .collect::<Option<Vec<_>>>()?;
        Some(Self { clauses, any })
    }

    /// The condition these checks are.
    pub fn render(&self) -> Value {
        match self.clauses.as_slice() {
            [] => json!({ "type": "expr", "expr": "true" }),
            [one] => one.render(),
            many => json!({
                "type": if self.any { "any" } else { "all" },
                "conditions": many.iter().map(Clause::render).collect::<Vec<_>>(),
            }),
        }
    }

    /// Something to start from: the first sensor with a number, if there is one.
    pub fn starter(home: &Home) -> Self {
        let entity = home.entities.with_untracked(|entities| {
            entities
                .iter()
                .find(|e| {
                    matches!(&e.capabilities, irori_types::Capabilities::Sensor(s)
                        if s.value_type == SensorValueType::Number)
                })
                .or_else(|| entities.iter().find(|e| WATCHABLE.contains(&e.id.kind())))
                .map(|e| e.id.to_string())
        });
        Self {
            clauses: entity
                .map(|e| Clause::fresh(&e, home))
                .into_iter()
                .collect(),
            any: false,
        }
    }

    /// The checks as a sentence: "Illuminance below 600 and Occupancy detected".
    pub fn words(&self, home: &Home) -> String {
        self.clauses
            .iter()
            .map(|clause| clause.words(home))
            .collect::<Vec<_>>()
            .join(if self.any { " or " } else { " and " })
    }
}

/// A simple expression as a sentence, or `None` for one that isn't checks.
pub fn expr_words(expr: &str, home: &Home) -> Option<String> {
    Checks::parse_expr(expr).map(|checks| checks.words(home))
}

/// A text a sensor can have: what it has said over the last day offered to pick, or typed.
#[component]
pub fn TextValue(
    entity: String,
    value: String,
    #[prop(optional)] allow_any: bool,
    pick: impl Fn(String) + Send + Sync + 'static,
) -> impl IntoView {
    let home = expect_context::<Home>();
    let choices = Signal::derive(move || {
        let mut choices: Vec<Choice> = entity
            .parse::<EntityId>()
            .map(|id| home.texts(&id))
            .unwrap_or_default()
            .into_iter()
            .map(|text| Choice::new(text.clone(), text))
            .collect();
        let now = entity.parse::<EntityId>().ok().and_then(|id| {
            home.states.with(
                |states| match states.get(&id).and_then(|s| s.state.as_ref()) {
                    Some(irori_types::State::Sensor(sensor)) => match &sensor.value {
                        irori_types::SensorValue::Text(text) => Some(text.clone()),
                        irori_types::SensorValue::Number(_) => None,
                    },
                    _ => None,
                },
            )
        });
        for choice in &mut choices {
            if now.as_deref() == Some(choice.value.as_str()) {
                choice.detail = "now".into();
            }
        }
        if allow_any {
            choices.insert(0, Choice::new("", "any change"));
        }
        choices
    });
    view! {
        <Combo
            choices=choices
            value=Signal::stored(value)
            custom=true
            placeholder="Pick or type a value…"
            pick=Callback::new(move |text: String| {
                pick(text.trim().chars().filter(|c| !matches!(c, '\'' | '"' | '\\')).collect())
            })
        />
    }
}

/// The checks to fill in: for each thing, a test that fits it — below or above a number, on or
/// off, a text or not — joined by "and" or "or".
#[component]
pub fn ChecksForm(
    checks: Checks,
    edit: impl Fn(Box<dyn FnOnce(&mut Value)>) + Clone + Send + Sync + 'static,
) -> impl IntoView {
    let home = expect_context::<Home>();
    let count = checks.clauses.len();
    let put = move |next: Checks| {
        let condition = next.render();
        edit(Box::new(move |v: &mut Value| *v = condition));
    };
    let rows = checks
        .clauses
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, clause)| {
            let now = clause.entity.parse::<EntityId>().ok().and_then(|id| {
                home.states.with_untracked(|states| {
                    states.get(&id).map(|s| model::state_words(s, &home))
                })
            });
            let with = {
                let (checks, put) = (checks.clone(), put.clone());
                move |f: &dyn Fn(&mut Checks)| {
                    let mut next = checks.clone();
                    f(&mut next);
                    put(next);
                }
            };
            let test_input = test_input(&clause, i, with.clone(), home);
            let joiner = (i > 0).then(|| {
                let any = checks.any;
                let with = with.clone();
                view! {
                    <div class="joiner">
                        <button type="button" class="join-toggle"
                            title="Switch every check between “and” and “or”"
                            on:click=move |_| with(&|c: &mut Checks| c.any = !c.any)>
                            <span class:on=!any>"and"</span>
                            <span class:on=any>"or"</span>
                        </button>
                    </div>
                }
            });
            let (with_entity, with_remove) = (with.clone(), with);
            let previous = clause.test.clone();
            view! {
                {joiner}
                <div class="clause">
                    <div class="row">
                        <div class="grow">
                            <EntityPicker value=clause.entity.clone() kinds=WATCHABLE.to_vec()
                                pick=move |id: String| {
                                    let mut fresh = Clause::fresh(&id, &home);
                                    // The same kind of test as before, where it still fits.
                                    if std::mem::discriminant(&fresh.test)
                                        == std::mem::discriminant(&previous)
                                        && !matches!(previous, Test::Text { .. })
                                    {
                                        fresh.test = previous.clone();
                                    }
                                    with_entity(&|c: &mut Checks| c.clauses[i] = fresh.clone());
                                } />
                        </div>
                        {(count > 1).then(|| view! {
                            <button class="btn small" title="Remove this check"
                                on:click=move |_| with_remove(&|c: &mut Checks| { c.clauses.remove(i); })>
                                "×"
                            </button>
                        })}
                    </div>
                    <div class="row test">{test_input}</div>
                    {now.map(|n| view! { <div class="muted now-line">{format!("now {n}")}</div> })}
                </div>
            }
        })
        .collect_view();
    let add = {
        let checks = checks.clone();
        move |_| {
            let mut next = checks.clone();
            if let Some(clause) = Checks::starter(&home).clauses.into_iter().next() {
                next.clauses.push(clause);
            }
            put(next);
        }
    };
    view! {
        {rows}
        <button class="btn small" on:click=add>"Add another check"</button>
    }
}

/// The inputs for one check's test.
fn test_input(
    clause: &Clause,
    i: usize,
    with: impl Fn(&dyn Fn(&mut Checks)) + Clone + Send + Sync + 'static,
    home: Home,
) -> AnyView {
    match clause.test.clone() {
        Test::Num { op, value } => {
            let (with_op, with_value) = (with.clone(), with);
            view! {
                <select on:change=move |e| {
                    let picked = event_target_value(&e);
                    if let Some((op, _)) = NUM_OPS.iter().find(|(o, _)| *o == picked) {
                        with_op(&|c: &mut Checks| c.clauses[i].test = Test::Num { op, value });
                    }
                }>
                    {NUM_OPS.iter().map(|(o, words)| view! {
                        <option value=*o selected=*o == op>{*words}</option>
                    }).collect_view()}
                </select>
                <input type="number" step="any" prop:value=value.to_string()
                    on:change=move |e| {
                        let Ok(value) = event_target_value(&e).trim().parse::<f64>() else { return };
                        with_value(&|c: &mut Checks| c.clauses[i].test = Test::Num { op, value });
                    } />
            }
            .into_any()
        }
        Test::Flag(on) => {
            let (yes, no) = clause
                .entity
                .parse::<EntityId>()
                .map_or(("on", "off"), |id| home.flag_words(&id));
            view! {
                <select on:change=move |e| {
                    let on = event_target_value(&e) == "on";
                    with(&|c: &mut Checks| c.clauses[i].test = Test::Flag(on));
                }>
                    <option value="on" selected=on>{format!("is {yes}")}</option>
                    <option value="off" selected=!on>{format!("is {no}")}</option>
                </select>
            }
            .into_any()
        }
        Test::Text { equal, value } => {
            let (with_equal, with_value) = (with.clone(), with);
            let kept = value.clone();
            view! {
                <select on:change=move |e| {
                    let equal = event_target_value(&e) == "is";
                    let value = kept.clone();
                    with_equal(&|c: &mut Checks| {
                        c.clauses[i].test = Test::Text { equal, value: value.clone() };
                    });
                }>
                    <option value="is" selected=equal>"is"</option>
                    <option value="not" selected=!equal>"is not"</option>
                </select>
                <div class="grow">
                    <TextValue entity=clause.entity.clone() value=value
                        pick=move |text: String| with_value(&|c: &mut Checks| {
                            c.clauses[i].test = Test::Text { equal, value: text.clone() };
                        }) />
                </div>
            }
            .into_any()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(condition: Value) {
        let checks = Checks::from_condition(&condition).unwrap_or_else(|| panic!("{condition}"));
        assert_eq!(checks.render(), condition);
    }

    #[test]
    fn conditions_read_as_checks_and_write_back_the_same() {
        round_trip(json!({ "type": "state", "entity": "switch.night", "is": true }));
        round_trip(json!({ "type": "expr", "expr": "num('sensor.lux') < 600" }));
        round_trip(json!({ "type": "not", "condition":
            { "type": "state", "entity": "sensor.tv", "is": "playing" } }));
        round_trip(json!({ "type": "all", "conditions": [
            { "type": "state", "entity": "binary_sensor.sofa", "is": true },
            { "type": "not", "condition": { "type": "state", "entity": "sensor.tv", "is": "playing" } },
            { "type": "expr", "expr": "num('sensor.lux') >= 12.5" },
        ] }));
        round_trip(json!({ "type": "any", "conditions": [
            { "type": "state", "entity": "switch.a", "is": false },
            { "type": "state", "entity": "switch.b", "is": true },
        ] }));
    }

    #[test]
    fn an_expression_of_checks_is_read_as_checks() {
        let checks = Checks::from_condition(&json!({ "type": "expr",
            "expr": "!on('binary_sensor.door') || text('sensor.washer') == 'rinse'" }))
        .unwrap();
        assert!(checks.any);
        assert_eq!(checks.clauses[0].test, Test::Flag(false));
        assert_eq!(
            checks.render(),
            json!({ "type": "any", "conditions": [
                { "type": "state", "entity": "binary_sensor.door", "is": false },
                { "type": "state", "entity": "sensor.washer", "is": "rinse" },
            ] })
        );
    }

    #[test]
    fn anything_more_stays_an_expression() {
        for condition in [
            json!({ "type": "expr", "expr": "num('sensor.a') < 3 && on('switch.b') || on('switch.c')" }),
            json!({ "type": "expr", "expr": "num('sensor.a') + 2 < 3" }),
            json!({ "type": "expr", "expr": "var('x') == 1" }),
            json!({ "type": "state", "entity": "sensor.a", "availability": "unavailable" }),
            json!({ "type": "not", "condition": { "type": "expr", "expr": "num('sensor.a') < 3" } }),
            json!({ "type": "all", "conditions": [
                { "type": "any", "conditions": [{ "type": "state", "entity": "switch.a", "is": true }] }
            ] }),
            json!({ "type": "time", "after": "19:45" }),
        ] {
            assert_eq!(Checks::from_condition(&condition), None, "{condition}");
        }
    }
}
