//! The flow engine against a scripted home, on a virtual clock.

use std::collections::BTreeMap;

use irori_flow_types::Flow;
use irori_flow_types::api::Severity;
use irori_flow_types::trace::{AbortReason, NearMissKind, Outcome, RunRecord};
use irori_flows::{Arm, CountingIds, Effect, Engine, sim, validate};
use irori_rules::MapRegistry;
use irori_types::{
    Availability, BinarySensorCapabilities, BinarySensorState, Capabilities, Context, ContextId,
    Entity, EntityId, EntityState, LightCapabilities, LightState, MediaPlayerCapabilities,
    MediaPlayerClass, MediaPlayerState, Name, Origin, Playback, SensorCapabilities, SensorState,
    SensorValue, SensorValueType, State, SwitchCapabilities, SwitchState, Timestamp, UniqueId,
};

const TV: &str = "media_player.demo_tv";
const MOTION: &str = "binary_sensor.demo_movement_motion";
const LUX: &str = "sensor.demo_luminosity_illuminance";
const OCCUPANCY: &str = "binary_sensor.demo_mmwave_occupancy";
const LIGHT: &str = "light.demo_hall_light";
const GUESTS: &str = "switch.guests_over";

fn id(s: &str) -> EntityId {
    s.parse().expect("an entity id")
}

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_jiff(jiff::Timestamp::from_second(1_790_000_000 + seconds).expect("a time"))
}

fn context() -> Context {
    Context {
        id: ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9").expect("valid"),
        parent_id: None,
        origin: Origin::System,
    }
}

fn state(entity: &str, value: State, seconds: i64) -> EntityState {
    EntityState {
        entity_id: id(entity),
        availability: Availability::Available,
        state: Some(value),
        attributes: BTreeMap::default(),
        last_changed: at(seconds),
        last_updated: at(seconds),
        last_reported: at(seconds),
        context: context(),
    }
}

fn flag(entity: &str, on: bool, seconds: i64) -> EntityState {
    let value = match id(entity).kind() {
        irori_types::EntityKind::Switch => State::Switch(SwitchState { on }),
        irori_types::EntityKind::Light => State::Light(LightState {
            on,
            brightness: None,
            color_mode: None,
            color_temp_kelvin: None,
            rgb: None,
        }),
        _ => State::BinarySensor(BinarySensorState { on }),
    };
    state(entity, value, seconds)
}

fn lux(value: f64, seconds: i64) -> EntityState {
    state(
        LUX,
        State::Sensor(SensorState {
            value: SensorValue::Number(value),
        }),
        seconds,
    )
}

fn entity(entity: &str, capabilities: Capabilities) -> Entity {
    Entity {
        id: id(entity),
        protocol: "demo".parse().expect("valid"),
        unique_id: UniqueId::try_from(entity.replace('.', "-")).expect("valid"),
        name: Name::try_from("x").expect("valid"),
        device_id: None,
        area_id: None,
        capabilities,
        entity_category: None,
    }
}

fn registry() -> MapRegistry {
    let binary = || Capabilities::BinarySensor(BinarySensorCapabilities { device_class: None });
    MapRegistry {
        entities: [
            entity(MOTION, binary()),
            entity(OCCUPANCY, binary()),
            entity(
                LUX,
                Capabilities::Sensor(SensorCapabilities {
                    value_type: SensorValueType::Number,
                    device_class: None,
                    unit: Some("lx".into()),
                    state_class: None,
                    options: Vec::new(),
                }),
            ),
            entity(
                LIGHT,
                Capabilities::Light(LightCapabilities {
                    brightness: true,
                    color_temp_kelvin: None,
                    rgb: false,
                }),
            ),
            entity(
                GUESTS,
                Capabilities::Switch(SwitchCapabilities { device_class: None }),
            ),
        ]
        .into_iter()
        .map(|e| (e.id.clone(), e))
        .collect(),
        timezone: false,
        location: false,
    }
}

fn hallway() -> Flow {
    serde_json::from_str(include_str!(
        "../../../fixtures/types/flow/valid/hallway.json"
    ))
    .expect("the hallway fixture")
}

fn flow(json: serde_json::Value) -> Flow {
    serde_json::from_value(json).expect("a flow")
}

/// An engine running `flow` in a dark, empty hallway.
fn engine_with(flow: Flow) -> Engine {
    let mut engine = Engine::new(Box::new(CountingIds::default()));
    engine.load_states([
        flag(MOTION, false, 0),
        flag(OCCUPANCY, false, 0),
        lux(8.0, 0),
        flag(LIGHT, false, 0),
        flag(GUESTS, false, 0),
    ]);
    let problems = validate::check(&flow, &registry());
    engine.set_flows(vec![Arm { flow, problems }], at(0));
    engine
}

fn change(engine: &mut Engine, new: EntityState) {
    let now = new.last_updated;
    engine.advance(now);
    let old = engine.state(&new.entity_id).cloned();
    engine.state_changed(old, new, now);
}

/// Answers every call the engine asked for with `ok`, and returns what it asked and what
/// finished.
fn effects(
    engine: &mut Engine,
    now: Timestamp,
) -> (
    Vec<(EntityId, String)>,
    Vec<RunRecord>,
    Vec<irori_flow_types::trace::NearMiss>,
) {
    let mut calls = Vec::new();
    let mut done = Vec::new();
    let mut misses = Vec::new();
    loop {
        let effects = engine.take_effects();
        if effects.is_empty() {
            break;
        }
        for effect in effects {
            match effect {
                Effect::Call {
                    call_id,
                    entity,
                    service,
                    ..
                } => {
                    calls.push((entity, service.to_string()));
                    engine.call_finished(call_id, Ok(()), now);
                }
                Effect::Finished(record) => done.push(*record),
                Effect::NearMiss(miss) => misses.push(miss),
            }
        }
    }
    (calls, done, misses)
}

#[test]
fn the_hallway_fixture_has_no_problems_at_all() {
    let problems = validate::check(&hallway(), &registry());
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn motion_in_the_dark_turns_the_light_on_until_the_hall_is_clear() {
    let mut engine = engine_with(hallway());
    change(&mut engine, flag(MOTION, true, 10));
    let (calls, done, _) = effects(&mut engine, at(10));
    assert_eq!(calls, [(id(LIGHT), "light.turn_on".to_owned())]);
    assert!(done.is_empty(), "still waiting for the hall to clear");

    // Waiting: the live view says where and until when.
    let active = engine.active(None);
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].at[0].node.as_str(), "clear");
    assert_eq!(active[0].at[0].doing, "waiting");
    // Occupancy is already clear, so it's holding: two minutes from when it started looking.
    assert!(active[0].at[0].holding_since.is_some());

    engine.advance(at(10 + 120));
    let (calls, done, _) = effects(&mut engine, at(130));
    assert_eq!(calls, [(id(LIGHT), "light.turn_off".to_owned())]);
    let run = &done[0];
    assert_eq!(run.outcome, Some(Outcome::Completed));
    let path: Vec<&str> = run.steps.iter().map(|s| s.node.as_str()).collect();
    assert_eq!(path, ["motion", "dark", "on", "clear", "off"]);
    assert_eq!(run.steps[1].reads[0].entity_id, id(LUX));
    assert_eq!(run.steps[1].reads[0].value, serde_json::json!(8.0));
    assert!(
        run.steps[0]
            .note
            .as_deref()
            .is_some_and(|n| n.contains("off → on"))
    );
    // What started it is on the record, and every call is made as this run.
    assert_eq!(run.cause, Some(context().id));

    // A record goes to the page and to disk and comes back the same.
    let written = serde_json::to_string(run).expect("serializes");
    let read: RunRecord = serde_json::from_str(&written).expect("reads back");
    assert_eq!(&read, run);
}

#[test]
fn too_bright_ends_at_the_gate_and_says_what_it_read() {
    let mut engine = engine_with(hallway());
    change(&mut engine, lux(42.0, 5));
    change(&mut engine, flag(MOTION, true, 10));
    let (calls, done, _) = effects(&mut engine, at(10));
    assert!(calls.is_empty());
    let run = &done[0];
    assert_eq!(run.outcome, Some(Outcome::Completed));
    assert_eq!(run.summary().summary, "ended at dark → no");
    assert_eq!(run.steps[1].reads[0].value, serde_json::json!(42.0));
}

#[test]
fn an_unavailable_sensor_fails_closed_and_is_a_near_miss_for_its_trigger() {
    let mut engine = engine_with(hallway());
    let mut offline = lux(8.0, 5);
    offline.availability = Availability::Unavailable;
    change(&mut engine, offline);
    change(&mut engine, flag(MOTION, true, 10));
    let (calls, done, _) = effects(&mut engine, at(10));
    assert!(calls.is_empty(), "an unavailable lux reading is not dark");
    assert!(
        done[0].steps[1]
            .note
            .as_deref()
            .is_some_and(|n| n.contains("unavailable")),
        "{:?}",
        done[0].steps[1].note
    );

    let mut gone = flag(MOTION, false, 20);
    gone.availability = Availability::Unavailable;
    change(&mut engine, gone);
    let (_, _, misses) = effects(&mut engine, at(20));
    assert_eq!(misses[0].kind, NearMissKind::Unavailable);
    assert!(misses[0].message.contains("became unavailable"));
}

#[test]
fn restart_starts_over_and_single_refuses_with_a_near_miss() {
    let mut engine = engine_with(hallway());
    change(&mut engine, flag(OCCUPANCY, true, 1));
    change(&mut engine, flag(MOTION, true, 10));
    change(&mut engine, flag(MOTION, false, 12));
    change(&mut engine, flag(MOTION, true, 14));
    let (calls, done, _) = effects(&mut engine, at(14));
    assert_eq!(calls.len(), 2, "turned on twice: once per run");
    assert_eq!(done[0].outcome, Some(Outcome::Superseded));
    assert_eq!(engine.active(None).len(), 1);

    let mut single = hallway();
    single.mode = irori_flow_types::Mode::default();
    let mut engine = engine_with(single);
    change(&mut engine, flag(OCCUPANCY, true, 1));
    change(&mut engine, flag(MOTION, true, 10));
    change(&mut engine, flag(MOTION, false, 12));
    change(&mut engine, flag(MOTION, true, 14));
    let (_, _, misses) = effects(&mut engine, at(14));
    assert_eq!(misses.len(), 1);
    assert_eq!(misses[0].kind, NearMissKind::Dropped);
    assert!(
        misses[0].message.contains("mode is single"),
        "{}",
        misses[0].message
    );
}

#[test]
fn a_hold_that_resets_is_a_near_miss_and_one_that_lasts_fires() {
    let held = flow(serde_json::json!({
        "id": "held", "name": "Held",
        "nodes": {
            "motion": { "type": "trigger", "trigger": { "type": "state", "entity": MOTION, "to": true, "for": "5s" } },
            "on": { "type": "call", "service": "light.turn_on", "entity": LIGHT }
        },
        "wires": [["motion", "on"]]
    }));
    let mut engine = engine_with(held);
    change(&mut engine, flag(MOTION, true, 10));
    change(&mut engine, flag(MOTION, false, 12));
    let (calls, _, misses) = effects(&mut engine, at(12));
    assert!(calls.is_empty());
    assert_eq!(misses[0].kind, NearMissKind::HoldReset);

    change(&mut engine, flag(MOTION, true, 20));
    engine.advance(at(25));
    let (calls, done, _) = effects(&mut engine, at(25));
    assert_eq!(calls.len(), 1);
    assert!(
        done[0].steps[0]
            .note
            .as_deref()
            .is_some_and(|n| n.contains("held on for 5s"))
    );
}

fn joined(mode: &str) -> Flow {
    flow(serde_json::json!({
        "id": "joined", "name": "Joined",
        "nodes": {
            "motion": { "type": "trigger", "trigger": { "type": "state", "entity": MOTION, "to": true } },
            "slow": { "type": "delay", "for": "3s" },
            "dark": { "type": "gate", "condition": { "type": "expr", "expr": format!("num('{LUX}') < 30") } },
            "both": if mode == "all" {
                serde_json::json!({ "type": "join", "mode": "all", "timeout": "1m" })
            } else {
                serde_json::json!({ "type": "join", "mode": "first" })
            },
            "on": { "type": "call", "service": "light.turn_on", "entity": LIGHT }
        },
        "wires": [["motion", "slow"], ["motion", "dark"], ["slow", "both"], ["dark:yes", "both"], ["both", "on"]]
    }))
}

#[test]
fn a_join_waits_for_every_path_or_lets_the_first_through() {
    let mut engine = engine_with(joined("all"));
    assert!(validate::check(&joined("all"), &registry()).is_empty());
    change(&mut engine, flag(MOTION, true, 10));
    let (calls, _, _) = effects(&mut engine, at(10));
    assert!(calls.is_empty(), "the slow path isn't here yet");
    engine.advance(at(13));
    let (calls, done, _) = effects(&mut engine, at(13));
    assert_eq!(calls.len(), 1, "once, not once per path");
    assert_eq!(done[0].outcome, Some(Outcome::Completed));

    let mut engine = engine_with(joined("first"));
    change(&mut engine, flag(MOTION, true, 10));
    let (calls, _, _) = effects(&mut engine, at(10));
    assert_eq!(calls.len(), 1, "the dark path got there first");
    engine.advance(at(13));
    let (calls, done, _) = effects(&mut engine, at(13));
    assert!(calls.is_empty(), "the slow path ends at the join");
    assert!(
        done[0]
            .steps
            .iter()
            .any(|s| s.note.as_deref() == Some("another path got here first"))
    );
}

#[test]
fn a_join_whose_other_path_ended_gives_up_at_once_saying_which() {
    let mut engine = engine_with(joined("all"));
    change(&mut engine, lux(400.0, 5));
    change(&mut engine, flag(MOTION, true, 10));
    engine.advance(at(13));
    let (calls, done, _) = effects(&mut engine, at(13));
    assert!(calls.is_empty());
    let join = done[0]
        .steps
        .iter()
        .rev()
        .find(|s| s.node.as_str() == "both")
        .expect("reached the join");
    assert_eq!(join.port, Some(irori_flow_types::Port::Timeout));
    assert!(
        join.note.as_deref().is_some_and(|n| n.contains("dark")),
        "{:?}",
        join.note
    );
}

#[test]
fn a_failed_call_ends_the_run_with_an_error_unless_its_error_port_is_wired() {
    let mut engine = engine_with(hallway());
    change(&mut engine, flag(MOTION, true, 10));
    let effects = engine.take_effects();
    let Some(Effect::Call { call_id, .. }) = effects.first() else {
        panic!("a call");
    };
    engine.call_finished(*call_id, Err("unavailable: no answer".into()), at(11));
    let done: Vec<RunRecord> = engine
        .take_effects()
        .into_iter()
        .filter_map(|e| match e {
            Effect::Finished(r) => Some(*r),
            _ => None,
        })
        .collect();
    assert!(matches!(&done[0].outcome, Some(Outcome::Error { node, .. }) if node.as_str() == "on"));
}

#[test]
fn editing_or_disabling_a_flow_aborts_its_runs() {
    let mut engine = engine_with(hallway());
    change(&mut engine, flag(OCCUPANCY, true, 1));
    change(&mut engine, flag(MOTION, true, 10));
    let _ = effects(&mut engine, at(10));
    let mut edited = hallway();
    edited.name = Name::try_from("Hall").expect("valid");
    engine.set_flows(
        vec![Arm {
            flow: edited,
            problems: Vec::new(),
        }],
        at(20),
    );
    let (_, done, _) = effects(&mut engine, at(20));
    assert_eq!(
        done[0].outcome,
        Some(Outcome::Aborted {
            reason: AbortReason::Changed
        })
    );
}

#[test]
fn a_dry_run_sends_nothing_and_says_what_it_assumed() {
    let states = vec![
        flag(MOTION, false, 0),
        flag(OCCUPANCY, true, 0),
        lux(400.0, 0),
        flag(LIGHT, false, 0),
    ];
    let overrides = BTreeMap::from([(id(LUX), serde_json::json!(20))]);
    let record = sim::dry_run(
        &hallway(),
        &"motion".parse().expect("valid"),
        states,
        &overrides,
        at(0),
    )
    .expect("runs");
    let path: Vec<&str> = record.steps.iter().map(|s| s.node.as_str()).collect();
    assert_eq!(
        path,
        ["motion", "dark", "on", "clear", "off"],
        "pretending it's dark"
    );
    assert!(record.steps[2].call.as_ref().is_some_and(|c| c.simulated));
    assert!(
        record
            .assumptions
            .iter()
            .any(|a| a.contains("waited its full time")),
        "{:?}",
        record.assumptions
    );
    // It took ten minutes of pretend time, and no time at all.
    assert_eq!(record.finished_at, Some(at(600)));
}

#[test]
fn a_backtest_replays_history_into_the_runs_it_would_have_made() {
    let history = BTreeMap::from([
        (
            id(MOTION),
            vec![
                flag(MOTION, false, 0),
                flag(MOTION, true, 100),
                flag(MOTION, false, 110),
                flag(MOTION, true, 5000),
                flag(MOTION, false, 5010),
            ],
        ),
        (id(LUX), vec![lux(8.0, 0), lux(300.0, 4000)]),
        (id(OCCUPANCY), vec![flag(OCCUPANCY, false, 0)]),
    ]);
    let (runs, changes) = sim::backtest(&hallway(), &history, &[], at(10_000));
    assert_eq!(changes, 5);
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].started_at, at(100));
    assert!(
        runs[0].steps.iter().any(|s| s.node.as_str() == "off"),
        "dark then: on, and off again"
    );
    assert_eq!(
        runs[1].summary().summary,
        "ended at dark → no",
        "bright by then"
    );
}

#[test]
fn the_graph_is_checked_where_a_canvas_can_go_wrong() {
    let bad = flow(serde_json::json!({
        "id": "bad", "name": "Bad",
        "nodes": {
            "motion": { "type": "trigger", "trigger": { "type": "state", "entity": MOTION, "to": true } },
            "a": { "type": "call", "service": "light.turn_on", "entity": LIGHT },
            "b": { "type": "call", "service": "light.turn_off", "entity": LIGHT },
            "lonely": { "type": "delay", "for": "1s" },
            "check": { "type": "gate", "condition": { "type": "expr", "expr": "var('level') > 3" } },
            "level": { "type": "set", "name": "level", "expr": "4" },
            "nope": { "type": "call", "service": "light.turn_on", "entity": "light.nope" }
        },
        "wires": [
            ["motion", "a"], ["motion", "b"], ["a", "check"], ["b", "check"],
            ["a", "level"], ["level", "nope"]
        ]
    }));
    let problems = validate::check(&bad, &registry());
    let about = |node: &str| {
        problems
            .iter()
            .filter(|p| p.node.as_ref().is_some_and(|n| n.as_str() == node))
            .map(|p| (p.severity, p.message.clone()))
            .collect::<Vec<_>>()
    };
    assert!(
        about("check")
            .iter()
            .any(|(s, m)| *s == Severity::Warning && m.contains("meet again")),
        "{problems:#?}"
    );
    assert!(
        about("check")
            .iter()
            .any(|(s, m)| *s == Severity::Error && m.contains("var('level')")),
        "{problems:#?}"
    );
    assert!(
        about("lonely")
            .iter()
            .any(|(_, m)| m.contains("never runs")),
        "{problems:#?}"
    );
    assert!(
        about("nope")
            .iter()
            .any(|(s, m)| *s == Severity::Error && m.contains("no such entity")),
        "{problems:#?}"
    );

    // Paths that part at a gate's two ports never both run: they meet freely.
    assert!(validate::check(&hallway(), &registry()).is_empty());

    let circle = flow(serde_json::json!({
        "id": "circle", "name": "Circle",
        "nodes": {
            "motion": { "type": "trigger", "trigger": { "type": "state", "entity": MOTION, "to": true } },
            "a": { "type": "delay", "for": "1s" },
            "b": { "type": "delay", "for": "1s" }
        },
        "wires": [["motion", "a"], ["a", "b"], ["b", "a"], ["b", "motion"]]
    }));
    let problems = validate::check(&circle, &registry());
    assert!(
        problems
            .iter()
            .any(|p| p.message.contains("into a trigger")),
        "{problems:#?}"
    );
    let circle = flow(serde_json::json!({
        "id": "circle", "name": "Circle",
        "nodes": {
            "motion": { "type": "trigger", "trigger": { "type": "state", "entity": MOTION, "to": true } },
            "a": { "type": "delay", "for": "1s" },
            "b": { "type": "delay", "for": "1s" }
        },
        "wires": [["motion", "a"], ["a", "b"], ["b", "a"]]
    }));
    let problems = validate::check(&circle, &registry());
    assert!(
        problems.iter().any(|p| p.message.contains("circle")),
        "{problems:#?}"
    );
}

#[test]
fn queued_runs_take_their_turn() {
    let mut queued = hallway();
    queued.mode =
        serde_json::from_value(serde_json::json!({ "type": "queued", "max": 1 })).expect("a mode");
    let mut engine = engine_with(queued);
    change(&mut engine, flag(OCCUPANCY, true, 1));
    change(&mut engine, flag(MOTION, true, 10));
    change(&mut engine, flag(MOTION, false, 11));
    change(&mut engine, flag(MOTION, true, 12));
    change(&mut engine, flag(MOTION, false, 13));
    change(&mut engine, flag(MOTION, true, 14));
    let (calls, _, misses) = effects(&mut engine, at(14));
    assert_eq!(calls.len(), 1, "one running, one waiting");
    assert_eq!(misses.len(), 1, "the third had nowhere to wait");
    // The first run's call was answered at 14, so its ten-minute wait ends at 614.
    engine.advance(at(14 + 600));
    let (calls, done, _) = effects(&mut engine, at(614));
    assert_eq!(done.len(), 1);
    assert_eq!(
        calls.len(),
        2,
        "the first turns off, the queued one turns on"
    );
}

#[test]
fn a_join_that_timed_out_doesnt_go_on_again_when_the_late_path_arrives() {
    let late = flow(serde_json::json!({
        "id": "late", "name": "Late",
        "nodes": {
            "motion": { "type": "trigger", "trigger": { "type": "state", "entity": MOTION, "to": true } },
            "slow": { "type": "delay", "for": "2m" },
            "dark": { "type": "gate", "condition": { "type": "expr", "expr": format!("num('{LUX}') < 30") } },
            "both": { "type": "join", "mode": "all", "timeout": "1m" },
            "on": { "type": "call", "service": "light.turn_on", "entity": LIGHT },
            "off": { "type": "call", "service": "light.turn_off", "entity": LIGHT }
        },
        "wires": [["motion", "slow"], ["motion", "dark"], ["slow", "both"], ["dark:yes", "both"],
                  ["both", "on"], ["both:timeout", "off"]]
    }));
    let mut engine = engine_with(late);
    change(&mut engine, flag(MOTION, true, 10));
    engine.advance(at(70));
    let (calls, _, _) = effects(&mut engine, at(70));
    assert_eq!(
        calls,
        [(id(LIGHT), "light.turn_off".to_owned())],
        "it timed out"
    );
    engine.advance(at(130));
    let (calls, done, _) = effects(&mut engine, at(130));
    assert!(
        calls.is_empty(),
        "the late path ends at the join: {calls:?}"
    );
    assert!(
        done[0]
            .steps
            .iter()
            .any(|s| s.note.as_deref() == Some("arrived after the join had already gone on"))
    );
}

#[test]
fn cancelling_a_queued_flows_run_lets_the_next_one_start() {
    let mut queued = hallway();
    queued.mode =
        serde_json::from_value(serde_json::json!({ "type": "queued", "max": 2 })).expect("a mode");
    let mut engine = engine_with(queued);
    change(&mut engine, flag(OCCUPANCY, true, 1));
    change(&mut engine, flag(MOTION, true, 10));
    change(&mut engine, flag(MOTION, false, 11));
    change(&mut engine, flag(MOTION, true, 12));
    let (calls, _, _) = effects(&mut engine, at(12));
    assert_eq!(calls.len(), 1, "one running, one waiting");
    let first = engine.active(None)[0].record.run_id.clone();
    assert!(engine.cancel(&first, at(20)));
    let (calls, done, _) = effects(&mut engine, at(20));
    assert_eq!(
        done[0].outcome,
        Some(Outcome::Aborted {
            reason: AbortReason::Cancelled
        })
    );
    assert_eq!(calls.len(), 1, "the waiting one started");
    assert_eq!(engine.active(None).len(), 1);
}

#[test]
fn a_resync_looks_at_every_wait_again() {
    let mut engine = engine_with(flow(serde_json::json!({
        "id": "waits", "name": "Waits",
        "nodes": {
            "motion": { "type": "trigger", "trigger": { "type": "state", "entity": MOTION, "to": true } },
            "clear": { "type": "wait", "until": { "type": "state", "entity": OCCUPANCY, "is": false }, "timeout": "10m" },
            "off": { "type": "call", "service": "light.turn_off", "entity": LIGHT }
        },
        "wires": [["motion", "clear"], ["clear:matched", "off"]]
    })));
    change(&mut engine, flag(OCCUPANCY, true, 1));
    change(&mut engine, flag(MOTION, true, 10));
    let (calls, _, _) = effects(&mut engine, at(10));
    assert!(calls.is_empty());
    // The change to clear was missed; a resync with the home as it is now still sees it.
    let mut home = engine.states();
    home.retain(|s| s.entity_id != id(OCCUPANCY));
    home.push(flag(OCCUPANCY, false, 30));
    engine.resync(home, at(40));
    let (calls, _, _) = effects(&mut engine, at(40));
    assert_eq!(calls, [(id(LIGHT), "light.turn_off".to_owned())]);
}

/// Motion works out a brightness from the light level, and the light takes it.
fn worked_out(level: &str) -> Flow {
    flow(serde_json::json!({
        "id": "worked_out", "name": "Worked out",
        "nodes": {
            "motion": { "type": "trigger", "trigger": { "type": "state", "entity": MOTION, "to": true } },
            "level": { "type": "set", "name": "level", "expr": level },
            "on": { "type": "call", "service": "light.turn_on", "entity": LIGHT,
                    "data": { "brightness_pct": { "expr": "var('level')" } } }
        },
        "wires": [["motion", "level"], ["level", "on"]]
    }))
}

#[test]
fn a_call_takes_a_setting_worked_out_earlier_in_the_run() {
    let level = format!("round(clamp(70 - num('{LUX}') / 600 * 25, 45, 70))");
    let flow = worked_out(&level);
    assert!(validate::check(&flow, &registry()).is_empty());
    let mut engine = engine_with(flow);
    change(&mut engine, lux(300.0, 5));
    change(&mut engine, flag(MOTION, true, 10));
    let asked = engine.take_effects();
    let Some(Effect::Call { call_id, data, .. }) = asked.first() else {
        panic!("a call, not {asked:?}");
    };
    let Some(light) = data else {
        panic!("light settings");
    };
    // 70 - 300 / 600 * 25 = 57.5, rounded.
    assert_eq!(light.0["brightness_pct"], 58);
    engine.call_finished(*call_id, Ok(()), at(10));
    let (_, done, _) = effects(&mut engine, at(10));
    let on = done[0]
        .steps
        .iter()
        .find(|s| s.node.as_str() == "on")
        .expect("as written");
    assert_eq!(
        on.call.as_ref().expect("as written").data,
        Some(serde_json::json!({ "brightness_pct": 58 }))
    );
}

#[test]
fn a_worked_out_setting_is_checked_like_any_expression() {
    // Text where a number belongs.
    let problems = validate::check(&worked_out("'bright'"), &registry());
    assert!(
        problems
            .iter()
            .any(|p| p.node.as_ref().is_some_and(|n| n.as_str() == "on")
                && p.message.contains("work out to a number")),
        "{problems:#?}"
    );
    // A variable nothing sets before the call.
    let mut unset = worked_out("50");
    unset.wires.retain(|w| w.to.as_str() != "on");
    unset
        .wires
        .push(serde_json::from_value(serde_json::json!(["motion", "on"])).expect("as written"));
    let problems = validate::check(&unset, &registry());
    assert!(
        problems.iter().any(|p| p.message.contains("var('level')")),
        "{problems:#?}"
    );
}

#[test]
fn a_setting_that_cant_be_worked_out_is_a_failed_call() {
    let mut direct = worked_out("1");
    direct.nodes.remove(&"level".parse().expect("as written"));
    direct.wires =
        vec![serde_json::from_value(serde_json::json!(["motion", "on"])).expect("as written")];
    if let Some(irori_flow_types::Node::Call {
        data: Some(data), ..
    }) = direct.nodes.get_mut(&"on".parse().expect("as written"))
    {
        data.0.insert(
            "brightness_pct".into(),
            serde_json::from_value(serde_json::json!({ "expr": format!("num('{LUX}')") }))
                .expect("as written"),
        );
    }
    assert!(validate::check(&direct, &registry()).is_empty());
    let mut engine = engine_with(direct);
    let mut offline = lux(8.0, 5);
    offline.availability = Availability::Unavailable;
    change(&mut engine, offline);
    change(&mut engine, flag(MOTION, true, 10));
    let (calls, done, _) = effects(&mut engine, at(10));
    assert!(calls.is_empty(), "nothing is sent");
    assert!(
        matches!(&done[0].outcome, Some(Outcome::Error { node, message }) if node.as_str() == "on" && message.contains("unavailable")),
        "{:?}",
        done[0].outcome
    );
}

#[test]
fn a_no_from_several_checks_says_which_one_and_reads_them_all() {
    let checks = flow(serde_json::json!({
        "id": "checks", "name": "Checks",
        "nodes": {
            "motion": { "type": "trigger", "trigger": { "type": "state", "entity": MOTION, "to": true } },
            "wanted": { "type": "gate", "condition": { "type": "all", "conditions": [
                { "type": "state", "entity": OCCUPANCY, "is": true },
                { "type": "expr", "expr": format!("num('{LUX}') < 30") },
                { "type": "state", "entity": GUESTS, "is": false }
            ] } },
            "on": { "type": "call", "service": "light.turn_on", "entity": LIGHT }
        },
        "wires": [["motion", "wanted"], ["wanted:yes", "on"]]
    }));
    let mut engine = engine_with(checks);
    change(&mut engine, flag(MOTION, true, 10));
    let (calls, done, _) = effects(&mut engine, at(10));
    assert!(calls.is_empty());
    let gate = &done[0].steps[1];
    let note = gate.note.as_deref().unwrap_or_default();
    // Occupancy is what said no; the dark and the guests were fine.
    assert!(note.contains(&format!("{OCCUPANCY} is on ✗")), "{note}");
    assert!(note.contains(&format!("num('{LUX}') < 30 ✓")), "{note}");
    assert!(note.ends_with("→ no"), "{note}");
    assert_eq!(gate.reads.len(), 3, "every check's reading is kept");
}

/// A light that comes on when the hall gets dark and stays dark for 30s.
fn got_dark() -> Flow {
    flow(serde_json::json!({
        "id": "got_dark", "name": "Got dark",
        "nodes": {
            "dark": { "type": "trigger", "trigger": { "type": "state", "entity": LUX, "below": 30, "for": "30s" } },
            "on": { "type": "call", "service": "light.turn_on", "entity": LIGHT }
        },
        "wires": [["dark", "on"]]
    }))
}

#[test]
fn a_level_trigger_fires_once_it_has_held_below_and_not_while_it_wobbles() {
    assert!(validate::check(&got_dark(), &registry()).is_empty());
    let mut engine = engine_with(got_dark());
    change(&mut engine, lux(300.0, 1));
    // Down below 30, then about inside the range: the hold keeps going.
    change(&mut engine, lux(25.0, 10));
    change(&mut engine, lux(22.0, 20));
    change(&mut engine, lux(28.0, 30));
    let (calls, _, misses) = effects(&mut engine, at(30));
    assert!(calls.is_empty(), "30s haven't passed yet");
    assert!(misses.is_empty(), "{misses:?}");
    engine.advance(at(40));
    let (calls, done, _) = effects(&mut engine, at(40));
    assert_eq!(calls, [(id(LIGHT), "light.turn_on".to_owned())]);
    assert_eq!(done.len(), 1);
    // Already below: another dark reading isn't another crossing.
    change(&mut engine, lux(10.0, 50));
    engine.advance(at(90));
    let (calls, _, _) = effects(&mut engine, at(90));
    assert!(calls.is_empty());
}

#[test]
fn a_level_trigger_that_goes_back_up_before_its_hold_is_a_near_miss() {
    let mut engine = engine_with(got_dark());
    change(&mut engine, lux(300.0, 1));
    change(&mut engine, lux(20.0, 10));
    change(&mut engine, lux(80.0, 20));
    engine.advance(at(60));
    let (calls, _, misses) = effects(&mut engine, at(60));
    assert!(calls.is_empty());
    assert_eq!(misses.len(), 1);
    assert_eq!(misses[0].kind, NearMissKind::HoldReset);
}

#[test]
fn a_trigger_can_wait_for_any_of_several_values() {
    let tv = flow(serde_json::json!({
        "id": "stopped", "name": "Stopped",
        "nodes": {
            "stopped": { "type": "trigger", "trigger": { "type": "state", "entity": GUESTS, "to": [true, false] } },
            "on": { "type": "call", "service": "light.turn_on", "entity": LIGHT }
        },
        "wires": [["stopped", "on"]]
    }));
    assert!(validate::check(&tv, &registry()).is_empty());
    let written = serde_json::to_value(&tv).expect("serializes");
    assert_eq!(
        written["nodes"]["stopped"]["trigger"]["to"],
        serde_json::json!([true, false])
    );
    let mut engine = engine_with(tv);
    change(&mut engine, flag(GUESTS, true, 5));
    let (first, _, _) = effects(&mut engine, at(5));
    change(&mut engine, flag(GUESTS, false, 6));
    let (second, _, _) = effects(&mut engine, at(6));
    assert_eq!((first.len(), second.len()), (1, 1));
}

#[test]
fn levels_and_value_lists_are_checked() {
    let bad = |trigger: serde_json::Value| {
        serde_json::from_value::<Flow>(serde_json::json!({
            "id": "x", "name": "X",
            "nodes": { "t": { "type": "trigger", "trigger": trigger } }
        }))
    };
    assert!(
        bad(serde_json::json!({ "type": "state", "entity": LUX, "below": 30, "to": 5 })).is_err()
    );
    assert!(
        bad(serde_json::json!({ "type": "state", "entity": LUX, "above": 30, "below": 10 }))
            .is_err()
    );
    assert!(
        bad(serde_json::json!({ "type": "state", "entity": GUESTS, "to": [true, true] })).is_err()
    );
    assert!(bad(serde_json::json!({ "type": "state", "entity": GUESTS, "to": [] })).is_err());
    assert!(
        bad(serde_json::json!({ "type": "state", "entity": GUESTS, "from": [true, true] }))
            .is_err()
    );
    assert!(bad(serde_json::json!({ "type": "state", "entity": GUESTS, "from": [] })).is_err());
    assert!(
        bad(serde_json::json!({ "type": "state", "entity": LUX, "below": 30, "from": [5, 6] }))
            .is_err()
    );
    let on_a_switch = bad(serde_json::json!({ "type": "state", "entity": GUESTS, "below": 3 }))
        .expect("the file is fine");
    let problems = validate::check(&on_a_switch, &registry());
    assert!(
        problems
            .iter()
            .any(|p| p.message.contains("sensor with numbers")),
        "{problems:#?}"
    );
}

fn playback(word: Playback, seconds: i64) -> EntityState {
    state(
        TV,
        State::MediaPlayer(MediaPlayerState {
            state: word,
            volume: None,
            muted: None,
            title: None,
            artist: None,
            album: None,
            app: None,
            content_type: None,
            duration: None,
            position: None,
        }),
        seconds,
    )
}

fn home_with_tv() -> MapRegistry {
    let mut home = registry();
    home.entities.insert(
        id(TV),
        entity(
            TV,
            Capabilities::MediaPlayer(MediaPlayerCapabilities {
                device_class: Some(MediaPlayerClass::Tv),
                volume: true,
                mute: true,
                seek: false,
                play_media: true,
                queue: false,
                turn_on: true,
                turn_off: true,
            }),
        ),
    );
    home
}

/// Playing turns the light on, once the check agrees it is playing. Paused turns it off.
fn tv_lights() -> Flow {
    flow(serde_json::json!({
        "id": "tv_lights", "name": "TV lights",
        "nodes": {
            "playing": { "type": "trigger", "trigger": { "type": "state", "entity": TV, "to": "playing" } },
            "paused": { "type": "trigger", "trigger": { "type": "state", "entity": TV, "to": "paused" } },
            "is_playing": { "type": "gate", "condition": { "type": "state", "entity": TV, "is": "playing" } },
            "on": { "type": "call", "service": "light.turn_on", "entity": LIGHT },
            "off": { "type": "call", "service": "light.turn_off", "entity": LIGHT }
        },
        "wires": [
            ["playing", "is_playing"],
            ["paused", "is_playing"],
            ["is_playing:yes", "on"],
            ["is_playing:no", "off"]
        ]
    }))
}

fn engine_with_tv(flow: Flow) -> Engine {
    let mut engine = Engine::new(Box::new(CountingIds::default()));
    engine.load_states([
        flag(MOTION, false, 0),
        flag(OCCUPANCY, false, 0),
        lux(8.0, 0),
        flag(LIGHT, false, 0),
        flag(GUESTS, false, 0),
        playback(Playback::Idle, 0),
    ]);
    let problems = validate::check(&flow, &home_with_tv());
    assert!(problems.is_empty(), "{problems:#?}");
    engine.set_flows(vec![Arm { flow, problems }], at(0));
    engine
}

#[test]
fn a_media_player_fires_when_it_changes_to_playing_and_a_check_can_ask_if_it_is() {
    let mut engine = engine_with_tv(tv_lights());

    change(&mut engine, playback(Playback::Playing, 10));
    let (calls, done, _) = effects(&mut engine, at(10));
    assert_eq!(calls, [(id(LIGHT), "light.turn_on".to_owned())]);
    assert!(
        done[0].steps[0]
            .note
            .as_deref()
            .is_some_and(|note| note.contains("\"idle\" → \"playing\"")),
        "{:?}",
        done[0].steps[0].note
    );
    assert!(
        done[0].steps[1]
            .note
            .as_deref()
            .is_some_and(|note| note.contains("→ yes")),
        "{:?}",
        done[0].steps[1].note
    );

    change(&mut engine, playback(Playback::Paused, 20));
    let (calls, done, _) = effects(&mut engine, at(20));
    assert_eq!(calls, [(id(LIGHT), "light.turn_off".to_owned())]);
    assert!(
        done[0].steps[1]
            .note
            .as_deref()
            .is_some_and(|note| note.contains("→ no")),
        "{:?}",
        done[0].steps[1].note
    );

    // Idle is a playback word this flow leaves alone.
    change(&mut engine, playback(Playback::Idle, 30));
    let (calls, _, _) = effects(&mut engine, at(30));
    assert!(calls.is_empty());

    let unknown = flow(serde_json::json!({
        "id": "bad_word", "name": "Bad word",
        "nodes": {
            "playing": { "type": "trigger", "trigger": { "type": "state", "entity": TV, "to": "on" } }
        }
    }));
    let problems = validate::check(&unknown, &home_with_tv());
    assert!(
        problems
            .iter()
            .any(|problem| problem.message.contains("never")),
        "{problems:#?}"
    );
}

/// The light goes on when the TV does what `trigger` says.
fn tv_trigger(trigger: serde_json::Value) -> Flow {
    flow(serde_json::json!({
        "id": "tv_stopped", "name": "TV stopped",
        "nodes": {
            "tv": { "type": "trigger", "trigger": trigger },
            "on": { "type": "call", "service": "light.turn_on", "entity": LIGHT }
        },
        "wires": [["tv", "on"]]
    }))
}

/// How many runs the TV changing to `word` started.
fn runs_after(engine: &mut Engine, word: Playback, seconds: i64) -> usize {
    change(engine, playback(word, seconds));
    effects(engine, at(seconds)).0.len()
}

#[test]
fn a_trigger_can_say_what_it_changes_from_and_leave_out_what_to() {
    let mut engine = engine_with_tv(tv_trigger(serde_json::json!({
        "type": "state", "entity": TV, "from": "playing"
    })));

    // Idle → playing isn't a change from playing.
    assert_eq!(runs_after(&mut engine, Playback::Playing, 10), 0);
    // Playing → anything else is.
    assert_eq!(runs_after(&mut engine, Playback::Paused, 20), 1);
    assert_eq!(runs_after(&mut engine, Playback::Idle, 30), 0);
    assert_eq!(runs_after(&mut engine, Playback::Playing, 40), 0);
    assert_eq!(runs_after(&mut engine, Playback::Off, 50), 1);
}

#[test]
fn a_trigger_with_from_and_to_fires_on_that_change_only() {
    let stopped = tv_trigger(serde_json::json!({
        "type": "state", "entity": TV, "from": ["playing", "paused"], "to": ["idle", "off"]
    }));
    // A list is written back as a list.
    let written = serde_json::to_value(&stopped).expect("writes");
    assert_eq!(
        written["nodes"]["tv"]["trigger"]["from"],
        serde_json::json!(["playing", "paused"])
    );
    let mut engine = engine_with_tv(stopped);

    assert_eq!(runs_after(&mut engine, Playback::Playing, 10), 0);
    assert_eq!(runs_after(&mut engine, Playback::Paused, 20), 0);
    // Paused → idle: from one of them, to one of them.
    assert_eq!(runs_after(&mut engine, Playback::Idle, 30), 1);
    // Idle → off goes to the right place, but not from the right one.
    assert_eq!(runs_after(&mut engine, Playback::Off, 40), 0);
    assert_eq!(runs_after(&mut engine, Playback::Buffering, 50), 0);
    assert_eq!(runs_after(&mut engine, Playback::Idle, 60), 0);
    assert_eq!(runs_after(&mut engine, Playback::Playing, 70), 0);
    assert_eq!(runs_after(&mut engine, Playback::Off, 80), 1);
}

#[test]
fn a_hold_keeps_going_while_the_value_stays_where_the_trigger_wants_it() {
    let mut engine = engine_with_tv(tv_trigger(serde_json::json!({
        "type": "state", "entity": TV, "from": "playing", "to": ["paused", "idle"], "for": "10s"
    })));
    change(&mut engine, playback(Playback::Playing, 10));
    change(&mut engine, playback(Playback::Paused, 20));
    // Paused → idle isn't from playing, but it's still stopped: the ten seconds keep counting.
    change(&mut engine, playback(Playback::Idle, 25));
    let (calls, _, misses) = effects(&mut engine, at(25));
    assert!(calls.is_empty());
    assert!(misses.is_empty(), "{misses:#?}");
    engine.advance(at(30));
    let (calls, _, _) = effects(&mut engine, at(30));
    assert_eq!(calls.len(), 1);

    // Going somewhere else before the time is up is a near-miss, as it always was.
    change(&mut engine, playback(Playback::Playing, 40));
    change(&mut engine, playback(Playback::Paused, 50));
    change(&mut engine, playback(Playback::Playing, 55));
    engine.advance(at(70));
    let (calls, _, misses) = effects(&mut engine, at(70));
    assert!(calls.is_empty());
    assert_eq!(misses[0].kind, NearMissKind::HoldReset);
}

#[test]
fn a_hold_from_a_value_ends_when_it_goes_back_to_it() {
    let mut engine = engine_with_tv(tv_trigger(serde_json::json!({
        "type": "state", "entity": TV, "from": "playing", "for": "10s"
    })));
    change(&mut engine, playback(Playback::Playing, 10));
    change(&mut engine, playback(Playback::Paused, 20));
    change(&mut engine, playback(Playback::Playing, 25));
    engine.advance(at(40));
    let (calls, _, misses) = effects(&mut engine, at(40));
    assert!(calls.is_empty());
    assert_eq!(misses[0].kind, NearMissKind::HoldReset);

    // Away from playing and staying away, wherever it wanders, fires.
    change(&mut engine, playback(Playback::Paused, 50));
    change(&mut engine, playback(Playback::Idle, 55));
    engine.advance(at(60));
    let (calls, _, _) = effects(&mut engine, at(60));
    assert_eq!(calls.len(), 1);
}

#[test]
fn what_a_trigger_changes_from_is_checked_like_what_it_changes_to() {
    let problems_of = |trigger: serde_json::Value, home: &MapRegistry| {
        validate::check(&tv_trigger(trigger), home)
            .into_iter()
            .map(|problem| format!("{:?}: {}", problem.field, problem.message))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let unknown_word = problems_of(
        serde_json::json!({ "type": "state", "entity": TV, "from": ["playing", "on"] }),
        &home_with_tv(),
    );
    assert!(unknown_word.contains("from"), "{unknown_word}");
    assert!(unknown_word.contains("never \"on\""), "{unknown_word}");

    let wrong_shape = problems_of(
        serde_json::json!({ "type": "state", "entity": TV, "from": true }),
        &home_with_tv(),
    );
    assert!(wrong_shape.contains("from"), "{wrong_shape}");
    assert!(wrong_shape.contains("a boolean"), "{wrong_shape}");

    let nowhere = problems_of(
        serde_json::json!({ "type": "state", "entity": TV, "from": "playing", "to": ["playing"] }),
        &home_with_tv(),
    );
    assert!(nowhere.contains("never fires"), "{nowhere}");
    // Lists that share a value still have somewhere to go.
    let somewhere = problems_of(
        serde_json::json!({
            "type": "state", "entity": TV, "from": ["playing", "paused"], "to": ["paused", "idle"]
        }),
        &home_with_tv(),
    );
    assert!(somewhere.is_empty(), "{somewhere}");
}

const BUTTON: &str = "event.desk_button_action";
const BLIND: &str = "cover.study_blind";

fn press(event_type: &str, seconds: i64) -> EntityState {
    state(
        BUTTON,
        State::Event(irori_types::EventState {
            event_type: event_type.to_owned(),
        }),
        seconds,
    )
}

/// A home with a smart button (a Tuya IH-K663: `single` and `double`) and a blind that can be
/// sent to a position but has no slats.
fn home_with_button() -> MapRegistry {
    let mut home = registry();
    home.entities.insert(
        id(BUTTON),
        entity(
            BUTTON,
            Capabilities::Event(irori_types::EventCapabilities {
                event_types: vec!["single".into(), "double".into()],
                device_class: None,
            }),
        ),
    );
    home.entities.insert(
        id(BLIND),
        entity(
            BLIND,
            Capabilities::Cover(irori_types::CoverCapabilities {
                device_class: None,
                position: true,
                tilt: false,
                stop: true,
            }),
        ),
    );
    home
}

/// One press toggles the light; a double press sends the blind half way.
fn button_flow() -> Flow {
    flow(serde_json::json!({
        "id": "desk_button", "name": "Desk button",
        "nodes": {
            "single": { "type": "trigger", "trigger": { "type": "state", "entity": BUTTON, "to": "single" } },
            "double": { "type": "trigger", "trigger": { "type": "state", "entity": BUTTON, "to": "double" } },
            "toggle": { "type": "call", "service": "light.toggle", "entity": LIGHT },
            "half": { "type": "call", "service": "cover.set_position", "entity": BLIND,
                      "data": { "position": 50 } }
        },
        "wires": [["single", "toggle"], ["double", "half"]]
    }))
}

fn engine_with_button(flow: Flow) -> Engine {
    let mut engine = Engine::new(Box::new(CountingIds::default()));
    engine.load_states([flag(LIGHT, false, 0), press("single", 0)]);
    let problems = validate::check(&flow, &home_with_button());
    assert!(problems.is_empty(), "{problems:#?}");
    engine.set_flows(vec![Arm { flow, problems }], at(0));
    engine
}

#[test]
fn every_press_of_a_button_fires_even_the_same_one_twice() {
    let mut engine = engine_with_button(button_flow());

    // The value it already had, pressed again: a change of nothing, and still a press.
    change(&mut engine, press("single", 10));
    let (calls, done, _) = effects(&mut engine, at(10));
    assert_eq!(calls, [(id(LIGHT), "light.toggle".to_owned())]);
    assert!(
        done[0].steps[0]
            .note
            .as_deref()
            .is_some_and(|note| note.contains("\"single\"")),
        "{:?}",
        done[0].steps[0].note
    );
    change(&mut engine, press("single", 20));
    let (calls, _, _) = effects(&mut engine, at(20));
    assert_eq!(calls, [(id(LIGHT), "light.toggle".to_owned())]);

    // Another kind of press starts the other path, and the call carries its setting.
    change(&mut engine, press("double", 30));
    let asked = engine.take_effects();
    let Some(Effect::Call {
        entity,
        service,
        data,
        ..
    }) = asked.first()
    else {
        panic!("a call, not {asked:?}");
    };
    assert_eq!(entity, &id(BLIND));
    assert_eq!(service.to_string(), "cover.set_position");
    assert_eq!(data.as_ref().expect("its position").0["position"], 50);
}

#[test]
fn a_button_going_out_of_reach_and_back_is_not_a_press() {
    let mut engine = engine_with_button(button_flow());
    change(&mut engine, press("single", 10));
    let _ = effects(&mut engine, at(10));

    // Unavailable, then back: reported again, but nothing happened. The core leaves
    // `last_changed` where the press put it.
    let mut gone = press("single", 10);
    gone.availability = Availability::Unavailable;
    gone.last_updated = at(20);
    change(&mut engine, gone);
    let mut back = press("single", 10);
    back.last_updated = at(30);
    change(&mut engine, back);

    let (calls, _, _) = effects(&mut engine, at(30));
    assert!(calls.is_empty(), "{calls:?}");
}

#[test]
fn a_press_is_not_something_to_check_or_wait_for_and_a_blind_is_asked_only_what_it_can_do() {
    let problems_of = |nodes: serde_json::Value| {
        let flow = flow(serde_json::json!({ "id": "odd", "name": "Odd", "nodes": nodes }));
        validate::check(&flow, &home_with_button())
            .into_iter()
            .map(|problem| problem.message)
            .collect::<Vec<_>>()
            .join("\n")
    };

    let unknown_press = problems_of(serde_json::json!({
        "t": { "type": "trigger", "trigger": { "type": "state", "entity": BUTTON, "to": "hold" } }
    }));
    assert!(unknown_press.contains("never"), "{unknown_press}");

    let held = problems_of(serde_json::json!({
        "t": { "type": "trigger", "trigger": { "type": "state", "entity": BUTTON, "to": "single", "for": "5s" } }
    }));
    assert!(held.contains("can't hold"), "{held}");

    let checked = problems_of(serde_json::json!({
        "t": { "type": "trigger", "trigger": { "type": "state", "entity": BUTTON } },
        "g": { "type": "gate", "condition": { "type": "state", "entity": BUTTON, "is": "single" } }
    }));
    assert!(checked.contains("trigger instead"), "{checked}");

    // No slats, so nothing to tilt; a position past the end; a word that isn't a number.
    let tilted = problems_of(serde_json::json!({
        "t": { "type": "trigger", "trigger": { "type": "state", "entity": BUTTON } },
        "c": { "type": "call", "service": "cover.set_tilt", "entity": BLIND, "data": { "tilt": 40 } }
    }));
    assert!(tilted.contains("tilt"), "{tilted}");

    // A setting worked out when it runs is checked by its range, not a stand-in's luck.
    let worked = problems_of(serde_json::json!({
        "t": { "type": "trigger", "trigger": { "type": "state", "entity": BUTTON } },
        "c": { "type": "call", "service": "cover.set_position", "entity": BLIND,
               "data": { "position": { "expr": format!("num('{LUX}') / 10") } } }
    }));
    assert!(
        !worked.contains("position"),
        "a worked-out position is fine: {worked}"
    );

    // Past the end of what a position can be: the blind itself says so.
    let past = problems_of(serde_json::json!({
        "t": { "type": "trigger", "trigger": { "type": "state", "entity": BUTTON } },
        "c": { "type": "call", "service": "cover.set_position", "entity": BLIND,
               "data": { "position": 140 } }
    }));
    assert!(past.contains("140"), "{past}");

    for (service, data, said) in [
        ("cover.set_position", serde_json::json!({}), "position"),
        (
            "cover.open",
            serde_json::json!({ "position": 10 }),
            "does not take data",
        ),
        ("lock.unlock", serde_json::json!({ "code": "1234" }), "code"),
    ] {
        let refused = serde_json::from_value::<Flow>(serde_json::json!({
            "id": "odd", "name": "Odd",
            "nodes": { "c": { "type": "call", "service": service, "entity": BLIND, "data": data } }
        }))
        .expect_err("refused as it's read");
        assert!(refused.to_string().contains(said), "{service}: {refused}");
    }
}

/// A lamp takes a narrower band of colour temperatures than lights in general. A temperature
/// worked out when the call runs can't be checked against it beforehand, and mustn't be
/// refused for where a stand-in happened to fall; a lamp with no colour temperature at all
/// still is.
#[test]
fn a_worked_out_setting_is_not_refused_for_where_a_stand_in_falls() {
    const LAMP: &str = "light.reading_lamp";
    let with_lamp = |color_temp_kelvin| {
        let mut home = registry();
        home.entities.insert(
            id(LAMP),
            entity(
                LAMP,
                Capabilities::Light(irori_types::LightCapabilities {
                    brightness: true,
                    color_temp_kelvin,
                    rgb: false,
                }),
            ),
        );
        home
    };
    let warm = flow(serde_json::json!({
        "id": "warm", "name": "Warm",
        "nodes": {
            "t": { "type": "trigger", "trigger": { "type": "state", "entity": MOTION, "to": true } },
            "on": { "type": "call", "service": "light.turn_on", "entity": LAMP,
                    "data": { "color_temp_kelvin": { "expr": format!("2200 + num('{LUX}')") } } }
        },
        "wires": [["t", "on"]]
    }));

    let narrow = with_lamp(Some(irori_types::ColorTempRange {
        min: 2200,
        max: 4000,
    }));
    let problems = validate::check(&warm, &narrow);
    assert!(problems.is_empty(), "{problems:#?}");

    let problems = validate::check(&warm, &with_lamp(None));
    assert!(
        problems
            .iter()
            .any(|problem| problem.message.contains("color temperature")),
        "{problems:#?}"
    );
}
