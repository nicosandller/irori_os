//! Running flows (`docs/specs/flows.md` §2.3, §3, §6).
//!
//! Sans-IO (F7): the engine never awaits, sleeps or reads the clock. It's handed what happened
//! and when — a state change, a call's answer, time passing — and hands back the calls to make
//! and the runs and near-misses to keep ([`Effect`]). The same code runs live, in a dry run with
//! a virtual clock, and in a backtest over history.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use irori_flow_types::api::{ActiveRun, Armed, Problem, TokenAt};
use irori_flow_types::trace::{
    AbortReason, CallDetail, Ending, NearMiss, NearMissKind, Outcome, Read, RunRecord, Step,
    TestKind,
};
use irori_flow_types::{
    CallData, Condition, Flow, JoinMode, LimitedMode, Mode, NamedMode, Node, NodeId, Port,
    RuleService, Trigger, TypedValue, Values, WaitUntil, Wire,
};
use irori_rules::eval::{self, Evaluator, Snapshot};
use irori_types::{Availability, ContextId, EntityId, EntityState, RuleId, Timestamp};

/// The most steps one run may take: fan-out can't loop, but it can multiply.
pub const MAX_STEPS: u32 = 256;

/// Where run ids come from. The extension mints ULIDs from the clock and randomness; a dry run
/// or a backtest counts, so the same input gives the same output.
pub trait IdGen: Send {
    fn run_id(&mut self, now: Timestamp) -> ContextId;
}

/// A ULID: 48 bits of milliseconds, then 80 of `random`.
pub fn ulid(millis: u64, random: u128) -> ContextId {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let value: u128 = (u128::from(millis & 0xFFFF_FFFF_FFFF) << 80) | (random & ((1 << 80) - 1));
    let text: String = (0..26)
        .rev()
        .map(|i| ALPHABET[((value >> (i * 5)) & 31) as usize] as char)
        .collect();
    ContextId::try_from(text).expect("a ULID is always a valid context id")
}

/// Counts, for runs that don't need to be unique outside one simulation.
#[derive(Debug, Default)]
pub struct CountingIds(u128);

impl IdGen for CountingIds {
    fn run_id(&mut self, now: Timestamp) -> ContextId {
        self.0 += 1;
        ulid(millis(now), self.0)
    }
}

fn millis(t: Timestamp) -> u64 {
    u64::try_from(t.as_jiff().as_millisecond()).unwrap_or(0)
}

fn plus(t: Timestamp, ms: i64) -> Timestamp {
    Timestamp::from_jiff(
        t.as_jiff()
            .checked_add(jiff::SignedDuration::from_millis(ms))
            .unwrap_or(t.as_jiff()),
    )
}

/// A flow to run, with what layer 3 found wrong with it.
#[derive(Debug, Clone)]
pub struct Arm {
    pub flow: Flow,
    pub problems: Vec<Problem>,
}

/// Something for whoever drives the engine to do or keep.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Call this service, then say how it went with [`Engine::call_finished`].
    Call {
        call_id: u64,
        run_id: ContextId,
        parent_id: Option<ContextId>,
        entity: EntityId,
        service: RuleService,
        data: Option<CallData>,
    },
    /// A run ended; keep its record.
    Finished(Box<RunRecord>),
    /// Keep this near-miss.
    NearMiss(NearMiss),
}

#[derive(Debug, Clone)]
struct Firing {
    trigger: NodeId,
    cause: Option<ContextId>,
    test: Option<TestKind>,
    note: String,
    reads: Vec<Read>,
}

#[derive(Debug)]
struct Loaded {
    flow: Arc<Flow>,
    version: String,
    armed: Armed,
    /// `for` counting on a trigger: until when.
    holds: BTreeMap<NodeId, Timestamp>,
    queue: VecDeque<Firing>,
}

#[derive(Debug, Clone, PartialEq)]
enum Doing {
    Ready,
    Delay {
        until: Timestamp,
    },
    Wait {
        since: Timestamp,
        timeout: Timestamp,
        holding_since: Option<Timestamp>,
        watch: BTreeSet<EntityId>,
    },
    Call {
        call_id: u64,
    },
    Joined,
}

#[derive(Debug)]
struct Token {
    node: NodeId,
    via: Option<Wire>,
    /// Its step in the record, once it has one.
    step: Option<usize>,
    doing: Doing,
}

#[derive(Debug, Default)]
struct JoinState {
    /// The wires tokens have come in on, and the tokens parked here.
    arrived: BTreeMap<Wire, u32>,
    passed: bool,
    deadline: Option<Timestamp>,
}

#[derive(Debug)]
struct Run {
    flow: Arc<Flow>,
    record: RunRecord,
    tokens: BTreeMap<u32, Token>,
    next_token: u32,
    vars: BTreeMap<String, serde_json::Value>,
    joins: BTreeMap<NodeId, JoinState>,
    error: Option<(NodeId, String)>,
    stopped: Option<(NodeId, Option<String>)>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Timer {
    Hold { flow: RuleId, node: NodeId },
    Token { run: ContextId, token: u32 },
    Join { run: ContextId, node: NodeId },
}

/// The flow engine.
pub struct Engine {
    flows: BTreeMap<RuleId, Loaded>,
    states: Arc<BTreeMap<EntityId, EntityState>>,
    runs: BTreeMap<ContextId, Run>,
    timers: BTreeSet<(Timestamp, u64, Timer)>,
    ready: VecDeque<(ContextId, u32)>,
    effects: Vec<Effect>,
    eval: Evaluator,
    ids: Box<dyn IdGen>,
    next_call: u64,
    next_timer: u64,
    /// Calls are recorded as done instead of asked for (dry runs, backtests).
    dry: bool,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("flows", &self.flows.len())
            .field("runs", &self.runs.len())
            .field("dry", &self.dry)
            .finish_non_exhaustive()
    }
}

impl Engine {
    pub fn new(ids: Box<dyn IdGen>) -> Self {
        Self {
            flows: BTreeMap::new(),
            states: Arc::default(),
            runs: BTreeMap::new(),
            timers: BTreeSet::new(),
            ready: VecDeque::new(),
            effects: Vec::new(),
            eval: Evaluator::default(),
            ids,
            next_call: 0,
            next_timer: 0,
            dry: false,
        }
    }

    /// An engine whose calls are recorded instead of made.
    pub fn dry(ids: Box<dyn IdGen>) -> Self {
        Self {
            dry: true,
            ..Self::new(ids)
        }
    }

    /// Replaces what the engine knows of the home, without firing anything.
    pub fn load_states(&mut self, states: impl IntoIterator<Item = EntityState>) {
        self.states = Arc::new(
            states
                .into_iter()
                .map(|state| (state.entity_id.clone(), state))
                .collect(),
        );
    }

    pub fn state(&self, entity: &EntityId) -> Option<&EntityState> {
        self.states.get(entity)
    }

    /// Replaces what the engine knows of the home after it may have missed changes, and looks
    /// at every wait again against the fresh values. Triggers don't fire: a missed change can't
    /// be told apart from no change.
    pub fn resync(&mut self, states: impl IntoIterator<Item = EntityState>, now: Timestamp) {
        self.load_states(states);
        let waiting: Vec<(ContextId, u32)> = self
            .runs
            .iter()
            .flat_map(|(run_id, run)| {
                run.tokens
                    .iter()
                    .filter(|(_, token)| matches!(token.doing, Doing::Wait { .. }))
                    .map(move |(token_id, _)| (run_id.clone(), *token_id))
            })
            .collect();
        for (run, token) in waiting {
            self.check_wait(&run, token, now);
        }
        self.drive(now);
    }

    /// Everything the engine knows of the home.
    pub fn states(&self) -> Vec<EntityState> {
        self.states.values().cloned().collect()
    }

    /// The flows to run. A flow whose definition changed, that was turned off, or that now has
    /// problems has its runs aborted; one that's the same keeps them.
    pub fn set_flows(&mut self, arms: Vec<Arm>, now: Timestamp) {
        let mut next = BTreeMap::new();
        for Arm { flow, problems } in arms {
            let version = flow.version();
            let armed = if !flow.enabled {
                Armed::Disabled
            } else if let Some(problem) = problems.iter().find(|p| p.is_error()) {
                Armed::Unarmed {
                    reason: match &problem.node {
                        Some(node) => format!("{node}: {}", problem.message),
                        None => problem.message.clone(),
                    },
                }
            } else {
                Armed::Armed
            };
            let id = flow.id.clone();
            let kept = self.flows.remove(&id).filter(|old| {
                old.version == version && armed == Armed::Armed && old.armed == Armed::Armed
            });
            let (holds, queue) = match kept {
                Some(old) => (old.holds, old.queue),
                None => {
                    let reason = if armed == Armed::Disabled {
                        AbortReason::Disabled
                    } else {
                        AbortReason::Changed
                    };
                    self.abort_flow(&id, reason, now);
                    (BTreeMap::new(), VecDeque::new())
                }
            };
            next.insert(
                id,
                Loaded {
                    flow: Arc::new(flow),
                    version,
                    armed,
                    holds,
                    queue,
                },
            );
        }
        let removed: Vec<RuleId> = self.flows.keys().cloned().collect();
        for id in removed {
            self.abort_flow(&id, AbortReason::Removed, now);
        }
        self.flows = next;
    }

    /// Whether each flow is armed, and why not.
    pub fn armed(&self) -> BTreeMap<RuleId, (Armed, String)> {
        self.flows
            .iter()
            .map(|(id, loaded)| (id.clone(), (loaded.armed.clone(), loaded.version.clone())))
            .collect()
    }

    /// Fires every armed flow's `startup` triggers: the engine has just started.
    pub fn start(&mut self, now: Timestamp) {
        let startups: Vec<(RuleId, NodeId)> = self
            .flows
            .iter()
            .filter(|(_, loaded)| loaded.armed == Armed::Armed)
            .flat_map(|(id, loaded)| {
                loaded
                    .flow
                    .nodes
                    .iter()
                    .filter(|(_, node)| {
                        matches!(
                            node,
                            Node::Trigger {
                                trigger: Trigger::Startup {}
                            }
                        )
                    })
                    .map(move |(node, _)| (id.clone(), node.clone()))
            })
            .collect();
        for (flow, node) in startups {
            self.fire_now(
                &flow,
                Firing {
                    trigger: node,
                    cause: None,
                    test: None,
                    note: "Irori started".into(),
                    reads: Vec::new(),
                },
                now,
            );
        }
        self.drive(now);
    }

    /// Fires `trigger` by hand, as if it just happened. Answers the run's id, or why it can't.
    /// The flow's mode applies: a `single` flow already running refuses.
    pub fn fire(
        &mut self,
        flow: &RuleId,
        trigger: &NodeId,
        test: Option<TestKind>,
        now: Timestamp,
    ) -> Result<Option<ContextId>, String> {
        let loaded = self
            .flows
            .get(flow)
            .ok_or_else(|| format!("there's no flow `{flow}`"))?;
        if !matches!(loaded.flow.nodes.get(trigger), Some(Node::Trigger { .. })) {
            return Err(format!("`{trigger}` isn't a trigger of `{flow}`"));
        }
        match &loaded.armed {
            Armed::Armed => {}
            Armed::Disabled => return Err(format!("`{flow}` is turned off")),
            Armed::Unarmed { reason } => return Err(format!("`{flow}` can't run: {reason}")),
        }
        let started = self.fire_now(
            flow,
            Firing {
                trigger: trigger.clone(),
                cause: None,
                test,
                note: "fired by hand".into(),
                reads: Vec::new(),
            },
            now,
        );
        self.drive(now);
        Ok(started)
    }

    /// Cuts a run short. `false` if there's no such run going.
    pub fn cancel(&mut self, run: &ContextId, now: Timestamp) -> bool {
        let found = self.runs.contains_key(run);
        self.finish(
            run,
            Some(Outcome::Aborted {
                reason: AbortReason::Cancelled,
            }),
            now,
        );
        self.drive(now);
        found
    }

    /// Stops everything: the engine is going away.
    pub fn shutdown(&mut self, now: Timestamp) {
        let runs: Vec<ContextId> = self.runs.keys().cloned().collect();
        for run in runs {
            self.finish(
                &run,
                Some(Outcome::Aborted {
                    reason: AbortReason::Shutdown,
                }),
                now,
            );
        }
    }

    /// An entity changed somewhere in the home.
    pub fn state_changed(&mut self, old: Option<EntityState>, new: EntityState, now: Timestamp) {
        let entity = &new.entity_id.clone();
        Arc::make_mut(&mut self.states).insert(entity.clone(), new.clone());

        // Triggers watching it.
        let watching: Vec<(RuleId, NodeId, Trigger)> = self
            .flows
            .iter()
            .filter(|(_, loaded)| loaded.armed == Armed::Armed)
            .flat_map(|(id, loaded)| {
                loaded
                    .flow
                    .nodes
                    .iter()
                    .filter_map(move |(node, n)| match n {
                        Node::Trigger { trigger } if trigger_entity(trigger) == Some(entity) => {
                            Some((id.clone(), node.clone(), trigger.clone()))
                        }
                        _ => None,
                    })
            })
            .collect();
        for (flow, node, trigger) in watching {
            self.state_trigger(&flow, &node, &trigger, old.as_ref(), &new, now);
        }

        // Waits that read it last time they looked.
        let waiting: Vec<(ContextId, u32)> = self
            .runs
            .iter()
            .flat_map(|(run_id, run)| {
                run.tokens
                    .iter()
                    .filter_map(move |(token_id, token)| match &token.doing {
                        Doing::Wait { watch, .. } if watch.contains(entity) => {
                            Some((run_id.clone(), *token_id))
                        }
                        _ => None,
                    })
            })
            .collect();
        for (run, token) in waiting {
            self.check_wait(&run, token, now);
        }
        self.drive(now);
    }

    /// How a call the engine asked for went.
    pub fn call_finished(&mut self, call_id: u64, result: Result<(), String>, now: Timestamp) {
        let found = self.runs.iter().find_map(|(run_id, run)| {
            run.tokens.iter().find_map(|(token_id, token)| {
                (token.doing == Doing::Call { call_id }).then(|| (run_id.clone(), *token_id))
            })
        });
        let Some((run_id, token_id)) = found else {
            return;
        };
        self.answer_call(&run_id, token_id, result, now);
        self.drive(now);
    }

    /// Time has passed: whatever was due by `now` happens, in order.
    pub fn advance(&mut self, now: Timestamp) {
        while let Some((at, _, _)) = self.timers.first() {
            if *at > now {
                break;
            }
            let Some((at, _, timer)) = self.timers.pop_first() else {
                break;
            };
            self.timer(timer, at);
            self.drive(at);
        }
    }

    /// When something is next due, if anything is.
    pub fn next_deadline(&self) -> Option<Timestamp> {
        self.timers.first().map(|(at, _, _)| *at)
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// The runs in progress, optionally of one flow: their records so far, and where each path
    /// is.
    pub fn active(&self, flow: Option<&RuleId>) -> Vec<ActiveRun> {
        self.runs
            .values()
            .filter(|run| flow.is_none_or(|flow| &run.record.flow_id == flow))
            .map(|run| ActiveRun {
                record: run.record.clone(),
                at: run
                    .tokens
                    .values()
                    .map(|token| {
                        let (doing, since, until, holding_since) = match &token.doing {
                            Doing::Ready => ("ready", None, None, None),
                            Doing::Delay { until } => ("delaying", None, Some(*until), None),
                            Doing::Wait {
                                since,
                                timeout,
                                holding_since,
                                ..
                            } => ("waiting", Some(*since), Some(*timeout), *holding_since),
                            Doing::Call { .. } => ("calling", None, None, None),
                            Doing::Joined => (
                                "joining",
                                None,
                                run.joins.get(&token.node).and_then(|join| join.deadline),
                                None,
                            ),
                        };
                        TokenAt {
                            node: token.node.clone(),
                            doing: doing.to_owned(),
                            since,
                            until,
                            holding_since,
                        }
                    })
                    .collect(),
            })
            .collect()
    }

    /// The triggers of `flow` holding a `for`: each one, and when it fires if it stays.
    pub fn holding(&self, flow: &RuleId) -> Vec<(NodeId, Timestamp)> {
        self.flows
            .get(flow)
            .map(|loaded| {
                loaded
                    .holds
                    .iter()
                    .map(|(node, until)| (node.clone(), *until))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// How many runs of `flow` are going.
    pub fn running(&self, flow: &RuleId) -> usize {
        self.runs
            .values()
            .filter(|run| &run.record.flow_id == flow)
            .count()
    }

    // --- Triggers --------------------------------------------------------------------------

    fn state_trigger(
        &mut self,
        flow: &RuleId,
        node: &NodeId,
        trigger: &Trigger,
        old: Option<&EntityState>,
        new: &EntityState,
        now: Timestamp,
    ) {
        let Trigger::State {
            entity,
            from,
            to,
            above,
            below,
            hold,
        } = trigger
        else {
            return;
        };
        let level = (above.is_some() || below.is_some()).then_some((*above, *below));
        let read = to_read(eval::Read::of(entity, &self.states));
        let old_value = old
            .and_then(|state| state.state.as_ref())
            .map(eval::value_of)
            .unwrap_or(serde_json::Value::Null);
        let new_value = read.value.clone();
        let was_available = old.is_some_and(|state| state.availability == Availability::Available);

        if new.availability == Availability::Unavailable || new.state.is_none() {
            let had_hold = self.cancel_hold(flow, node);
            if was_available || had_hold {
                let what = if new.availability == Availability::Unavailable {
                    "became unavailable"
                } else {
                    "doesn't know its value"
                };
                self.near_miss(
                    flow,
                    node,
                    NearMissKind::Unavailable,
                    format!("{entity} {what}, so this trigger can't fire until it's back"),
                    vec![read],
                    now,
                );
            }
            return;
        }
        // Something that happens rather than a value that lasts (a button's press): each one
        // counts, the same one twice included. The core says when it happened by moving
        // `last_changed`, and leaves it alone for what isn't one — the device coming back in
        // reach, or the protocol repeating its last press as it reconnects.
        let happening = entity.kind().counts_every_report();
        if happening && old.is_none_or(|old| old.last_changed == new.last_changed) {
            return;
        }
        if !happening && old.is_some() && old_value == new_value {
            if was_available && to.as_ref().is_some_and(|to| any_matches(to, &new_value)) {
                self.near_miss(
                    flow,
                    node,
                    NearMissKind::SameValue,
                    format!(
                        "{entity} reported {} again, but it already was, so a change to {} \
                         didn't happen",
                        words(&new_value),
                        words(&new_value)
                    ),
                    vec![read],
                    now,
                );
            }
            return;
        }
        let note = if happening {
            format!("{entity}: {}", words(&new_value))
        } else {
            format!(
                "{entity} went {} → {}",
                words(&old_value),
                words(&new_value)
            )
        };
        let (from_ok, to_ok) = match level {
            // A level: it has to come into the range from outside it (or from not knowing).
            // Moving about inside the range changes nothing, so a hold keeps going.
            Some(range) => {
                let inside = |value: &serde_json::Value| {
                    value.as_f64().is_some_and(|n| {
                        range.0.is_none_or(|above| n > above)
                            && range.1.is_none_or(|below| n < below)
                    })
                };
                if inside(&new_value) && inside(&old_value) && old.is_some() {
                    return;
                }
                (!inside(&old_value) || old.is_none(), inside(&new_value))
            }
            None => (
                from.as_ref()
                    .is_none_or(|from| any_matches(from, &old_value)),
                to.as_ref().is_none_or(|to| any_matches(to, &new_value)),
            ),
        };
        if from_ok && to_ok {
            match hold {
                Some(hold) => {
                    let until = plus(now, hold.millis());
                    if let Some(loaded) = self.flows.get_mut(flow) {
                        loaded.holds.insert(node.clone(), until);
                    }
                    self.schedule(
                        until,
                        Timer::Hold {
                            flow: flow.clone(),
                            node: node.clone(),
                        },
                    );
                }
                None => {
                    self.fire_now(
                        flow,
                        Firing {
                            trigger: node.clone(),
                            cause: Some(new.context.id.clone()),
                            test: None,
                            note,
                            reads: vec![read],
                        },
                        now,
                    );
                }
            }
        } else if level.is_none()
            && to_ok
            && !from
                .as_ref()
                .is_some_and(|from| to.is_none() && any_matches(from, &new_value))
        {
            // It didn't come from where the trigger says, but it's still where the trigger
            // wants it (paused → idle, after playing → paused): a hold keeps going.
        } else if self.cancel_hold(flow, node) {
            let hold = hold
                .as_ref()
                .map(|hold| hold.as_str().to_owned())
                .unwrap_or_default();
            self.near_miss(
                flow,
                node,
                NearMissKind::HoldReset,
                format!("{note} before the {hold} it had to hold for was up"),
                vec![read],
                now,
            );
        }
    }

    fn cancel_hold(&mut self, flow: &RuleId, node: &NodeId) -> bool {
        self.flows
            .get_mut(flow)
            .is_some_and(|loaded| loaded.holds.remove(node).is_some())
    }

    fn near_miss(
        &mut self,
        flow: &RuleId,
        node: &NodeId,
        kind: NearMissKind,
        message: String,
        reads: Vec<Read>,
        now: Timestamp,
    ) {
        let version = self
            .flows
            .get(flow)
            .map(|loaded| loaded.version.clone())
            .unwrap_or_default();
        self.effects.push(Effect::NearMiss(NearMiss {
            at: now,
            flow_id: flow.clone(),
            version,
            node: node.clone(),
            kind,
            message,
            reads,
        }));
    }

    /// A trigger fired: the mode decides whether it becomes a run. Answers the run's id if one
    /// started now.
    fn fire_now(&mut self, flow: &RuleId, firing: Firing, now: Timestamp) -> Option<ContextId> {
        let loaded = self.flows.get(flow)?;
        let mode = loaded.flow.mode.clone();
        let running: Vec<ContextId> = self
            .runs
            .iter()
            .filter(|(_, run)| &run.record.flow_id == flow)
            .map(|(id, _)| id.clone())
            .collect();
        let refuse = |why: &str| {
            format!(
                "{}, but {why}",
                if firing.note.is_empty() {
                    format!("{} fired", firing.trigger)
                } else {
                    firing.note.clone()
                }
            )
        };
        match mode {
            Mode::Named(NamedMode::Single) if !running.is_empty() => {
                let message = refuse("a run was already going and the mode is single");
                self.near_miss(
                    flow,
                    &firing.trigger,
                    NearMissKind::Dropped,
                    message,
                    firing.reads,
                    now,
                );
                None
            }
            Mode::Named(NamedMode::Restart) => {
                for run in running {
                    self.finish(&run, Some(Outcome::Superseded), now);
                }
                Some(self.start_run(flow, firing, now))
            }
            Mode::Limited(LimitedMode::Queued { max }) if !running.is_empty() => {
                let loaded = self.flows.get_mut(flow)?;
                if loaded.queue.len() < usize::from(max) {
                    loaded.queue.push_back(firing);
                } else {
                    let message = refuse(&format!("{max} runs were already waiting their turn"));
                    self.near_miss(
                        flow,
                        &firing.trigger,
                        NearMissKind::Dropped,
                        message,
                        firing.reads,
                        now,
                    );
                }
                None
            }
            Mode::Limited(LimitedMode::Parallel { max }) if running.len() >= usize::from(max) => {
                let message = refuse(&format!("{max} runs were already going at once"));
                self.near_miss(
                    flow,
                    &firing.trigger,
                    NearMissKind::Dropped,
                    message,
                    firing.reads,
                    now,
                );
                None
            }
            _ => Some(self.start_run(flow, firing, now)),
        }
    }

    fn start_run(&mut self, flow_id: &RuleId, firing: Firing, now: Timestamp) -> ContextId {
        let run_id = self.ids.run_id(now);
        let Some(loaded) = self.flows.get(flow_id) else {
            return run_id;
        };
        let flow = Arc::clone(&loaded.flow);
        let record = RunRecord {
            run_id: run_id.clone(),
            flow_id: flow_id.clone(),
            version: loaded.version.clone(),
            trigger: firing.trigger.clone(),
            cause: firing.cause,
            test: firing.test,
            started_at: now,
            finished_at: None,
            outcome: None,
            ended_at: Vec::new(),
            steps: vec![Step {
                seq: 0,
                node: firing.trigger.clone(),
                token: 0,
                via: None,
                at: now,
                finished_at: Some(now),
                port: Some(Port::Out),
                reads: firing.reads,
                note: Some(firing.note),
                call: None,
            }],
            assumptions: Vec::new(),
        };
        self.runs.insert(
            run_id.clone(),
            Run {
                flow,
                record,
                tokens: BTreeMap::new(),
                next_token: 1,
                vars: BTreeMap::new(),
                joins: BTreeMap::new(),
                error: None,
                stopped: None,
            },
        );
        self.leave(&run_id, &firing.trigger, Port::Out);
        run_id
    }

    // --- Running ---------------------------------------------------------------------------

    /// A token leaves `node` by `port`: one new token per wire, or the path ends there.
    fn leave(&mut self, run_id: &ContextId, node: &NodeId, port: Port) {
        let Some(run) = self.runs.get_mut(run_id) else {
            return;
        };
        let wires: Vec<Wire> = run.flow.wires_from(node, &port).cloned().collect();
        if wires.is_empty() {
            run.record.ended_at.push(Ending {
                node: node.clone(),
                port: Some(port),
            });
            return;
        }
        for wire in wires {
            let id = run.next_token;
            run.next_token += 1;
            run.tokens.insert(
                id,
                Token {
                    node: wire.to.clone(),
                    via: Some(wire),
                    step: None,
                    doing: Doing::Ready,
                },
            );
            self.ready.push_back((run_id.clone(), id));
        }
    }

    /// Runs every token that's ready, then ends the runs that have nothing left.
    fn drive(&mut self, now: Timestamp) {
        while let Some((run_id, token_id)) = self.ready.pop_front() {
            self.step(&run_id, token_id, now);
        }
        let idle: Vec<ContextId> = self
            .runs
            .iter()
            .filter(|(_, run)| {
                run.tokens
                    .values()
                    .all(|token| token.doing == Doing::Joined)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for run_id in idle {
            let parked = self
                .runs
                .get(&run_id)
                .is_some_and(|run| !run.tokens.is_empty());
            if parked {
                self.give_up_joins(&run_id, now);
                // Giving up may have been the end of it: a `timeout` with nothing wired.
                let emptied = self
                    .runs
                    .get(&run_id)
                    .is_some_and(|run| run.tokens.is_empty());
                if emptied && self.ready.is_empty() {
                    self.finish(&run_id, None, now);
                }
            } else {
                self.finish(&run_id, None, now);
            }
        }
        if !self.ready.is_empty() {
            self.drive(now);
        }
    }

    fn snapshot(&self, run_id: &ContextId, now: Timestamp) -> Snapshot {
        Snapshot {
            states: Arc::clone(&self.states),
            vars: self
                .runs
                .get(run_id)
                .map(|run| run.vars.clone())
                .unwrap_or_default(),
            now,
        }
    }

    /// Opens a step for a token arriving at its node.
    fn open_step(&mut self, run_id: &ContextId, token_id: u32, now: Timestamp) -> Option<usize> {
        let run = self.runs.get_mut(run_id)?;
        let seq = u32::try_from(run.record.steps.len()).unwrap_or(u32::MAX);
        let token = run.tokens.get_mut(&token_id)?;
        run.record.steps.push(Step {
            seq,
            node: token.node.clone(),
            token: token_id,
            via: token.via.clone(),
            at: now,
            finished_at: None,
            port: None,
            reads: Vec::new(),
            note: None,
            call: None,
        });
        let index = run.record.steps.len() - 1;
        token.step = Some(index);
        Some(index)
    }

    fn with_step(&mut self, run_id: &ContextId, token_id: u32, f: impl FnOnce(&mut Step)) {
        if let Some(run) = self.runs.get_mut(run_id)
            && let Some(index) = run.tokens.get(&token_id).and_then(|token| token.step)
            && let Some(step) = run.record.steps.get_mut(index)
        {
            f(step);
        }
    }

    /// The token's step is done and it leaves by `port`.
    fn pass(&mut self, run_id: &ContextId, token_id: u32, port: Port, now: Timestamp) {
        self.with_step(run_id, token_id, |step| {
            step.finished_at = Some(now);
            step.port = Some(port);
        });
        let Some(token) = self
            .runs
            .get_mut(run_id)
            .and_then(|run| run.tokens.remove(&token_id))
        else {
            return;
        };
        self.leave(run_id, &token.node, port);
    }

    /// The token's path ends inside its node.
    fn end_token(&mut self, run_id: &ContextId, token_id: u32, now: Timestamp) {
        self.with_step(run_id, token_id, |step| step.finished_at = Some(now));
        if let Some(run) = self.runs.get_mut(run_id)
            && let Some(token) = run.tokens.remove(&token_id)
        {
            run.record.ended_at.push(Ending {
                node: token.node,
                port: None,
            });
        }
    }

    fn step(&mut self, run_id: &ContextId, token_id: u32, now: Timestamp) {
        let Some(run) = self.runs.get(run_id) else {
            return;
        };
        let Some(token) = run.tokens.get(&token_id) else {
            return;
        };
        if token.doing != Doing::Ready {
            return;
        }
        let Some(node) = run.flow.nodes.get(&token.node).cloned() else {
            self.end_token(run_id, token_id, now);
            return;
        };
        let node_id = token.node.clone();
        if run.record.steps.len() >= MAX_STEPS as usize {
            if let Some(run) = self.runs.get_mut(run_id) {
                run.error = Some((
                    node_id,
                    format!("the run took more than {MAX_STEPS} steps; paths multiply too much"),
                ));
            }
            let tokens: Vec<u32> = self
                .runs
                .get(run_id)
                .map(|run| run.tokens.keys().copied().collect())
                .unwrap_or_default();
            for token in tokens {
                self.end_token(run_id, token, now);
            }
            return;
        }
        self.open_step(run_id, token_id, now);
        match node {
            Node::Trigger { .. } => self.pass(run_id, token_id, Port::Out, now),
            Node::Gate { condition } => {
                let snapshot = self.snapshot(run_id, now);
                let outcome = self.eval.condition(&condition, &snapshot);
                let holds = outcome.result == Ok(true);
                let what = self.marked(&condition, &snapshot);
                let note = match &outcome.result {
                    Ok(holds) => format!("{what} → {}", yes_no(*holds)),
                    Err(error) => format!("{what} → no: {error}"),
                };
                self.with_step(run_id, token_id, |step| {
                    step.reads = reads(outcome.reads);
                    step.note = Some(note);
                });
                self.pass(
                    run_id,
                    token_id,
                    if holds { Port::Yes } else { Port::No },
                    now,
                );
            }
            Node::Switch { cases } => {
                let snapshot = self.snapshot(run_id, now);
                let mut all_reads = Vec::new();
                let mut chosen = Port::Else;
                let mut notes = Vec::new();
                for (i, case) in cases.iter().enumerate() {
                    let outcome = self.eval.condition(case, &snapshot);
                    let what = self.marked(case, &snapshot);
                    for read in outcome.reads {
                        if !all_reads
                            .iter()
                            .any(|r: &eval::Read| r.entity_id == read.entity_id)
                        {
                            all_reads.push(read);
                        }
                    }
                    match outcome.result {
                        Ok(true) => {
                            chosen = Port::Case(u8::try_from(i + 1).unwrap_or(u8::MAX));
                            notes.push(format!("case {}: {what} → yes", i + 1));
                            break;
                        }
                        Ok(false) => notes.push(format!("case {}: {what} → no", i + 1)),
                        Err(error) => notes.push(format!("case {}: {error}", i + 1)),
                    }
                }
                self.with_step(run_id, token_id, |step| {
                    step.reads = reads(all_reads);
                    step.note = Some(notes.join("; "));
                });
                self.pass(run_id, token_id, chosen, now);
            }
            Node::Call {
                service,
                entity,
                data,
            } => {
                let call_id = self.next_call;
                self.next_call += 1;
                let dry = self.dry;
                // Settings that are worked out are worked out now, from the home as it is.
                let mut read = Vec::new();
                let resolved = match &data {
                    None => Ok((None, Vec::new())),
                    Some(data) => {
                        let snapshot = self.snapshot(run_id, now);
                        data.resolve(service, |expr| {
                            let outcome = self.eval.value(expr.as_str(), &snapshot);
                            read.extend(outcome.reads);
                            match outcome.result? {
                                serde_json::Value::Number(n) => n
                                    .as_f64()
                                    .ok_or_else(|| format!("`{}` isn't a number", expr.as_str())),
                                other => {
                                    Err(format!("`{}` gave {other}, not a number", expr.as_str()))
                                }
                            }
                        })
                        .map(|(data, notes)| (Some(data), notes))
                    }
                };
                let (data, worked) = match resolved {
                    Ok(resolved) => resolved,
                    Err(error) => {
                        self.with_step(run_id, token_id, |step| {
                            step.reads = reads(read);
                            step.note = Some(format!("couldn't work out the settings: {error}"));
                            step.call = Some(CallDetail {
                                service: service.to_string(),
                                entity_id: entity.clone(),
                                data: None,
                                result: None,
                                simulated: dry,
                            });
                        });
                        self.answer_call(run_id, token_id, Err(error), now);
                        return;
                    }
                };
                self.with_step(run_id, token_id, |step| {
                    step.reads = reads(read);
                    step.call = Some(CallDetail {
                        service: service.to_string(),
                        entity_id: entity.clone(),
                        data: data
                            .as_ref()
                            .and_then(|data| serde_json::to_value(data).ok()),
                        result: None,
                        simulated: dry,
                    });
                    step.note = Some(if worked.is_empty() {
                        format!("{service} {entity}")
                    } else {
                        format!("{service} {entity} ({})", worked.join(", "))
                    });
                });
                if let Some(token) = self
                    .runs
                    .get_mut(run_id)
                    .and_then(|run| run.tokens.get_mut(&token_id))
                {
                    token.doing = Doing::Call { call_id };
                }
                if dry {
                    self.answer_call(run_id, token_id, Ok(()), now);
                } else {
                    let parent_id = self
                        .runs
                        .get(run_id)
                        .and_then(|run| run.record.cause.clone());
                    self.effects.push(Effect::Call {
                        call_id,
                        run_id: run_id.clone(),
                        parent_id,
                        entity,
                        service,
                        data,
                    });
                }
            }
            Node::Set { name, expr } => {
                let snapshot = self.snapshot(run_id, now);
                let outcome = self.eval.value(expr.as_str(), &snapshot);
                match outcome.result {
                    Ok(value) => {
                        let note = format!("{name} = {}", words(&value));
                        if let Some(run) = self.runs.get_mut(run_id) {
                            run.vars.insert(name.as_str().to_owned(), value);
                        }
                        self.with_step(run_id, token_id, |step| {
                            step.reads = reads(outcome.reads);
                            step.note = Some(note);
                        });
                        self.pass(run_id, token_id, Port::Out, now);
                    }
                    Err(error) => {
                        self.with_step(run_id, token_id, |step| {
                            step.reads = reads(outcome.reads);
                            step.note = Some(format!("couldn't work out {name}: {error}"));
                        });
                        if let Some(run) = self.runs.get_mut(run_id) {
                            run.error.get_or_insert((node_id, error));
                        }
                        self.end_token(run_id, token_id, now);
                    }
                }
            }
            Node::Delay { hold } => {
                let until = plus(now, hold.millis());
                if let Some(token) = self
                    .runs
                    .get_mut(run_id)
                    .and_then(|run| run.tokens.get_mut(&token_id))
                {
                    token.doing = Doing::Delay { until };
                }
                self.with_step(run_id, token_id, |step| {
                    step.note = Some(format!("waiting {}", hold.as_str()));
                });
                self.schedule(
                    until,
                    Timer::Token {
                        run: run_id.clone(),
                        token: token_id,
                    },
                );
            }
            Node::Wait { timeout, .. } => {
                let deadline = plus(now, timeout.millis());
                if let Some(token) = self
                    .runs
                    .get_mut(run_id)
                    .and_then(|run| run.tokens.get_mut(&token_id))
                {
                    token.doing = Doing::Wait {
                        since: now,
                        timeout: deadline,
                        holding_since: None,
                        watch: BTreeSet::new(),
                    };
                }
                self.schedule(
                    deadline,
                    Timer::Token {
                        run: run_id.clone(),
                        token: token_id,
                    },
                );
                self.check_wait(run_id, token_id, now);
            }
            Node::Join { mode, timeout } => self.arrive(
                run_id,
                token_id,
                &node_id,
                mode,
                timeout.map(|t| t.millis()),
                now,
            ),
            Node::Stop { reason } => {
                let reason = reason.map(|reason| reason.as_str().to_owned());
                self.with_step(run_id, token_id, |step| {
                    step.finished_at = Some(now);
                    step.note = Some(reason.clone().unwrap_or_else(|| "stop".into()));
                });
                if let Some(run) = self.runs.get_mut(run_id) {
                    run.stopped = Some((node_id.clone(), reason));
                    run.record.ended_at.push(Ending {
                        node: node_id,
                        port: None,
                    });
                }
                // The whole run ends: every other path with it.
                let others: Vec<u32> = self
                    .runs
                    .get(run_id)
                    .map(|run| run.tokens.keys().copied().collect())
                    .unwrap_or_default();
                for token in others {
                    if token != token_id {
                        self.with_step(run_id, token, |step| {
                            step.finished_at = Some(now);
                            step.note
                                .get_or_insert_with(|| "cut short by a stop".into());
                        });
                    }
                    if let Some(run) = self.runs.get_mut(run_id) {
                        run.tokens.remove(&token);
                    }
                }
            }
        }
    }

    /// A condition as a trace note. Several checks together are each marked with how they
    /// came out — "… is on ✗ and … is off ✓" — so a `no` says which one it was.
    fn marked(&mut self, condition: &Condition, snapshot: &Snapshot) -> String {
        let (children, joiner) = match condition {
            Condition::All { conditions } => (conditions, " and "),
            Condition::Any { conditions } => (conditions, " or "),
            _ => return describe(condition),
        };
        children
            .iter()
            .map(|child| {
                let mark = match self.eval.condition(child, snapshot).result {
                    Ok(true) => "✓".to_owned(),
                    Ok(false) => "✗".to_owned(),
                    Err(error) => format!("✗ ({error})"),
                };
                format!("{} {mark}", describe(child))
            })
            .collect::<Vec<_>>()
            .join(joiner)
    }

    fn answer_call(
        &mut self,
        run_id: &ContextId,
        token_id: u32,
        result: Result<(), String>,
        now: Timestamp,
    ) {
        self.with_step(run_id, token_id, |step| {
            if let Some(call) = &mut step.call {
                call.result = Some(result.clone());
            }
        });
        match result {
            Ok(()) => self.pass(run_id, token_id, Port::Out, now),
            Err(error) => {
                let Some(run) = self.runs.get(run_id) else {
                    return;
                };
                let Some(node) = run.tokens.get(&token_id).map(|token| token.node.clone()) else {
                    return;
                };
                if run.flow.wires_from(&node, &Port::Error).next().is_some() {
                    self.pass(run_id, token_id, Port::Error, now);
                } else {
                    self.with_step(run_id, token_id, |step| {
                        step.finished_at = Some(now);
                        step.port = Some(Port::Error);
                    });
                    if let Some(run) = self.runs.get_mut(run_id) {
                        run.tokens.remove(&token_id);
                        run.record.ended_at.push(Ending {
                            node: node.clone(),
                            port: Some(Port::Error),
                        });
                        run.error.get_or_insert((node, error));
                    }
                }
            }
        }
    }

    /// Looks at a waiting token's level again: matched (after its `for`), still waiting, or
    /// holding.
    fn check_wait(&mut self, run_id: &ContextId, token_id: u32, now: Timestamp) {
        let Some(run) = self.runs.get(run_id) else {
            return;
        };
        let Some(token) = run.tokens.get(&token_id) else {
            return;
        };
        let Some(Node::Wait { until, .. }) = run.flow.nodes.get(&token.node).cloned() else {
            return;
        };
        let Doing::Wait {
            since,
            timeout,
            holding_since,
            ..
        } = token.doing.clone()
        else {
            return;
        };
        let snapshot = self.snapshot(run_id, now);
        let outcome = self.eval.wait(&until, &snapshot);
        let mut watch: BTreeSet<EntityId> = outcome
            .reads
            .iter()
            .map(|read| read.entity_id.clone())
            .collect();
        if let WaitUntil::State { entity, .. } = &until {
            watch.insert(entity.clone());
        }
        let holds = outcome.result == Ok(true);
        let hold_ms = match &until {
            WaitUntil::State { hold, .. } | WaitUntil::Expr { hold, .. } => {
                hold.as_ref().map(|hold| hold.millis())
            }
        };
        let described = describe_wait(&until);
        self.with_step(run_id, token_id, |step| {
            step.reads = reads(outcome.reads.clone());
        });
        match (holds, hold_ms) {
            (true, None) => {
                self.with_step(run_id, token_id, |step| {
                    step.note = Some(format!("{described} → matched"));
                });
                self.pass(run_id, token_id, Port::Matched, now);
            }
            (true, Some(ms)) => {
                let held_from = holding_since.unwrap_or(now);
                let done = plus(held_from, ms);
                if done <= now {
                    self.with_step(run_id, token_id, |step| {
                        step.note = Some(format!("{described} → matched"));
                    });
                    self.pass(run_id, token_id, Port::Matched, now);
                    return;
                }
                if let Some(token) = self
                    .runs
                    .get_mut(run_id)
                    .and_then(|run| run.tokens.get_mut(&token_id))
                {
                    token.doing = Doing::Wait {
                        since,
                        timeout,
                        holding_since: Some(held_from),
                        watch,
                    };
                }
                self.with_step(run_id, token_id, |step| {
                    step.note = Some(format!("{described}: holding"));
                });
                if holding_since.is_none() {
                    self.schedule(
                        done,
                        Timer::Token {
                            run: run_id.clone(),
                            token: token_id,
                        },
                    );
                }
            }
            (false, _) => {
                if let Some(token) = self
                    .runs
                    .get_mut(run_id)
                    .and_then(|run| run.tokens.get_mut(&token_id))
                {
                    token.doing = Doing::Wait {
                        since,
                        timeout,
                        holding_since: None,
                        watch,
                    };
                }
                let why = match &outcome.result {
                    Err(error) => format!(": {error}"),
                    _ => String::new(),
                };
                self.with_step(run_id, token_id, |step| {
                    step.note = Some(format!("{described}: waiting{why}"));
                });
            }
        }
    }

    fn arrive(
        &mut self,
        run_id: &ContextId,
        token_id: u32,
        node: &NodeId,
        mode: JoinMode,
        timeout_ms: Option<i64>,
        now: Timestamp,
    ) {
        let Some(run) = self.runs.get_mut(run_id) else {
            return;
        };
        let incoming: Vec<Wire> = run.flow.wires_into(node).cloned().collect();
        let via = run
            .tokens
            .get(&token_id)
            .and_then(|token| token.via.clone());
        let join = run.joins.entry(node.clone()).or_default();
        match mode {
            JoinMode::First => {
                if join.passed {
                    self.with_step(run_id, token_id, |step| {
                        step.note = Some("another path got here first".into());
                    });
                    self.end_token(run_id, token_id, now);
                } else {
                    join.passed = true;
                    self.with_step(run_id, token_id, |step| {
                        step.note = Some("first here: goes on".into());
                    });
                    self.pass(run_id, token_id, Port::Out, now);
                }
            }
            JoinMode::All if join.passed => {
                // It already went on (everyone arrived, it timed out, or it gave up): a path
                // arriving now ends here, rather than sending the run on a second time.
                self.with_step(run_id, token_id, |step| {
                    step.note = Some("arrived after the join had already gone on".into());
                });
                self.end_token(run_id, token_id, now);
            }
            JoinMode::All => {
                let first = join.arrived.is_empty();
                if let Some(via) = via {
                    join.arrived.insert(via, token_id);
                }
                let missing = incoming
                    .iter()
                    .filter(|wire| !join.arrived.contains_key(*wire))
                    .count();
                if first && let Some(ms) = timeout_ms {
                    let deadline = plus(now, ms);
                    join.deadline = Some(deadline);
                    self.schedule(
                        deadline,
                        Timer::Join {
                            run: run_id.clone(),
                            node: node.clone(),
                        },
                    );
                }
                if missing == 0 {
                    self.complete_join(
                        run_id,
                        token_id,
                        node,
                        Port::Out,
                        "every path is here".into(),
                        now,
                    );
                } else {
                    if let Some(token) = self
                        .runs
                        .get_mut(run_id)
                        .and_then(|run| run.tokens.get_mut(&token_id))
                    {
                        token.doing = Doing::Joined;
                    }
                    self.with_step(run_id, token_id, |step| {
                        step.note = Some(format!(
                            "waiting for {missing} more path{}",
                            if missing == 1 { "" } else { "s" }
                        ));
                    });
                }
            }
        }
    }

    /// A join lets one token through by `port`; the others parked there end.
    fn complete_join(
        &mut self,
        run_id: &ContextId,
        through: u32,
        node: &NodeId,
        port: Port,
        note: String,
        now: Timestamp,
    ) {
        let parked: Vec<u32> = self
            .runs
            .get_mut(run_id)
            .and_then(|run| run.joins.get_mut(node))
            .map(|join| {
                join.passed = true;
                join.deadline = None;
                join.arrived.values().copied().collect()
            })
            .unwrap_or_default();
        for token in parked {
            if token != through {
                self.with_step(run_id, token, |step| {
                    step.finished_at = Some(now);
                    step.note = Some("merged".into());
                });
                if let Some(run) = self.runs.get_mut(run_id) {
                    run.tokens.remove(&token);
                }
            }
        }
        if let Some(token) = self
            .runs
            .get_mut(run_id)
            .and_then(|run| run.tokens.get_mut(&through))
        {
            token.doing = Doing::Ready;
        }
        self.with_step(run_id, through, |step| step.note = Some(note));
        self.pass(run_id, through, port, now);
    }

    /// Every path left is parked at a join that will never complete: the paths it waits for
    /// ended elsewhere. Each gives up through its `timeout` now, saying which didn't come.
    fn give_up_joins(&mut self, run_id: &ContextId, now: Timestamp) {
        let Some(run) = self.runs.get(run_id) else {
            return;
        };
        let stuck: Vec<(NodeId, u32, Vec<String>)> = run
            .joins
            .iter()
            .filter(|(_, join)| !join.passed && !join.arrived.is_empty())
            .filter_map(|(node, join)| {
                let through = *join.arrived.values().next_back()?;
                let missing = run
                    .flow
                    .wires_into(node)
                    .filter(|wire| !join.arrived.contains_key(*wire))
                    .map(|wire| wire.from.node.to_string())
                    .collect();
                Some((node.clone(), through, missing))
            })
            .collect();
        for (node, through, missing) in stuck {
            let note = format!(
                "gave up: the path from {} ended before getting here",
                missing.join(", ")
            );
            self.complete_join(run_id, through, &node, Port::Timeout, note, now);
        }
    }

    fn schedule(&mut self, at: Timestamp, timer: Timer) {
        self.next_timer += 1;
        self.timers.insert((at, self.next_timer, timer));
    }

    fn timer(&mut self, timer: Timer, now: Timestamp) {
        match timer {
            Timer::Hold { flow, node } => {
                let due = self
                    .flows
                    .get(&flow)
                    .and_then(|loaded| loaded.holds.get(&node))
                    .is_some_and(|until| *until <= now);
                if !due {
                    return;
                }
                self.cancel_hold(&flow, &node);
                let Some(Node::Trigger {
                    trigger: Trigger::State { entity, hold, .. },
                }) = self
                    .flows
                    .get(&flow)
                    .and_then(|loaded| loaded.flow.nodes.get(&node))
                    .cloned()
                else {
                    return;
                };
                let read = to_read(eval::Read::of(&entity, &self.states));
                let hold = hold
                    .map(|hold| hold.as_str().to_owned())
                    .unwrap_or_default();
                let cause = self
                    .states
                    .get(&entity)
                    .map(|state| state.context.id.clone());
                self.fire_now(
                    &flow,
                    Firing {
                        trigger: node,
                        cause,
                        test: None,
                        note: format!("{entity} held {} for {hold}", words(&read.value)),
                        reads: vec![read],
                    },
                    now,
                );
            }
            Timer::Token { run, token } => {
                let doing = self
                    .runs
                    .get(&run)
                    .and_then(|r| r.tokens.get(&token))
                    .map(|t| t.doing.clone());
                match doing {
                    Some(Doing::Delay { until }) if until <= now => {
                        self.pass(&run, token, Port::Out, now);
                    }
                    Some(Doing::Wait { timeout, .. }) if timeout <= now => {
                        let Some(node) = self
                            .runs
                            .get(&run)
                            .and_then(|r| r.tokens.get(&token))
                            .map(|t| t.node.clone())
                        else {
                            return;
                        };
                        let timeout_text =
                            match self.runs.get(&run).and_then(|r| r.flow.nodes.get(&node)) {
                                Some(Node::Wait { timeout, .. }) => timeout.as_str().to_owned(),
                                _ => String::new(),
                            };
                        self.with_step(&run, token, |step| {
                            step.note = Some(format!("gave up waiting after {timeout_text}"));
                        });
                        self.pass(&run, token, Port::Timeout, now);
                    }
                    Some(Doing::Wait { .. }) => self.check_wait(&run, token, now),
                    _ => {}
                }
            }
            Timer::Join { run, node } => {
                let pending = self
                    .runs
                    .get(&run)
                    .and_then(|r| r.joins.get(&node))
                    .filter(|join| !join.passed && join.deadline.is_some_and(|d| d <= now))
                    .and_then(|join| join.arrived.values().next_back().copied());
                if let Some(through) = pending {
                    self.complete_join(
                        &run,
                        through,
                        &node,
                        Port::Timeout,
                        "timed out waiting for the other paths".into(),
                        now,
                    );
                }
            }
        }
    }

    /// Ends a run: its outcome is `outcome` if given, else what its paths came to.
    fn finish(&mut self, run_id: &ContextId, outcome: Option<Outcome>, now: Timestamp) {
        let Some(mut run) = self.runs.remove(run_id) else {
            return;
        };
        let cut_short = outcome.is_some();
        let outcome = outcome.unwrap_or_else(|| match (&run.stopped, &run.error) {
            (Some((node, reason)), _) => Outcome::Stopped {
                node: node.clone(),
                reason: reason.clone(),
            },
            (None, Some((node, message))) => Outcome::Error {
                node: node.clone(),
                message: message.clone(),
            },
            (None, None) => Outcome::Completed,
        });
        if cut_short {
            for step in &mut run.record.steps {
                if step.finished_at.is_none() {
                    step.finished_at = Some(now);
                    step.note = Some(match &step.note {
                        Some(note) => format!("{note} (cut short)"),
                        None => "cut short".into(),
                    });
                }
            }
        }
        run.record.finished_at = Some(now);
        run.record.outcome = Some(outcome);
        self.ready.retain(|(id, _)| id != run_id);
        let flow = run.record.flow_id.clone();
        let outcome_now = run.record.outcome.clone();
        self.effects.push(Effect::Finished(Box::new(run.record)));
        // A queued firing gets its turn — a cancelled run included — unless the flow itself was
        // turned off, changed or removed, or the engine is stopping (`abort_flow` has cleared
        // the queue for those).
        let flow_going = matches!(
            outcome_now,
            Some(Outcome::Aborted { reason })
                if reason != AbortReason::Cancelled
        );
        if !flow_going
            && let Some(next) = self
                .flows
                .get_mut(&flow)
                .and_then(|loaded| loaded.queue.pop_front())
        {
            self.start_run(&flow, next, now);
        }
    }

    fn abort_flow(&mut self, flow: &RuleId, reason: AbortReason, now: Timestamp) {
        let runs: Vec<ContextId> = self
            .runs
            .iter()
            .filter(|(_, run)| &run.record.flow_id == flow)
            .map(|(id, _)| id.clone())
            .collect();
        for run in runs {
            self.finish(&run, Some(Outcome::Aborted { reason }), now);
        }
        if let Some(loaded) = self.flows.get_mut(flow) {
            loaded.queue.clear();
            loaded.holds.clear();
        }
    }
}

/// Whether `value` is one of `wanted`.
fn any_matches(wanted: &Values, value: &serde_json::Value) -> bool {
    wanted.iter().any(|want| eval::matches(want, value))
}

fn trigger_entity(trigger: &Trigger) -> Option<&EntityId> {
    match trigger {
        Trigger::State { entity, .. } => Some(entity),
        _ => None,
    }
}

fn reads(reads: Vec<eval::Read>) -> Vec<Read> {
    reads.into_iter().map(to_read).collect()
}

/// The evaluator's read as the trace keeps it (the trace's type compiles without CEL, so the two
/// crates can't share one).
fn to_read(read: eval::Read) -> Read {
    Read {
        entity_id: read.entity_id,
        availability: read.availability,
        value: read.value,
    }
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// A value the way a person says it: on, off, 42, "rinse", unknown.
pub fn words(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Bool(true) => "on".into(),
        serde_json::Value::Bool(false) => "off".into(),
        serde_json::Value::Null => "unknown".into(),
        serde_json::Value::String(text) => format!("\"{text}\""),
        other => other.to_string(),
    }
}

fn typed(value: &TypedValue) -> String {
    match value {
        TypedValue::Bool(true) => "on".into(),
        TypedValue::Bool(false) => "off".into(),
        TypedValue::Number(n) => n.to_string(),
        TypedValue::Text(text) => format!("\"{text}\""),
        TypedValue::Null => "unknown".into(),
    }
}

/// A condition in a few words, for a step's note.
pub fn describe(condition: &Condition) -> String {
    match condition {
        Condition::State {
            entity,
            is,
            availability,
        } => match (is, availability) {
            (Some(is), _) => format!("{entity} is {}", typed(is)),
            (None, Some(availability)) => format!("{entity} is {availability:?}").to_lowercase(),
            (None, None) => entity.to_string(),
        },
        Condition::Expr { expr } => expr.as_str().to_owned(),
        Condition::Time { .. } => "time window".into(),
        Condition::Sun { .. } => "sun window".into(),
        Condition::All { conditions } => conditions
            .iter()
            .map(describe)
            .collect::<Vec<_>>()
            .join(" and "),
        Condition::Any { conditions } => conditions
            .iter()
            .map(describe)
            .collect::<Vec<_>>()
            .join(" or "),
        Condition::Not { condition } => format!("not ({})", describe(condition)),
    }
}

fn describe_wait(until: &WaitUntil) -> String {
    match until {
        WaitUntil::State {
            entity, is, hold, ..
        } => {
            let is = is.as_ref().map(typed).unwrap_or_else(|| "available".into());
            match hold {
                Some(hold) => format!("{entity} is {is} for {}", hold.as_str()),
                None => format!("{entity} is {is}"),
            }
        }
        WaitUntil::Expr { expr, hold } => match hold {
            Some(hold) => format!("{} for {}", expr.as_str(), hold.as_str()),
            None => expr.as_str().to_owned(),
        },
    }
}
