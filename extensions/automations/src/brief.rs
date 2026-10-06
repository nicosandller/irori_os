//! One flow, written out for the assistant: its logic, what the entities it touches report
//! right now, how its last run went, and what is wrong with it.
//!
//! This is everything a model is told about an automation, and for a model on the person's own
//! machine it is all there is: no tools, and a small context. So it is written to a budget,
//! and what gives way first is what a question about the automation needs least.

use std::collections::BTreeMap;

use irori_flow_types::api::{Armed, Problem, Severity};
use irori_flow_types::trace::{NearMiss, Outcome, RunRecord, Step};
use irori_flow_types::{Flow, Node, NodeId};
use irori_rules::MapRegistry;
use irori_types::{Availability, EntityId, EntityState};

/// The budget when the caller names none.
pub const BUDGET: usize = 6_000;

/// How many near-misses are told, newest first.
const NEAR_MISSES: usize = 5;
/// How many of a run's last steps are told.
const STEPS: usize = 25;
/// The longest a value, or a step's settings when room is short, is written.
const VALUE: usize = 160;

/// What the brief is written from.
#[derive(Debug)]
pub struct Picture<'a> {
    pub flow: &'a Flow,
    pub armed: &'a Armed,
    pub problems: &'a [Problem],
    pub registry: &'a MapRegistry,
    pub states: &'a [EntityState],
    /// Every entity the flow reads or acts on.
    pub involved: Vec<EntityId>,
    /// A run under way now.
    pub going: Option<&'a RunRecord>,
    /// The last run that wasn't a test.
    pub last: Option<&'a RunRecord>,
    /// Whether there are kept runs at all, tests included.
    pub ran_as_test: bool,
    /// Newest first.
    pub near_misses: &'a [NearMiss],
}

/// The flow in words, inside `budget` characters.
///
/// Problems, the logic and the current values always stay. What gives way, in order: the
/// near-misses; then a run's steps, down to how it ended and the check that decided it; then
/// each step's settings past their first [`VALUE`] characters.
pub fn write(picture: &Picture<'_>, budget: usize) -> String {
    let mut out = String::new();
    for tight in 0..=3 {
        out = at(picture, tight);
        if out.chars().count() <= budget {
            return out;
        }
    }
    cut(&out, budget)
}

fn at(picture: &Picture<'_>, tight: u8) -> String {
    let flow = picture.flow;
    let mut out = format!(
        "The automation \"{}\" (`{}`) is turned {}.\n",
        flow.name,
        flow.id,
        if flow.enabled { "on" } else { "off" }
    );
    if let Some(description) = &flow.description {
        out.push_str(&format!("Its description: {description}\n"));
    }
    out.push_str(&match picture.armed {
        Armed::Armed => "It is armed: its triggers are being watched.\n".to_owned(),
        Armed::Disabled => "It is not armed, because it is turned off: nothing will start it \
                            until it is turned on.\n"
            .to_owned(),
        Armed::Unarmed { reason } => {
            format!("It is NOT armed and cannot run until this is fixed: {reason}\n")
        }
    });
    out.push_str(&format!(
        "When a trigger fires while a run is going, its mode is {}.\n",
        json(&flow.mode)
    ));

    out.push_str("\nProblems:\n");
    if picture.problems.is_empty() {
        out.push_str("- None. Nothing in its definition stops it running.\n");
    }
    for problem in picture.problems {
        out.push_str(&format!(
            "- {}{}{}: {}\n",
            match problem.severity {
                Severity::Error => "Error (stops it running)",
                Severity::Warning => "Warning (it still runs)",
            },
            problem
                .node
                .as_ref()
                .map(|node| format!(", at step `{node}`"))
                .unwrap_or_default(),
            problem
                .field
                .as_ref()
                .map(|field| format!(", in `{field}`"))
                .unwrap_or_default(),
            problem.message
        ));
    }

    out.push_str(
        "\nLogic. Each step is written as its id, what it is, its settings, and where each way \
         out of it leads. A way out that isn't listed leads nowhere: the path ends there.\n",
    );
    let mut ways: BTreeMap<&NodeId, Vec<String>> = BTreeMap::new();
    for wire in &flow.wires {
        ways.entry(&wire.from.node)
            .or_default()
            .push(format!("{} → `{}`", wire.from.port, wire.to));
    }
    // Triggers first: they are where reading the logic starts.
    let ordered = flow
        .nodes
        .iter()
        .filter(|(_, node)| node.is_trigger())
        .chain(flow.nodes.iter().filter(|(_, node)| !node.is_trigger()));
    for (id, node) in ordered {
        let settings = settings(node);
        out.push_str(&format!(
            "- `{id}` — {} {}{}\n",
            kind(node),
            if tight >= 3 {
                cut(&settings, VALUE)
            } else {
                settings
            },
            match ways.get(id) {
                Some(ways) => format!(" — {}", ways.join("; ")),
                None => " — the path ends here".to_owned(),
            }
        ));
    }

    out.push_str("\nWhat each entity it reads or acts on reports right now:\n");
    if picture.involved.is_empty() {
        out.push_str("- It names no entities.\n");
    }
    for entity in &picture.involved {
        out.push_str(&format!(
            "- {} = {}\n",
            named(picture.registry, entity),
            match picture
                .states
                .iter()
                .find(|state| &state.entity_id == entity)
            {
                Some(state) => shown(state),
                None if picture.registry.entities.contains_key(entity) => "unknown".to_owned(),
                None => "no such entity in the home".to_owned(),
            }
        ));
    }

    out.push('\n');
    if let Some(run) = picture.going {
        out.push_str("A run is going right now. ");
        told(&mut out, picture, run, tight);
        out.push('\n');
    }
    match picture.last {
        Some(run) => {
            out.push_str("The last run that wasn't a test: ");
            told(&mut out, picture, run, tight);
        }
        None if picture.ran_as_test => {
            out.push_str("It has only ever been run as a test, never by one of its triggers.\n");
        }
        None if picture.going.is_none() => {
            out.push_str("It has never run.\n");
        }
        None => {}
    }

    if tight == 0 && !picture.near_misses.is_empty() {
        out.push_str(
            "\nNear-misses, newest first: times a trigger was close to starting a run and \
             didn't.\n",
        );
        for miss in picture.near_misses.iter().take(NEAR_MISSES) {
            out.push_str(&format!(
                "- {} at step `{}`: {}{}\n",
                miss.at,
                miss.node,
                miss.message,
                reads(picture, &miss.reads)
            ));
        }
    }
    out
}

/// One run: how it started and ended, and its steps.
fn told(out: &mut String, picture: &Picture<'_>, run: &RunRecord, tight: u8) {
    out.push_str(&format!(
        "started {} by trigger `{}` and {}.\n",
        run.started_at,
        run.trigger,
        match &run.outcome {
            None => "has not finished".to_owned(),
            Some(Outcome::Completed) => "completed: every path it took ran to its end".to_owned(),
            Some(Outcome::Error { node, message }) => {
                format!("FAILED at step `{node}`: {message}")
            }
            Some(Outcome::Stopped { node, reason }) => format!(
                "was ended by the stop step `{node}`{}",
                reason
                    .as_ref()
                    .map(|reason| format!(" ({})", json(reason)))
                    .unwrap_or_default()
            ),
            Some(Outcome::Superseded) => "was cut short by a newer run starting over".to_owned(),
            Some(Outcome::Aborted { reason }) => {
                format!("was aborted from outside ({})", json(reason))
            }
        }
    ));
    if !run.ended_at.is_empty() {
        out.push_str(&format!(
            "Its paths ended at: {}.\n",
            run.ended_at
                .iter()
                .map(|ending| match &ending.port {
                    Some(port) =>
                        format!("`{}` (left by `{port}`, which leads nowhere)", ending.node),
                    None => format!("`{}`", ending.node),
                })
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let steps: Vec<&Step> = if tight >= 2 {
        // Only what decided it: the last check, and any call that failed.
        let decided = run.steps.iter().rev().find(|step| {
            matches!(
                picture.flow.nodes.get(&step.node),
                Some(Node::Gate { .. } | Node::Switch { .. } | Node::Wait { .. })
            )
        });
        run.steps
            .iter()
            .filter(|step| {
                decided.is_some_and(|decided| decided.seq == step.seq)
                    || step
                        .call
                        .as_ref()
                        .is_some_and(|call| matches!(call.result, Some(Err(_))))
            })
            .collect()
    } else {
        let from = run.steps.len().saturating_sub(STEPS);
        run.steps.iter().skip(from).collect()
    };
    if steps.is_empty() {
        return;
    }
    out.push_str(if tight >= 2 {
        "The steps that decided it:\n"
    } else if run.steps.len() > steps.len() {
        "Its last steps, in order:\n"
    } else {
        "Its steps, in order:\n"
    });
    for step in steps {
        out.push_str(&format!("- `{}`", step.node));
        if let Some(node) = picture.flow.nodes.get(&step.node) {
            out.push_str(&format!(" ({})", kind(node)));
        }
        match (&step.port, &step.finished_at) {
            (Some(port), _) => out.push_str(&format!(" left by `{port}`")),
            (None, None) => out.push_str(" is still going"),
            (None, Some(_)) => out.push_str(" ended the path"),
        }
        if let Some(note) = &step.note {
            out.push_str(&format!(": {}", cut(note, 2 * VALUE)));
        }
        out.push_str(&reads(picture, &step.reads));
        if let Some(call) = &step.call {
            out.push_str(&format!(
                "; called {} on {}{} — {}",
                call.service,
                named(picture.registry, &call.entity_id),
                call.data
                    .as_ref()
                    .map(|data| format!(" with {}", cut(&data.to_string(), VALUE)))
                    .unwrap_or_default(),
                match &call.result {
                    None => "no answer yet".to_owned(),
                    Some(Ok(())) if call.simulated => "recorded, not sent".to_owned(),
                    Some(Ok(())) => "it worked".to_owned(),
                    Some(Err(error)) => format!("it FAILED: {error}"),
                }
            ));
        }
        out.push('\n');
    }
}

fn reads(picture: &Picture<'_>, reads: &[irori_flow_types::trace::Read]) -> String {
    if reads.is_empty() {
        return String::new();
    }
    let seen: Vec<String> = reads
        .iter()
        .map(|read| {
            let value = match read.availability {
                None => "no such entity".to_owned(),
                Some(Availability::Unavailable) => "unavailable".to_owned(),
                Some(_) => cut(&read.value.to_string(), VALUE),
            };
            format!("{} was {value}", named(picture.registry, &read.entity_id))
        })
        .collect();
    format!("; it read: {}", seen.join(", "))
}

/// An entity the way a person and the logic both know it: `Name (entity.id)`.
fn named(registry: &MapRegistry, entity: &EntityId) -> String {
    match registry.entities.get(entity) {
        Some(known) => format!("{} (`{entity}`)", known.name),
        None => format!("`{entity}`"),
    }
}

fn shown(state: &EntityState) -> String {
    if state.availability == Availability::Unavailable {
        return "unavailable".to_owned();
    }
    match &state.state {
        None => "unknown".to_owned(),
        Some(value) => cut(&json(value), VALUE),
    }
}

fn kind(node: &Node) -> &'static str {
    match node {
        Node::Trigger { .. } => "trigger",
        Node::Gate { .. } => "check",
        Node::Switch { .. } => "choose",
        Node::Call { .. } => "call",
        Node::Set { .. } => "set",
        Node::Delay { .. } => "wait",
        Node::Wait { .. } => "wait until",
        Node::Join { .. } => "join",
        Node::Stop { .. } => "stop",
    }
}

/// A step's settings as the flow file writes them, without the `type` its kind already says.
fn settings(node: &Node) -> String {
    let mut value = serde_json::to_value(node).unwrap_or_default();
    if let Some(fields) = value.as_object_mut() {
        fields.remove("type");
        if fields.is_empty() {
            return String::new();
        }
    }
    value.to_string()
}

fn json(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "unknown".to_owned())
}

/// `text`, or its first `most` characters and an ellipsis.
fn cut(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(most.saturating_sub(1)).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow() -> Flow {
        serde_json::from_str(include_str!(
            "../../../fixtures/types/flow/valid/hallway.json"
        ))
        .expect("the fixture is a flow")
    }

    /// A run where it was dark enough, but the light refused.
    fn failed() -> RunRecord {
        serde_json::from_value(serde_json::json!({
            "run_id": "01K5B2Q9A1B2C3D4E5F6G7H8J9",
            "flow_id": "hallway_motion_light",
            "version": "v",
            "trigger": "motion",
            "started_at": "2026-09-29T20:00:00Z",
            "finished_at": "2026-09-29T20:00:01Z",
            "outcome": "error",
            "node": "on",
            "message": "the light didn't answer",
            "ended_at": [{ "node": "on", "port": "error" }],
            "steps": [
                { "seq": 0, "node": "motion", "token": 0, "at": "2026-09-29T20:00:00Z",
                  "finished_at": "2026-09-29T20:00:00Z", "port": "out" },
                { "seq": 1, "node": "dark", "token": 0, "at": "2026-09-29T20:00:00Z",
                  "finished_at": "2026-09-29T20:00:00Z", "port": "yes",
                  "note": "num('sensor.demo_luminosity_illuminance') < 30 → true",
                  "reads": [{ "entity_id": "sensor.demo_luminosity_illuminance",
                              "availability": "available", "value": 8.0 }] },
                { "seq": 2, "node": "on", "token": 0, "at": "2026-09-29T20:00:00Z",
                  "finished_at": "2026-09-29T20:00:01Z", "port": "error",
                  "call": { "service": "light.turn_on", "entity_id": "light.demo_hall_light",
                            "data": { "brightness_pct": 60 },
                            "result": { "Err": "the light didn't answer" } } }
            ]
        }))
        .expect("a run record")
    }

    fn missed() -> NearMiss {
        serde_json::from_value(serde_json::json!({
            "at": "2026-09-29T21:00:00Z",
            "flow_id": "hallway_motion_light",
            "version": "v",
            "node": "motion",
            "kind": "dropped",
            "message": "motion went on, but a run was already going",
        }))
        .expect("a near-miss")
    }

    fn brief(run: Option<&RunRecord>, ran_as_test: bool, budget: usize) -> String {
        let flow = flow();
        let misses = [missed()];
        write(
            &Picture {
                flow: &flow,
                armed: &Armed::Armed,
                problems: &[],
                registry: &MapRegistry {
                    entities: BTreeMap::new(),
                    timezone: false,
                    location: false,
                },
                states: &[],
                involved: vec!["light.demo_hall_light".parse().expect("an entity id")],
                going: None,
                last: run,
                ran_as_test,
                near_misses: &misses,
            },
            budget,
        )
    }

    #[test]
    fn a_run_that_failed_says_where_and_what_it_had_read() {
        let run = failed();
        let text = brief(Some(&run), true, 20_000);
        for said in [
            "FAILED at step `on`: the light didn't answer",
            "`dark` (check) left by `yes`: num('sensor.demo_luminosity_illuminance') < 30 → true",
            "it read: `sensor.demo_luminosity_illuminance` was 8.0",
            "called light.turn_on on `light.demo_hall_light` with {\"brightness_pct\":60} — it \
             FAILED: the light didn't answer",
            "`on` (left by `error`, which leads nowhere)",
            "Near-misses, newest first",
            "motion went on, but a run was already going",
            "`off` — call {\"entity\":\"light.demo_hall_light\",\"service\":\"light.turn_off\"} — \
             the path ends here",
        ] {
            assert!(text.contains(said), "no {said:?} in:\n{text}");
        }
    }

    #[test]
    fn a_tight_budget_keeps_what_decided_the_run() {
        let run = failed();
        let full = brief(Some(&run), true, 20_000);
        let no_misses = brief(Some(&run), true, full.chars().count() - 1);
        assert!(!no_misses.contains("Near-misses"), "{no_misses}");
        assert!(no_misses.contains("`motion` (trigger) left by `out`"));
        let decided = brief(Some(&run), true, no_misses.chars().count() - 1);
        assert!(decided.contains("The steps that decided it:"), "{decided}");
        assert!(!decided.contains("`motion` (trigger) left by"), "{decided}");
        assert!(
            decided.contains("`dark` (check) left by `yes`"),
            "{decided}"
        );
        assert!(decided.contains("it FAILED"), "{decided}");
        assert!(decided.contains("What each entity it reads or acts on reports right now"));
    }

    #[test]
    fn a_flow_only_ever_tested_is_not_said_to_have_run() {
        assert!(brief(None, true, 20_000).contains("only ever been run as a test"));
        assert!(brief(None, false, 20_000).contains("It has never run."));
    }
}
