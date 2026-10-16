//! Evaluating expressions, conditions and wait matchers against a snapshot of the home, at run
//! time, recording what was read (the trace's `reads`, `docs/specs/flows.md` §6).
//!
//! Unavailable and unknown never count as a value (rules.md K5): `num()` of an unavailable sensor
//! is an error, and a condition whose expression errors doesn't hold.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use cel::extractors::Arguments;
use cel::{Context, FunctionContext, ResolveResult, Value};
use irori_types::{
    Availability, EntityId, EntityState, ObjectId, State, Timestamp, Typed, ValueShape,
};
use serde::{Deserialize, Serialize};

use crate::clock::{self, Place};
use crate::expr::{Compiled, compile};
use crate::{AvailabilityWanted, Condition, TypedValue, WaitUntil};

/// What a window of time says when nobody has said what time zone the home is in.
const NO_ZONE: &str = "time and sun windows need the home's time zone";

/// What an evaluation can see: every entity's state, the run's variables, and now.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub states: Arc<BTreeMap<EntityId, EntityState>>,
    pub vars: BTreeMap<String, serde_json::Value>,
    pub now: Timestamp,
}

/// One entity as it was read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Read {
    pub entity_id: EntityId,
    /// `None` when there's no such entity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<Availability>,
    /// Which of its values was read, when not the main one: `position`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<ObjectId>,
    /// Its value, `null` if unknown.
    #[serde(default)]
    pub value: serde_json::Value,
}

impl Read {
    pub fn of(entity_id: &EntityId, states: &BTreeMap<EntityId, EntityState>) -> Self {
        Self::of_field(entity_id, None, states)
    }

    /// The entity's `field` as it is, or its main value when none is named.
    pub fn of_field(
        entity_id: &EntityId,
        field: Option<&ObjectId>,
        states: &BTreeMap<EntityId, EntityState>,
    ) -> Self {
        let state = states.get(entity_id);
        Self {
            entity_id: entity_id.clone(),
            availability: state.map(|state| state.availability),
            field: field.cloned(),
            value: state
                .and_then(|state| state.state.as_ref())
                .map(|state| read_of(state, field))
                .unwrap_or(serde_json::Value::Null),
        }
    }
}

/// The plain value of a state: `true`, `21.5`, `"rinse"`.
pub fn value_of(state: &State) -> serde_json::Value {
    state.primary().to_json()
}

/// The plain value of one of a state's fields, `null` while the entity hasn't said it; the
/// main value when no field is named.
pub fn read_of(state: &State, field: Option<&ObjectId>) -> serde_json::Value {
    match field {
        None => value_of(state),
        Some(field) => state
            .field(field.as_str())
            .map_or(serde_json::Value::Null, |value| value.to_json()),
    }
}

/// A result and what it read on the way.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome<T> {
    pub result: Result<T, String>,
    pub reads: Vec<Read>,
}

/// Compiles each expression once and keeps it.
#[derive(Debug, Default)]
pub struct Evaluator {
    compiled: HashMap<String, Arc<Compiled>>,
    /// Where the home is, for windows of time and `hour()`. `None` until it's been said.
    place: Option<Arc<Place>>,
}

impl Evaluator {
    /// Says where the home is, or that it's no longer known.
    pub fn set_place(&mut self, place: Option<Place>) {
        self.place = place.map(Arc::new);
    }

    pub fn place(&self) -> Option<&Place> {
        self.place.as_deref()
    }

    fn program(&mut self, source: &str) -> Result<Arc<Compiled>, String> {
        if let Some(compiled) = self.compiled.get(source) {
            return Ok(Arc::clone(compiled));
        }
        let compiled = Arc::new(compile(source).map_err(|e| e.to_string())?);
        self.compiled
            .insert(source.to_owned(), Arc::clone(&compiled));
        Ok(compiled)
    }

    /// Evaluates an expression to a JSON scalar.
    pub fn value(&mut self, source: &str, snapshot: &Snapshot) -> Outcome<serde_json::Value> {
        let compiled = match self.program(source) {
            Ok(compiled) => compiled,
            Err(error) => {
                return Outcome {
                    result: Err(error),
                    reads: Vec::new(),
                };
            }
        };
        let reads = Arc::new(Mutex::new(Vec::<Read>::new()));
        let mut context = Context::default();
        bind(&mut context, snapshot, &reads, self.place.clone());
        let result = compiled
            .program_ref()
            .execute(&context)
            .map_err(|e| e.to_string())
            .and_then(|value| to_json(value, source));
        let reads = std::mem::take(&mut *reads.lock().unwrap_or_else(PoisonError::into_inner));
        Outcome { result, reads }
    }

    /// Evaluates an expression that must be a boolean.
    pub fn boolean(&mut self, source: &str, snapshot: &Snapshot) -> Outcome<bool> {
        let Outcome { result, reads } = self.value(source, snapshot);
        let result = result.and_then(|value| match value {
            serde_json::Value::Bool(b) => Ok(b),
            other => Err(format!("`{source}` gave {other}, not true or false")),
        });
        Outcome { result, reads }
    }

    /// Whether a condition holds now. An error (an unavailable sensor, say) is `Err`, which the
    /// engine counts as not holding.
    pub fn condition(&mut self, condition: &Condition, snapshot: &Snapshot) -> Outcome<bool> {
        match condition {
            Condition::State {
                entity,
                field,
                is,
                availability,
            } => state_holds(entity, field.as_ref(), is.as_ref(), *availability, snapshot),
            Condition::Expr { expr } => self.boolean(expr.as_str(), snapshot),
            Condition::Time {
                after,
                before,
                weekday,
            } => Outcome {
                result: match &self.place {
                    Some(place) => Ok(clock::in_time_window(
                        place,
                        after.as_ref(),
                        before.as_ref(),
                        weekday.as_deref(),
                        snapshot.now,
                    )),
                    None => Err(NO_ZONE.to_owned()),
                },
                reads: Vec::new(),
            },
            Condition::Sun {
                after,
                before,
                offset,
                after_offset,
                before_offset,
            } => Outcome {
                result: match &self.place {
                    Some(place) => {
                        // An end's own offset, or the one that moves both.
                        let millis = |own: &Option<crate::CompactDuration>| {
                            own.as_ref()
                                .or(offset.as_ref())
                                .map_or(0, crate::CompactDuration::millis)
                        };
                        clock::in_sun_window(
                            place,
                            *after,
                            *before,
                            millis(after_offset),
                            millis(before_offset),
                            snapshot.now,
                        )
                    }
                    None => Err(NO_ZONE.to_owned()),
                },
                reads: Vec::new(),
            },
            Condition::All { conditions } | Condition::Any { conditions } => {
                // Every child is looked at, even once the answer is known, so the trace has
                // each one's reading (rules.md §6.5).
                let all = matches!(condition, Condition::All { .. });
                let mut reads = Vec::new();
                let mut decided = false;
                let mut error = None;
                for child in conditions {
                    let outcome = self.condition(child, snapshot);
                    merge(&mut reads, outcome.reads);
                    match outcome.result {
                        Ok(holds) if holds != all => decided = true,
                        Ok(_) => {}
                        Err(e) => {
                            error.get_or_insert(e);
                        }
                    }
                }
                let result = match (decided, error) {
                    (true, _) => Ok(!all),
                    // In `all`, a child that can't be read can't be said to hold.
                    (false, Some(error)) if all => Err(error),
                    // In `any`, it just isn't the one that holds.
                    _ => Ok(all),
                };
                Outcome { result, reads }
            }
            Condition::Not { condition } => {
                let outcome = self.condition(condition, snapshot);
                Outcome {
                    result: outcome.result.map(|holds| !holds),
                    reads: outcome.reads,
                }
            }
        }
    }

    /// Whether a wait's level holds now (without its `for`, which the engine times).
    pub fn wait(&mut self, until: &WaitUntil, snapshot: &Snapshot) -> Outcome<bool> {
        match until {
            WaitUntil::State {
                entity,
                field,
                is,
                availability,
                ..
            } => state_holds(entity, field.as_ref(), is.as_ref(), *availability, snapshot),
            WaitUntil::Expr { expr, .. } => self.boolean(expr.as_str(), snapshot),
        }
    }
}

fn merge(into: &mut Vec<Read>, reads: Vec<Read>) {
    for read in reads {
        if !into.iter().any(|seen| seen.entity_id == read.entity_id) {
            into.push(read);
        }
    }
}

fn state_holds(
    entity: &EntityId,
    field: Option<&ObjectId>,
    is: Option<&TypedValue>,
    availability: Option<AvailabilityWanted>,
    snapshot: &Snapshot,
) -> Outcome<bool> {
    let read = Read::of_field(entity, field, &snapshot.states);
    let reads = vec![read.clone()];
    let Some(state) = snapshot.states.get(entity) else {
        return Outcome {
            result: Err(format!("{entity} isn't in the home")),
            reads,
        };
    };
    if let Some(wanted) = availability {
        let is_available = state.availability == Availability::Available;
        let wanted_available = wanted == AvailabilityWanted::Available;
        if is_available != wanted_available {
            return Outcome {
                result: Ok(false),
                reads,
            };
        }
        if is.is_none() {
            return Outcome {
                result: Ok(true),
                reads,
            };
        }
    }
    let Some(is) = is else {
        return Outcome {
            result: Ok(true),
            reads,
        };
    };
    if state.availability == Availability::Unavailable {
        return Outcome {
            result: Err(format!("{entity} is unavailable")),
            reads,
        };
    }
    Outcome {
        result: Ok(matches(is, &read.value)),
        reads,
    }
}

/// Whether a value from a rule (`to: true`, `is: 21`) matches a state's plain value.
pub fn matches(wanted: &TypedValue, value: &serde_json::Value) -> bool {
    match (wanted, value) {
        (TypedValue::Null, serde_json::Value::Null) => true,
        (TypedValue::Bool(a), serde_json::Value::Bool(b)) => a == b,
        (TypedValue::Number(a), serde_json::Value::Number(b)) => {
            b.as_f64().is_some_and(|b| (a - b).abs() < f64::EPSILON)
        }
        (TypedValue::Text(a), serde_json::Value::String(b)) => a == b,
        _ => false,
    }
}

fn to_json(value: Value, source: &str) -> Result<serde_json::Value, String> {
    Ok(match value {
        Value::Bool(b) => serde_json::Value::Bool(b),
        Value::Int(i) => serde_json::json!(i),
        Value::UInt(u) => serde_json::json!(u),
        // Every number is a decimal inside (`expr::compile`); a whole one reads as `45`.
        Value::Float(f) if f.fract() == 0.0 && f.abs() < 9e15 => serde_json::json!(f as i64),
        Value::Float(f) => serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .ok_or_else(|| format!("`{source}` isn't a finite number"))?,
        Value::String(s) => serde_json::Value::String(s.as_ref().clone()),
        Value::Null => serde_json::Value::Null,
        other => return Err(format!("`{source}` gave {other:?}, not a plain value")),
    })
}

fn from_json(value: &serde_json::Value) -> Value {
    match value {
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => Value::Float(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => Value::String(Arc::new(s.clone())),
        _ => Value::Null,
    }
}

type Reads = Arc<Mutex<Vec<Read>>>;

/// A bound entity function, as CEL calls it.
type EntityFn = Box<dyn Fn(&FunctionContext, Arc<String>) -> ResolveResult + Send + Sync>;
type ValuedFn = Box<dyn Fn(&FunctionContext, Arguments) -> ResolveResult + Send + Sync>;

/// Looks an entity up, noting the read.
fn look(
    states: &BTreeMap<EntityId, EntityState>,
    reads: &Reads,
    id: &str,
) -> Result<Option<EntityState>, String> {
    let entity: EntityId = id
        .parse()
        .map_err(|e: irori_types::IdError| e.to_string())?;
    let read = Read::of(&entity, states);
    let mut all = reads.lock().unwrap_or_else(PoisonError::into_inner);
    if !all.iter().any(|seen| seen.entity_id == entity) {
        all.push(read);
    }
    Ok(states.get(&entity).cloned())
}

/// The entity's state, only if it's available and has a value.
fn live(
    function: &str,
    states: &BTreeMap<EntityId, EntityState>,
    reads: &Reads,
    id: &str,
) -> Result<State, String> {
    let Some(state) = look(states, reads, id)? else {
        return Err(format!("{function}({id:?}): no such entity"));
    };
    if state.availability == Availability::Unavailable {
        return Err(format!("{function}({id:?}): entity is unavailable"));
    }
    state
        .state
        .ok_or_else(|| format!("{function}({id:?}): entity has never reported a value"))
}

/// One of an entity's values beside its main one, read as `function` reads: `num` a number,
/// `on` an on/off, `text` a text.
fn read_field(
    function: &str,
    shape: ValueShape,
    state: &State,
    id: &str,
    field: &str,
) -> Result<Value, String> {
    match state.field(field) {
        None => Err(format!(
            "{function}({id:?}, {field:?}): the entity hasn't said its {field}"
        )),
        Some(value) if value.shape() != shape => Err(format!(
            "{function}({id:?}, {field:?}): {field} is {}",
            match value.shape() {
                ValueShape::Number => "a number — use num()",
                ValueShape::Bool => "on/off — use on()",
                ValueShape::Text => "text — use text()",
            }
        )),
        Some(Typed::Number(n)) => Ok(Value::Float(n)),
        Some(Typed::Bool(on)) => Ok(Value::Bool(on)),
        Some(Typed::Text(text)) => Ok(Value::String(Arc::new(text))),
    }
}

fn fail(ftx: &FunctionContext, message: String) -> ResolveResult {
    ftx.error(message).into()
}

/// The arguments of a maths function, as numbers.
fn numbers(
    ftx: &FunctionContext,
    name: &str,
    args: &[Value],
) -> Result<Vec<f64>, cel::ExecutionError> {
    args.iter()
        .map(|arg| match arg {
            Value::Float(f) => Ok(*f),
            #[allow(clippy::cast_precision_loss)]
            Value::Int(i) => Ok(*i as f64),
            #[allow(clippy::cast_precision_loss)]
            Value::UInt(u) => Ok(*u as f64),
            other => Err(ftx.error(format!("{name}() takes numbers, not {other:?}"))),
        })
        .collect()
}

fn bind(context: &mut Context, snapshot: &Snapshot, reads: &Reads, place: Option<Arc<Place>>) {
    let entity_fn =
        |name: &'static str, read: fn(State, &str) -> Result<Value, String>| -> EntityFn {
            let states = Arc::clone(&snapshot.states);
            let reads = Arc::clone(reads);
            Box::new(move |ftx: &FunctionContext, id: Arc<String>| {
                match live(name, &states, &reads, &id).and_then(|state| read(state, &id)) {
                    Ok(value) => Ok(value),
                    Err(message) => fail(ftx, message),
                }
            })
        };

    // `num(id)` reads the entity's main value; `num(id, 'position')` one of its others.
    let valued_fn = |name: &'static str,
                     shape: ValueShape,
                     read: fn(State, &str) -> Result<Value, String>|
     -> ValuedFn {
        let states = Arc::clone(&snapshot.states);
        let reads = Arc::clone(reads);
        Box::new(move |ftx: &FunctionContext, Arguments(args): Arguments| {
            let read = match args.as_slice() {
                [Value::String(id)] => {
                    live(name, &states, &reads, id).and_then(|state| read(state, id))
                }
                [Value::String(id), Value::String(field)] => live(name, &states, &reads, id)
                    .and_then(|state| read_field(name, shape, &state, id, field)),
                _ => Err(format!(
                    "{name}() takes an entity id, and the name of one of its values if not the main one"
                )),
            };
            match read {
                Ok(value) => Ok(value),
                Err(message) => fail(ftx, message),
            }
        })
    };

    let num = valued_fn("num", ValueShape::Number, |state, id| {
        match state.primary() {
            Typed::Number(n) => Ok(Value::Float(n)),
            Typed::Text(_) => Err(format!("num({id:?}): entity is text, not a number")),
            Typed::Bool(_) => Err(format!(
                "num({id:?}): entity is on/off, not a number — use on({id:?})"
            )),
        }
    });
    context.add_function("num", move |ftx: &FunctionContext, args: Arguments| {
        num(ftx, args)
    });

    let on = valued_fn("on", ValueShape::Bool, |state, id| match state.primary() {
        Typed::Bool(on) => Ok(Value::Bool(on)),
        Typed::Number(_) => Err(format!(
            "on({id:?}): entity is a number, not on/off — use num({id:?})"
        )),
        Typed::Text(_) => Err(format!(
            "on({id:?}): entity is text, not on/off — use text({id:?})"
        )),
    });
    context.add_function("on", move |ftx: &FunctionContext, args: Arguments| {
        on(ftx, args)
    });

    let text = valued_fn("text", ValueShape::Text, |state, id| {
        match state.primary() {
            Typed::Text(text) => Ok(Value::String(Arc::new(text))),
            Typed::Number(_) => Err(format!("text({id:?}): entity is a number")),
            Typed::Bool(_) => Err(format!("text({id:?}): entity is not a text sensor")),
        }
    });
    context.add_function("text", move |ftx: &FunctionContext, args: Arguments| {
        text(ftx, args)
    });

    let brightness = entity_fn("brightness", |state, id| match state {
        State::Light(light) => light
            .brightness
            .map(|b| Value::Float(f64::from(b)))
            .ok_or_else(|| format!("brightness({id:?}): the light hasn't said its brightness")),
        _ => Err(format!("brightness({id:?}): entity is not a light")),
    });
    context.add_function(
        "brightness",
        move |ftx: &FunctionContext, id: Arc<String>| brightness(ftx, id),
    );

    {
        let states = Arc::clone(&snapshot.states);
        let reads = Arc::clone(reads);
        context.add_function(
            "available",
            move |ftx: &FunctionContext, id: Arc<String>| match look(&states, &reads, &id) {
                // Gone at run time is "not available", not an error.
                Ok(state) => {
                    Ok(Value::Bool(state.is_some_and(|state| {
                        state.availability == Availability::Available
                    })))
                }
                Err(message) => fail(ftx, message),
            },
        );
    }
    {
        let states = Arc::clone(&snapshot.states);
        let reads = Arc::clone(reads);
        context.add_function(
            "unknown",
            move |ftx: &FunctionContext, id: Arc<String>| match look(&states, &reads, &id) {
                Ok(state) => Ok(Value::Bool(state.is_none_or(|state| state.state.is_none()))),
                Err(message) => fail(ftx, message),
            },
        );
    }
    {
        let states = Arc::clone(&snapshot.states);
        let reads = Arc::clone(reads);
        context.add_function(
            "attr",
            move |ftx: &FunctionContext, id: Arc<String>, key: Arc<String>| {
                let state = match look(&states, &reads, &id) {
                    Ok(Some(state)) => state,
                    Ok(None) => return fail(ftx, format!("attr({id:?}): no such entity")),
                    Err(message) => return fail(ftx, message),
                };
                if state.availability == Availability::Unavailable {
                    return fail(ftx, format!("attr({id:?}): entity is unavailable"));
                }
                match state
                    .attributes
                    .iter()
                    .find(|(name, _)| name.as_str() == key.as_str())
                {
                    Some((_, value)) => Ok(from_json(value)),
                    None => fail(ftx, format!("attr({id:?}, {key:?}): no such attribute")),
                }
            },
        );
    }
    {
        let vars = snapshot.vars.clone();
        context.add_function(
            crate::expr::VAR_FN,
            move |ftx: &FunctionContext, name: Arc<String>| match vars.get(name.as_str()) {
                Some(value) => Ok(from_json(value)),
                None => fail(ftx, format!("var({name:?}) hasn't been set on this path")),
            },
        );
    }
    {
        let now = snapshot.now;
        context.add_function("now_ts", move |_ftx: &FunctionContext| {
            #[allow(clippy::cast_precision_loss)] // seconds since 1970 fit a float exactly
            Ok(Value::Float(now.as_jiff().as_second() as f64))
        });
    }
    context.add_function("min", |ftx: &FunctionContext, a: Value, b: Value| {
        numbers(ftx, "min", &[a, b]).map(|n| Value::Float(n[0].min(n[1])))
    });
    context.add_function("max", |ftx: &FunctionContext, a: Value, b: Value| {
        numbers(ftx, "max", &[a, b]).map(|n| Value::Float(n[0].max(n[1])))
    });
    context.add_function("round", |ftx: &FunctionContext, a: Value| {
        numbers(ftx, "round", &[a]).map(|n| Value::Float(n[0].round()))
    });
    context.add_function(
        "clamp",
        |ftx: &FunctionContext, a: Value, low: Value, high: Value| {
            let n = numbers(ftx, "clamp", &[a, low, high])?;
            if n[1] > n[2] {
                return fail(
                    ftx,
                    format!(
                        "clamp(): the low end {} is above the high end {}",
                        n[1], n[2]
                    ),
                );
            }
            Ok(Value::Float(n[0].clamp(n[1], n[2])))
        },
    );
    for name in ["hour", "minute"] {
        let (place, now) = (place.clone(), snapshot.now);
        context.add_function(name, move |ftx: &FunctionContext| match &place {
            Some(place) => {
                let (hour, minute) = clock::hour_and_minute(place, now);
                Ok(Value::Int(if name == "hour" { hour } else { minute }))
            }
            None => fail(ftx, format!("{name}() needs the home's time zone")),
        });
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use irori_types::{
        BinarySensorState, Context as Cause, ContextId, Origin, SensorState, SensorValue,
    };

    fn at() -> Timestamp {
        "2026-09-29T20:00:00Z".parse().unwrap()
    }

    fn state(id: &str, value: State, availability: Availability) -> (EntityId, EntityState) {
        let id: EntityId = id.parse().unwrap();
        (
            id.clone(),
            EntityState {
                entity_id: id,
                availability,
                state: Some(value),
                attributes: Default::default(),
                last_changed: at(),
                last_updated: at(),
                last_reported: at(),
                context: Cause {
                    id: ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9").unwrap(),
                    parent_id: None,
                    origin: Origin::System,
                },
            },
        )
    }

    fn home(lux: f64, lux_available: bool) -> Snapshot {
        let availability = if lux_available {
            Availability::Available
        } else {
            Availability::Unavailable
        };
        Snapshot {
            states: Arc::new(BTreeMap::from([
                state(
                    "sensor.lux",
                    State::Sensor(SensorState {
                        value: SensorValue::Number(lux),
                    }),
                    availability,
                ),
                state(
                    "binary_sensor.motion",
                    State::BinarySensor(BinarySensorState { on: true }),
                    Availability::Available,
                ),
            ])),
            vars: BTreeMap::from([("level".to_owned(), serde_json::json!(40))]),
            now: at(),
        }
    }

    #[test]
    fn an_expression_reports_what_it_read() {
        let mut eval = Evaluator::default();
        let outcome = eval.boolean(
            "on('binary_sensor.motion') && num('sensor.lux') < 30",
            &home(42.0, true),
        );
        assert_eq!(outcome.result, Ok(false));
        assert_eq!(outcome.reads.len(), 2);
        assert_eq!(outcome.reads[1].entity_id.as_str(), "sensor.lux");
        assert_eq!(outcome.reads[1].value, serde_json::json!(42.0));
    }

    fn blind(position: Option<u8>) -> Snapshot {
        Snapshot {
            states: Arc::new(BTreeMap::from([state(
                "cover.blind",
                State::Cover(irori_types::CoverState {
                    state: irori_types::OpenState::Open,
                    position,
                    tilt: None,
                }),
                Availability::Available,
            )])),
            vars: BTreeMap::new(),
            now: at(),
        }
    }

    #[test]
    fn a_field_is_read_by_name() {
        let mut eval = Evaluator::default();
        let outcome = eval.boolean("num('cover.blind', 'position') > 50", &blind(Some(70)));
        assert_eq!(outcome.result, Ok(true));
        assert_eq!(
            eval.boolean("text('cover.blind') == 'open'", &blind(Some(70)))
                .result,
            Ok(true)
        );

        // A blind that hasn't said where it is has nothing to compare, which is an error and
        // not zero.
        let outcome = eval.boolean("num('cover.blind', 'position') > 50", &blind(None));
        assert!(
            outcome
                .result
                .unwrap_err()
                .contains("hasn't said its position")
        );

        let condition: Condition = serde_json::from_value(serde_json::json!({
            "type": "state", "entity": "cover.blind", "field": "position", "is": 70
        }))
        .unwrap();
        let outcome = eval.condition(&condition, &blind(Some(70)));
        assert_eq!(outcome.result, Ok(true));
        assert_eq!(
            outcome.reads[0].field.as_ref().unwrap().as_str(),
            "position"
        );
        assert_eq!(outcome.reads[0].value, serde_json::json!(70.0));
    }

    #[test]
    fn an_unavailable_sensor_is_an_error_not_a_value() {
        let mut eval = Evaluator::default();
        let outcome = eval.boolean("num('sensor.lux') < 30", &home(8.0, false));
        assert!(outcome.result.unwrap_err().contains("unavailable"));
        let condition: Condition = serde_json::from_value(serde_json::json!({
            "type": "state", "entity": "sensor.lux", "is": 8
        }))
        .unwrap();
        assert!(
            eval.condition(&condition, &home(8.0, false))
                .result
                .is_err()
        );
        assert_eq!(
            eval.condition(&condition, &home(8.0, true)).result,
            Ok(true)
        );
    }

    #[test]
    fn a_function_of_the_wrong_kind_says_which_one_fits() {
        let mut snapshot = home(8.0, true);
        let (id, washer) = state(
            "sensor.washer_program",
            State::Sensor(SensorState {
                value: SensorValue::Text("rinse".into()),
            }),
            Availability::Available,
        );
        Arc::make_mut(&mut snapshot.states).insert(id, washer);
        let mut eval = Evaluator::default();
        let mut error = |expr| eval.boolean(expr, &snapshot).result.unwrap_err();
        assert!(
            error("on('sensor.washer_program')").contains(
                r#"on("sensor.washer_program"): entity is text, not on/off — use text("sensor.washer_program")"#
            )
        );
        assert!(error("num('binary_sensor.motion') < 1").contains(
            r#"num("binary_sensor.motion"): entity is on/off, not a number — use on("binary_sensor.motion")"#
        ));
    }

    #[test]
    fn variables_and_combinators() {
        let mut eval = Evaluator::default();
        assert_eq!(
            eval.value("var('level') + 2", &home(8.0, true)).result,
            Ok(serde_json::json!(42))
        );
        // Only the function is renamed, never text inside a string.
        assert_eq!(
            eval.value("var('level') > 1 ? 'var(x)' : 'no'", &home(8.0, true))
                .result,
            Ok(serde_json::json!("var(x)"))
        );
        assert_eq!(
            eval.value("var ('level')", &home(8.0, true)).result,
            Ok(serde_json::json!(40))
        );
        let any: Condition = serde_json::from_value(serde_json::json!({
            "type": "any", "conditions": [
                { "type": "expr", "expr": "num('sensor.lux') < 30" },
                { "type": "state", "entity": "binary_sensor.motion", "is": true }
            ]
        }))
        .unwrap();
        // The lux reading fails, but `any` still holds through the motion sensor.
        let outcome = eval.condition(&any, &home(8.0, false));
        assert_eq!(outcome.result, Ok(true));
        assert_eq!(outcome.reads.len(), 2);
    }

    #[test]
    fn numbers_are_just_numbers() {
        let mut eval = Evaluator::default();
        let at = home(300.0, true);
        let mut value = |source: &str| eval.value(source, &at).result;
        // Whole and decimal numbers mix, and dividing doesn't round down.
        assert_eq!(value("70 - 12.5"), Ok(serde_json::json!(57.5)));
        assert_eq!(value("10 / 4"), Ok(serde_json::json!(2.5)));
        assert_eq!(
            value("70 - num('sensor.lux') / 600 * 25"),
            Ok(serde_json::json!(57.5))
        );
        // A number inside text, or after a decimal point, stays as it was.
        assert_eq!(value("'room 2'"), Ok(serde_json::json!("room 2")));
        assert_eq!(value("1.25 * 2"), Ok(serde_json::json!(2.5)));
    }

    #[test]
    fn maths_functions() {
        let mut eval = Evaluator::default();
        let at = home(300.0, true);
        let mut value = |source: &str| eval.value(source, &at).result;
        assert_eq!(value("min(3, 4.5)"), Ok(serde_json::json!(3)));
        assert_eq!(value("max(3, 4.5)"), Ok(serde_json::json!(4.5)));
        assert_eq!(value("round(57.5)"), Ok(serde_json::json!(58)));
        assert_eq!(value("clamp(80, 45, 70)"), Ok(serde_json::json!(70)));
        assert_eq!(
            value("round(clamp(70 - num('sensor.lux') / 600 * 25, 45, 70))"),
            Ok(serde_json::json!(58))
        );
        assert!(value("clamp(1, 5, 2)").unwrap_err().contains("low end"));
    }
}
