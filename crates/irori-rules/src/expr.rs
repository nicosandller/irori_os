//! CEL expressions against a [`StateView`] (M0.8 spike).
//!
//! Author-facing surface the spec should document, if this crate stays:
//! - `num("sensor.foo")` — numeric reading as a float
//! - `on("binary_sensor.foo")` / `on("switch.foo")` — boolean `on`
//! - `available("sensor.foo")` — whether the entity is reachable
//!
//! CEL itself uses double or single quotes for strings. Mixed int/float comparison works
//! (`num("x") < 30` is fine).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use cel::{Context, FunctionContext, Program, ResolveResult, Value};

/// A compiled CEL expression. Compile once; evaluate many times.
#[derive(Debug)]
pub struct Compiled {
    source: String,
    program: Program,
}

impl Compiled {
    pub(crate) fn program_ref(&self) -> &Program {
        &self.program
    }
}

/// What `num` / `on` / `available` read. The real engine will wrap core state; the spike
/// uses this so expressions can be tested without the core.
pub trait StateView {
    fn reading(&self, entity_id: &str) -> Option<Reading>;
}

/// One entity's current reading, including availability and "never reported" (`None`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reading {
    Number { value: Option<f64>, available: bool },
    Flag { on: Option<bool>, available: bool },
}

impl StateView for BTreeMap<String, Reading> {
    fn reading(&self, entity_id: &str) -> Option<Reading> {
        self.get(entity_id).copied()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExprError {
    Parse(String),
    Exec(String),
}

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(message) | Self::Exec(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ExprError {}

/// What `var(…)` is called inside CEL, where `var` is a reserved word. Authors write `var`;
/// [`compile`] renames it on the way in.
pub(crate) const VAR_FN: &str = "irori_var";

/// The name an author wrote for a function CEL knows by another name.
pub(crate) fn author_name(name: &str) -> &str {
    if name == VAR_FN { "var" } else { name }
}

/// `var(` outside string literals becomes `irori_var(`, and a whole number `30` becomes `30.0`;
/// everything else is kept as written.
fn rename_reserved(source: &str) -> String {
    let mut out = String::with_capacity(source.len() + 8);
    let chars: Vec<char> = source.chars().collect();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
            out.push(c);
            i += 1;
            continue;
        }
        let starts_word = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
        // A whole number is written as a decimal, so every number is the same kind: CEL won't
        // mix `70 - 12.5`, and `10 / 4` would be 2.
        if starts_word && c.is_ascii_digit() && (i == 0 || chars[i - 1] != '.') {
            let mut j = i;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            let whole = chars
                .get(j)
                .is_none_or(|next| !(next.is_alphanumeric() || *next == '_' || *next == '.'));
            out.extend(&chars[i..j]);
            if whole {
                out.push_str(".0");
            }
            i = j;
            continue;
        }
        if starts_word && chars[i..].starts_with(&['v', 'a', 'r']) {
            let mut j = i + 3;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let ends_word = chars
                .get(i + 3)
                .is_none_or(|next| !(next.is_alphanumeric() || *next == '_'));
            if ends_word && chars.get(j) == Some(&'(') {
                out.push_str(VAR_FN);
                i += 3;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Compile `source` as CEL. Does not type-check against a registry; that's a later layer.
pub fn compile(source: &str) -> Result<Compiled, ExprError> {
    let program = Program::compile(&rename_reserved(source))
        .map_err(|errors| ExprError::Parse(errors.to_string()))?;
    Ok(Compiled {
        source: source.to_owned(),
        program,
    })
}

/// Evaluate a compiled expression to a boolean, the only result a condition may produce.
pub fn eval_bool<S: StateView + Clone + Send + Sync + 'static>(
    compiled: &Compiled,
    state: &S,
) -> Result<bool, ExprError> {
    let value = eval(compiled, state)?;
    match value {
        Value::Bool(value) => Ok(value),
        other => Err(ExprError::Exec(format!(
            "expression `{}` returned {other:?}, not a boolean",
            compiled.source
        ))),
    }
}

fn eval<S: StateView + Clone + Send + Sync + 'static>(
    compiled: &Compiled,
    state: &S,
) -> Result<Value, ExprError> {
    let mut context = Context::default();
    bind_functions(&mut context, state.clone());
    compiled
        .program
        .execute(&context)
        .map_err(|error| ExprError::Exec(error.to_string()))
}

fn bind_functions<S: StateView + Clone + Send + Sync + 'static>(context: &mut Context, state: S) {
    let for_num = state.clone();
    let for_on = state.clone();
    let for_available = state;

    context.add_function("num", move |ftx: &FunctionContext, id: Arc<String>| {
        number(&for_num, ftx, &id)
    });
    context.add_function("on", move |ftx: &FunctionContext, id: Arc<String>| {
        flag(&for_on, ftx, &id)
    });
    context.add_function(
        "available",
        move |ftx: &FunctionContext, id: Arc<String>| reachable(&for_available, ftx, &id),
    );
}

fn number(state: &impl StateView, ftx: &FunctionContext, id: &str) -> ResolveResult {
    match state.reading(id) {
        None => ftx
            .error(format!("num({id:?}): no such entity — check the entity id"))
            .into(),
        Some(Reading::Number {
            available: false, ..
        }) => ftx
            .error(format!("num({id:?}): entity is unavailable"))
            .into(),
        Some(Reading::Number { value: None, .. }) => ftx
            .error(format!("num({id:?}): entity has never reported a number"))
            .into(),
        Some(Reading::Number {
            value: Some(value), ..
        }) => Ok(Value::Float(value)),
        Some(Reading::Flag { .. }) => ftx
            .error(format!(
                "num({id:?}): entity is on/off, not a number — use on({id:?})"
            ))
            .into(),
    }
}

fn flag(state: &impl StateView, ftx: &FunctionContext, id: &str) -> ResolveResult {
    match state.reading(id) {
        None => ftx
            .error(format!("on({id:?}): no such entity — check the entity id"))
            .into(),
        Some(Reading::Flag {
            available: false, ..
        }) => ftx
            .error(format!("on({id:?}): entity is unavailable"))
            .into(),
        Some(Reading::Flag { on: None, .. }) => ftx
            .error(format!("on({id:?}): entity has never reported on/off"))
            .into(),
        Some(Reading::Flag { on: Some(on), .. }) => Ok(Value::Bool(on)),
        Some(Reading::Number { .. }) => ftx
            .error(format!(
                "on({id:?}): entity is a number, not on/off — use num({id:?})"
            ))
            .into(),
    }
}

fn reachable(state: &impl StateView, _ftx: &FunctionContext, id: &str) -> ResolveResult {
    match state.reading(id) {
        // Gone at run time is "not available", not an error: `!available('sensor.x')` is true.
        None => Ok(Value::Bool(false)),
        Some(Reading::Number { available, .. } | Reading::Flag { available, .. }) => {
            Ok(Value::Bool(available))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::time::Instant;

    const LUX: &str = "sensor.demo_luminosity_illuminance";
    const MOTION: &str = "binary_sensor.demo_movement_motion";
    const GUESTS: &str = "switch.guests_over";

    fn home(lux: f64, motion: bool, guests: bool) -> BTreeMap<String, Reading> {
        BTreeMap::from([
            (
                LUX.into(),
                Reading::Number {
                    value: Some(lux),
                    available: true,
                },
            ),
            (
                MOTION.into(),
                Reading::Flag {
                    on: Some(motion),
                    available: true,
                },
            ),
            (
                GUESTS.into(),
                Reading::Flag {
                    on: Some(guests),
                    available: true,
                },
            ),
        ])
    }

    fn dark_and_not_guests() -> Compiled {
        compile(&format!(r#"num("{LUX}") < 30.0 && !on("{GUESTS}")"#))
            .expect("hallway condition compiles")
    }

    #[test]
    fn hallway_condition_is_true_when_dark_and_no_guests() {
        let compiled = dark_and_not_guests();
        assert!(eval_bool(&compiled, &home(8.0, true, false)).unwrap());
        assert!(!eval_bool(&compiled, &home(400.0, true, false)).unwrap());
        assert!(!eval_bool(&compiled, &home(8.0, true, true)).unwrap());
    }

    #[test]
    fn single_quotes_are_strings_in_cel() {
        let compiled = compile(&format!(r#"num('{LUX}') < 30.0"#)).unwrap();
        assert!(eval_bool(&compiled, &home(8.0, true, false)).unwrap());
    }

    #[test]
    fn int_and_float_compare() {
        // The roadmap draft used `num(...) < 30` (an int). cel 0.14 accepts that against a
        // float reading; the spec can keep the unadorned integer.
        let compiled = compile(&format!(r#"num("{LUX}") < 30"#)).unwrap();
        assert!(eval_bool(&compiled, &home(8.0, true, false)).unwrap());
        assert!(!eval_bool(&compiled, &home(400.0, true, false)).unwrap());
    }

    #[test]
    fn parse_error_names_the_problem() {
        let error = compile("num(").unwrap_err();
        assert!(
            !error.to_string().is_empty(),
            "parse errors must not be blank"
        );
        // Keep this assertion tight enough that a silent "Error" isn't enough.
        let message = error.to_string();
        assert!(
            message.contains("num") || message.contains("parse") || message.contains("Parse"),
            "parse error should mention the source or that it failed to parse, got: {message}"
        );
    }

    #[test]
    fn missing_entity_says_which_id() {
        let compiled = compile(r#"num("sensor.does_not_exist") < 30.0"#).unwrap();
        let message = eval_bool(&compiled, &home(8.0, true, false))
            .unwrap_err()
            .to_string();
        assert!(message.contains("sensor.does_not_exist"), "got: {message}");
        assert!(message.contains("no such entity"), "got: {message}");
    }

    #[test]
    fn wrong_function_for_kind_suggests_the_other() {
        let compiled = compile(&format!(r#"num("{MOTION}") < 30.0"#)).unwrap();
        let message = eval_bool(&compiled, &home(8.0, true, false))
            .unwrap_err()
            .to_string();
        assert!(message.contains("use on("), "got: {message}");
    }

    #[test]
    fn unavailable_is_an_error_not_a_false() {
        let mut state = home(8.0, true, false);
        state.insert(
            LUX.into(),
            Reading::Number {
                value: Some(8.0),
                available: false,
            },
        );
        let compiled = dark_and_not_guests();
        let message = eval_bool(&compiled, &state).unwrap_err().to_string();
        assert!(message.contains("unavailable"), "got: {message}");
    }

    #[test]
    fn never_reported_is_an_error() {
        let mut state = home(8.0, true, false);
        state.insert(
            LUX.into(),
            Reading::Number {
                value: None,
                available: true,
            },
        );
        let compiled = dark_and_not_guests();
        let message = eval_bool(&compiled, &state).unwrap_err().to_string();
        assert!(message.contains("never reported"), "got: {message}");
    }

    #[test]
    fn available_is_false_when_the_entity_is_gone() {
        let compiled = compile(&format!(r#"!available("{LUX}")"#)).unwrap();
        let empty: BTreeMap<String, Reading> = BTreeMap::new();
        assert!(eval_bool(&compiled, &empty).unwrap());
    }

    #[test]
    fn eval_cost_is_well_under_a_millisecond() {
        let compiled = dark_and_not_guests();
        let state = home(8.0, true, false);
        // Warm once so the first-call cost isn't the number we report.
        eval_bool(&compiled, &state).unwrap();

        let n = 5_000_u32;
        let started = Instant::now();
        for _ in 0..n {
            assert!(eval_bool(&compiled, &state).unwrap());
        }
        let elapsed = started.elapsed();
        let per = elapsed / n;
        eprintln!("CEL eval: {n} runs in {elapsed:?} ({per:?} each, target µs)");
        assert!(
            per.as_micros() < 1_000,
            "eval took {per:?}; even a generous 1ms budget on a Mac should hold"
        );
    }
}
