//! Trying a flow without trusting it (`docs/specs/flows.md` §7): the same engine, on a virtual
//! clock, with its calls recorded instead of made.

use std::collections::BTreeMap;

use irori_flow_types::trace::{Outcome, RunRecord, TestKind};
use irori_flow_types::{Flow, NodeId, Port};
use irori_types::{Availability, EntityId, EntityState, State, Timestamp, Typed};

use irori_rules::clock::Place;

use crate::engine::{Arm, CountingIds, Effect, Engine};

/// How many timers a simulation follows before it decides the flow won't settle.
const MAX_TICKS: usize = 100_000;

/// A dry run: `trigger` fires now against `states` with `overrides` applied, time
/// fast-forwards, and nothing in the home changes meanwhile. The record says what it had to
/// assume.
pub fn dry_run(
    flow: &Flow,
    trigger: &NodeId,
    states: Vec<EntityState>,
    overrides: &BTreeMap<EntityId, serde_json::Value>,
    place: Option<Place>,
    now: Timestamp,
) -> Result<RunRecord, String> {
    let mut flow = flow.clone();
    flow.enabled = true;
    let mut engine = Engine::dry(Box::new(CountingIds::default()));
    engine.set_place(place, now);
    let mut states: BTreeMap<EntityId, EntityState> = states
        .into_iter()
        .map(|state| (state.entity_id.clone(), state))
        .collect();
    for (entity, value) in overrides {
        let state = states
            .get_mut(entity)
            .ok_or_else(|| format!("{entity} isn't in the home"))?;
        pretend(state, value)?;
    }
    engine.load_states(states.into_values());
    let id = flow.id.clone();
    engine.set_flows(
        vec![Arm {
            flow,
            problems: Vec::new(),
        }],
        now,
    );
    engine.fire(&id, trigger, Some(TestKind::Dry), now)?;
    let mut records = settle(&mut engine, None);
    let mut record = records
        .pop()
        .ok_or_else(|| "the run didn't finish".to_owned())?;
    record.assumptions = assumptions(&record);
    Ok(record)
}

/// Replays `history` — each entity's states, oldest first — through `flow`, and answers the
/// runs it would have made between `from` and `to`, and how many changes it replayed.
///
/// The clock is replayed too: a time or sun trigger fires where it would have, as far back as
/// the history goes, or over the day before `to` when the flow watches nothing.
///
/// Each entity starts from its first state in the history; the rest are replayed in the order
/// of their own timestamps. An entity with no history didn't change in the window, so its
/// state in `baseline` (the home now) held throughout. Calls don't feed back into state: the
/// history already holds what really happened.
pub fn backtest(
    flow: &Flow,
    history: &BTreeMap<EntityId, Vec<EntityState>>,
    baseline: &[EntityState],
    place: Option<Place>,
    to: Timestamp,
) -> (Vec<RunRecord>, usize) {
    let mut flow = flow.clone();
    flow.enabled = true;
    let mut engine = Engine::dry(Box::new(CountingIds::default()));
    let mut initial: Vec<EntityState> = baseline
        .iter()
        .filter(|state| history.get(&state.entity_id).is_none_or(Vec::is_empty))
        .cloned()
        .collect();
    let mut changes: Vec<(Timestamp, EntityState)> = Vec::new();
    for states in history.values() {
        let mut states = states.iter();
        if let Some(first) = states.next() {
            initial.push(first.clone());
        }
        changes.extend(states.map(|state| (state.last_updated, state.clone())));
    }
    // Stable, so two changes at the same instant keep their order.
    changes.sort_by_key(|(at, _)| *at);
    let start = history
        .values()
        .filter_map(|states| states.first())
        .map(|state| state.last_updated)
        .min()
        .unwrap_or_else(|| {
            Timestamp::from_jiff(
                to.as_jiff()
                    .checked_sub(jiff::SignedDuration::from_hours(24))
                    .unwrap_or(to.as_jiff()),
            )
        });
    engine.load_states(initial);
    engine.set_place(place, start);
    engine.set_flows(
        vec![Arm {
            flow,
            problems: Vec::new(),
        }],
        start,
    );
    let replayed = changes.len();
    let mut records = Vec::new();
    for (at, state) in changes {
        records.extend(settle(&mut engine, Some(at)));
        let old = engine.state(&state.entity_id).cloned();
        engine.state_changed(old, state, at);
        records.extend(finished(&mut engine));
    }
    records.extend(settle(&mut engine, Some(to)));
    // A run still going at the end of the window is shown as far as it got.
    for run in engine.active(None) {
        records.push(run.record);
    }
    (records, replayed)
}

/// Follows the engine's timers — up to `until` if given, else until nothing is left — and
/// answers the runs that finished.
fn settle(engine: &mut Engine, until: Option<Timestamp>) -> Vec<RunRecord> {
    let mut records = finished(engine);
    let mut ticks = 0;
    while let Some(next) = engine.next_deadline() {
        if until.is_some_and(|until| next > until) || ticks >= MAX_TICKS {
            break;
        }
        // With no end given, this follows one run to its end. A time trigger's next moment is
        // always on the clock, and would otherwise be followed for ever.
        if until.is_none() && engine.idle() {
            break;
        }
        // One deadline at a time, each at its own moment: a replay wants every time of day
        // the window holds, where a live engine that woke late fires only once.
        engine.advance(next);
        records.extend(finished(engine));
        ticks += 1;
    }
    records
}

fn finished(engine: &mut Engine) -> Vec<RunRecord> {
    engine
        .take_effects()
        .into_iter()
        .filter_map(|effect| match effect {
            Effect::Finished(record) => Some(*record),
            _ => None,
        })
        .collect()
}

/// What a dry run couldn't know and so assumed.
fn assumptions(record: &RunRecord) -> Vec<String> {
    let mut said = Vec::new();
    for step in &record.steps {
        if step.port == Some(Port::Timeout) {
            said.push(format!(
                "{}: nothing changes during a dry run, so this waited its full time. In real \
                 life it may have gone on sooner.",
                step.node
            ));
        }
        if let Some(call) = &step.call
            && call.simulated
        {
            said.push(format!(
                "{}: {} {} was recorded, not sent, and assumed to work.",
                step.node, call.service, call.entity_id
            ));
        }
    }
    if matches!(record.outcome, Some(Outcome::Error { .. })) {
        said.push("It ended in an error; see the step it stopped at.".into());
    }
    said.dedup();
    said
}

/// Makes `state` say `value`, the way its kind holds it.
fn pretend(state: &mut EntityState, value: &serde_json::Value) -> Result<(), String> {
    let entity = state.entity_id.clone();
    let wrong = || format!("{value} isn't a value {entity} can have");
    state.availability = Availability::Available;
    let typed = match value {
        serde_json::Value::Null => {
            state.state = None;
            return Ok(());
        }
        serde_json::Value::Bool(on) => Typed::Bool(*on),
        serde_json::Value::Number(n) => Typed::Number(n.as_f64().ok_or_else(wrong)?),
        serde_json::Value::String(text) => Typed::Text(text.clone()),
        _ => return Err(wrong()),
    };
    state.state =
        Some(State::with_primary(entity.kind(), state.state.as_ref(), &typed).ok_or_else(wrong)?);
    Ok(())
}
