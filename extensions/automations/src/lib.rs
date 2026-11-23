//! The Automations extension (`docs/specs/flows.md`): the flow engine as a process, with its
//! flows on disk and a page to build, test and debug them.
//!
//! One task owns everything: the engine, the store, and what it knows of the home. It wakes for
//! the core's pushes (state changes, registry changes, the page's questions), for the engine's
//! next timer, for a call's answer, and every couple of seconds to notice edited flow files.

pub mod brief;
pub mod people;
pub mod rpc;
pub mod store;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use irori_flow_types::Flow;
use irori_flow_types::trace::{AbortReason, Outcome};
use irori_flows::{Arm, Effect, Engine, IdGen, ulid, validate};
use irori_protocol::WireCommand;
use irori_protocol::engine::{EngineClient, Incoming, Registry};
use irori_rules::clock::Place;
use irori_rules::{CallData, MapRegistry, RuleService};
use irori_types::{ContextId, RuleId, Timestamp, UserId};
use tokio::sync::mpsc;

use crate::people::{Standing, View};
use crate::store::Store;

/// How often flow files are checked for edits made outside the page.
const POLL: Duration = Duration::from_secs(2);

/// The wall clock. The engine itself never reads it (it's handed "now"); this process does.
pub fn now() -> Timestamp {
    Timestamp::from_jiff(jiff::Timestamp::now())
}

/// ULIDs from the clock and the operating system's randomness.
#[derive(Debug, Default)]
pub struct RandomIds;

impl IdGen for RandomIds {
    fn run_id(&mut self, now: Timestamp) -> ContextId {
        let mut bytes = [0u8; 16];
        let _ = getrandom::fill(&mut bytes);
        let millis = u64::try_from(now.as_jiff().as_millisecond()).unwrap_or(0);
        ulid(millis, u128::from_le_bytes(bytes))
    }
}

/// The running extension.
pub struct Service {
    pub client: EngineClient,
    pub engine: Engine,
    pub store: Store,
    pub registry: MapRegistry,
    /// Which reading of the home `registry` is: a person's standing is built on one.
    home_was: u64,
    /// Where each person who has a flow, or has asked, stands now (`docs/specs/access.md` §4).
    pub people: BTreeMap<UserId, Standing>,
    /// Whether the flows have been armed once already: before that, what each flow was doing is
    /// not known, and a change made while Irori was stopped is the one to find.
    started: bool,
    /// What the turned-on flows that can't arm watch. A change to one of these can't be taken
    /// on an old reading of the home: what they are missing may just have arrived.
    waiting: BTreeSet<irori_types::EntityId>,
    calls: mpsc::UnboundedSender<(u64, Result<(), String>)>,
    /// The runs in progress when they were last kept on disk.
    kept_running: BTreeSet<ContextId>,
}

impl Service {
    /// What a flow of `owner`'s is checked against: the home itself for the home's own, else
    /// the home as that person has it. Why not, when it can't be told.
    pub fn view(&self, owner: Option<&UserId>) -> Result<View<'_>, String> {
        let Some(user) = owner else {
            return Ok(View {
                owner: true,
                registry: &self.registry,
                seen: None,
            });
        };
        match self.people.get(user) {
            Some(standing) if standing.here => Ok(View {
                owner: standing.owner,
                registry: &standing.registry,
                seen: standing.seen.as_ref(),
            }),
            Some(_) => Err(format!(
                "{user} is not in the home any more, so what their flow may touch can't be told"
            )),
            None => Err("what the owner of this flow can reach could not be told yet".to_owned()),
        }
    }

    /// Everything wrong with `flow`, errors first: what the checker finds, and a warning
    /// wherever it compares a number with a sensor Irori converts to °C. Checked as its owner:
    /// against the rooms they have and what they may send (`docs/specs/access.md` §4).
    pub fn problems(&self, flow: &irori_flow_types::Flow) -> Vec<irori_flow_types::api::Problem> {
        let view = match self.view(flow.owner.as_ref()) {
            Ok(view) => view,
            Err(why) => {
                return vec![irori_flow_types::api::Problem {
                    severity: irori_flow_types::api::Severity::Error,
                    node: None,
                    wire: None,
                    field: None,
                    message: why,
                }];
            }
        };
        let native: BTreeMap<_, _> = self
            .engine
            .states()
            .into_iter()
            .filter(|state| view.sees(&state.entity_id))
            .filter_map(|state| {
                let unit = state
                    .attributes
                    .iter()
                    .find(|(key, _)| key.as_str() == irori_types::units::Conversion::NATIVE_UNIT)
                    .and_then(|(_, unit)| unit.as_str().map(str::to_owned))?;
                Some((state.entity_id, unit))
            })
            .collect();
        let mut problems = validate::check_for(flow, view.registry, view.owner);
        problems.extend(validate::converted(flow, view.registry, &native));
        if flow.owner.is_some() {
            // A person's flow is checked against their rooms, and says so: what is outside them
            // reads as no entity at all, which is the same thing whether it is hidden or gone.
            for problem in &mut problems {
                if problem.message.contains("no such entity") {
                    problem.message = problem.message.replace(
                        " — check the entity id",
                        " in your rooms — check the entity id",
                    );
                }
            }
        }
        problems
    }
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Service")
            .field("engine", &self.engine)
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

/// Where flows live: `flows/` in the config directory when the core says where that is, else in
/// the extension's own data (and it says so).
pub fn flows_dir() -> (PathBuf, PathBuf) {
    let data = std::env::var_os("IRORI_EXTENSION_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let flows = match std::env::var_os("IRORI_CONFIG_DIR") {
        Some(config) => PathBuf::from(config).join("flows"),
        None => {
            tracing::warn!(
                "the core didn't say where the config directory is; keeping flows in this \
                 extension's own data instead"
            );
            data.join("flows")
        }
    };
    (flows, data)
}

/// Where the home is, as the core said it, if it said a time zone this engine knows.
fn place_of(registry: &Registry) -> Option<Place> {
    let said = registry.place.as_ref()?;
    let zone = said.time_zone.as_deref()?;
    match Place::new(zone, said.latitude.zip(said.longitude)) {
        Ok(place) => Some(place),
        Err(error) => {
            tracing::warn!(%error, "the home's time zone can't be used; time triggers stay off");
            None
        }
    }
}

/// The home as the checks read it, and where it is. What the checks are told about time comes
/// from the place this engine could actually build, so a flow is never armed on a zone it
/// can't tell the time in.
fn registry_view(registry: Registry) -> (MapRegistry, Option<Place>) {
    let place = place_of(&registry);
    let view = MapRegistry {
        entities: registry
            .entities
            .into_iter()
            .map(|entity| (entity.id.clone(), entity))
            .collect(),
        timezone: place.is_some(),
        location: place.as_ref().is_some_and(Place::has_location),
    };
    (view, place)
}

impl Service {
    /// Connects to the core, loads the home and the flows, and arms them.
    pub async fn start(
        client: EngineClient,
        store: Store,
    ) -> Result<(Self, mpsc::UnboundedReceiver<(u64, Result<(), String>)>), String> {
        let (calls, answers) = mpsc::unbounded_channel();
        let mut engine = Engine::new(Box::new(RandomIds));
        // Subscribed before reading, so nothing that happens in between is missed.
        client
            .subscribe(true, true)
            .await
            .map_err(|e| e.to_string())?;
        engine.load_states(client.get_states().await.map_err(|e| e.to_string())?);
        let (registry, place) =
            registry_view(client.get_registry().await.map_err(|e| e.to_string())?);
        engine.set_place(place, now());
        let mut service = Self {
            client,
            engine,
            store,
            registry,
            home_was: 0,
            people: BTreeMap::new(),
            started: false,
            waiting: BTreeSet::new(),
            calls,
            kept_running: BTreeSet::new(),
        };
        // Runs a stop with no warning cut off last time: recorded, so the run's page says what
        // happened instead of the run never having been.
        for mut run in service.store.take_running() {
            tracing::warn!(flow = %run.flow_id, run = %run.run_id, "a run was cut off when Irori stopped");
            run.outcome = Some(Outcome::Aborted {
                reason: AbortReason::Interrupted,
            });
            run.finished_at = Some(now());
            service.store.add_run(&run);
        }
        service.refresh_people().await;
        service.arm();
        service.started = true;
        service.engine.start(now());
        service.apply_effects();
        Ok((service, answers))
    }

    /// Asks the core where each person who owns a flow stands now, and takes into account the
    /// people it has not been asked about. Called whenever the home or the people changed, before
    /// the flows are checked again. A flow whose owner is no longer in the home is kept, turned
    /// off, and handed to the owner of the home.
    pub async fn refresh_people(&mut self) {
        let owners: BTreeSet<UserId> = self
            .store
            .flows()
            .filter_map(|flow| flow.owner.clone())
            .chain(self.people.keys().cloned())
            .collect();
        for user in owners {
            self.learn(&user).await;
        }
        self.hand_over_the_gone();
    }

    /// Asks the core where this person stands, and keeps the answer. When the core can't say,
    /// nothing is known of them: what they could reach before might be more than they can now,
    /// so their flows wait to be checked until the core answers.
    pub async fn learn(&mut self, user: &UserId) {
        match self.client.get_reach(user.clone()).await {
            Ok(reach) => {
                // The same answer about the same home is what is already known.
                if self
                    .people
                    .get(user)
                    .is_some_and(|known| known.told == reach && known.home_was == self.home_was)
                {
                    return;
                }
                let standing = Standing::of(reach, &self.registry, self.home_was);
                self.people.insert(user.clone(), standing);
            }
            Err(error) => {
                tracing::warn!(%user, %error, "couldn't ask what this person can reach");
                self.people.remove(user);
            }
        }
    }

    /// A removed person's flows are kept, turned off, and the home's own: nothing they made is
    /// lost, and nothing keeps running on a person who is not there to answer for it.
    fn hand_over_the_gone(&mut self) {
        let gone: Vec<(RuleId, UserId)> = self
            .store
            .flows()
            .filter_map(|flow| {
                let owner = flow.owner.as_ref()?;
                let standing = self.people.get(owner)?;
                (!standing.here).then(|| (flow.id.clone(), owner.clone()))
            })
            .collect();
        for (id, owner) in gone {
            let Some(mut flow) = self.store.flow(&id).cloned() else {
                continue;
            };
            flow.owner = None;
            flow.enabled = false;
            match self.store.save(&flow, now(), None) {
                Ok(()) => self.client.note(
                    "flow_handed_over",
                    id.as_str(),
                    &format!("{owner} was removed; it is kept, turned off, and the home's own"),
                ),
                Err(error) => {
                    tracing::warn!(flow = %id, %error, "couldn't hand over a removed person's flow");
                }
            }
        }
    }

    /// Checks every flow against the home and hands them to the engine. A definition not seen
    /// before — a hand edit, say — is kept as a version, so its runs can be drawn on it.
    ///
    /// A person's flow that is running and has lost what it needs of their rooms or their role
    /// is turned off, for good: it does not start again by itself when the room comes back
    /// (`docs/specs/access.md` §4). The reason is said once, to the activity log. Done here and
    /// not only when a flow was seen running, so a change made while Irori was stopped counts.
    /// A flow whose owner could not be asked about is left as it is, until they can be.
    pub fn arm(&mut self) {
        let at = now();
        for flow in self.store.flows() {
            if let Err(error) = self.store.keep_version(flow, at, None) {
                tracing::warn!(flow = %flow.id, %error, "couldn't keep this version");
            }
        }
        let before = self.engine.armed();
        self.turn_off_what_lost_access(at, &before);
        let arms: Vec<Arm> = self
            .store
            .flows()
            .map(|flow| Arm {
                flow: flow.clone(),
                problems: self.problems(flow),
            })
            .collect();
        self.engine.set_flows(arms, now());
        self.apply_effects();
        // Every change to the flows or the home comes through here, so this is never behind.
        // Asked of the engine, not worked out from what a flow names: an expression naming an
        // entity the home doesn't have can't be read, so that entity isn't among the named.
        let armed = self.engine.armed();
        self.waiting = self
            .store
            .flows()
            .filter(|flow| {
                matches!(
                    armed.get(&flow.id),
                    Some((irori_flow_types::api::Armed::Unarmed { .. }, _))
                )
            })
            .flat_map(|flow| validate::watched(flow, &self.registry))
            .collect();
    }

    /// Turns off the flows of people that are on and can't be, for want of what their owner
    /// has now: what is wrong with them would not be, were the owner the home's. A flow that is
    /// wrong for its own reasons (an entity the home doesn't have) is left to arm again when
    /// the home does.
    fn turn_off_what_lost_access(
        &mut self,
        at: Timestamp,
        before: &BTreeMap<RuleId, (irori_flow_types::api::Armed, String)>,
    ) {
        let lost: Vec<(Flow, String)> = self
            .store
            .flows()
            .filter(|flow| flow.enabled && flow.owner.is_some())
            // One that was running, or any at all when starting: a flow just written that can't
            // run never did, and is told so by the save.
            .filter(|flow| {
                !self.started
                    || matches!(
                        before.get(&flow.id),
                        Some((irori_flow_types::api::Armed::Armed, _))
                    )
            })
            .filter_map(|flow| {
                // Only when they could be asked: a person not told yet is not a person who lost.
                self.view(flow.owner.as_ref()).ok()?;
                let reason = self
                    .problems(flow)
                    .into_iter()
                    .find(|problem| problem.is_error())?
                    .message;
                let wrong_anyway = validate::check_for(flow, &self.registry, true)
                    .iter()
                    .any(irori_flow_types::api::Problem::is_error);
                (!wrong_anyway).then(|| (flow.clone(), reason))
            })
            .collect();
        for (mut flow, reason) in lost {
            flow.enabled = false;
            match self.store.save(&flow, at, None) {
                Ok(()) => self
                    .client
                    .note("flow_turned_off", flow.id.as_str(), &reason),
                Err(error) => {
                    tracing::warn!(flow = %flow.id, %error, "couldn't turn off a flow that can't run");
                }
            }
        }
    }

    /// Does what the engine asked: makes its calls, keeps its records.
    pub fn apply_effects(&mut self) {
        for effect in self.engine.take_effects() {
            match effect {
                Effect::Call {
                    call_id,
                    run_id,
                    parent_id,
                    entity,
                    service,
                    data,
                } => {
                    let client = self.client.clone();
                    let answers = self.calls.clone();
                    let (command, data) = command(service, data.as_ref());
                    tokio::spawn(async move {
                        let result = client
                            .call_service(entity, command, data, run_id, parent_id)
                            .await
                            .map_err(|e| e.to_string());
                        let _ = answers.send((call_id, result));
                    });
                }
                Effect::Finished(record) => {
                    tracing::info!(
                        flow = %record.flow_id,
                        run = %record.run_id,
                        outcome = %record.summary().outcome,
                        "run finished"
                    );
                    self.store.add_run(&record);
                }
                Effect::NearMiss(miss) => {
                    tracing::info!(flow = %miss.flow_id, "near-miss: {}", miss.message);
                    self.store.add_near_miss(&miss);
                }
            }
        }
        // Written when a run starts or ends, not on every step: enough to say it was cut off.
        let running = self.engine.in_progress();
        let ids: BTreeSet<ContextId> = running.iter().map(|run| run.run_id.clone()).collect();
        if ids != self.kept_running {
            self.store.keep_running(&running);
            self.kept_running = ids;
        }
    }

    /// Everything the core pushes.
    pub async fn incoming(&mut self, message: Incoming) -> bool {
        match message {
            Incoming::StateChanged {
                old_state,
                new_state,
                ..
            } => {
                self.engine
                    .state_changed(old_state.map(|s| *s), *new_state, now());
                self.apply_effects();
            }
            Incoming::Missed { events } => {
                // Said before the `RegistryChanged` that reads the home again: the triggers
                // that may have missed a change say so first.
                self.engine.missed(events, now());
                self.apply_effects();
            }
            Incoming::RegistryChanged => self.read_again().await,
            Incoming::App(request) => {
                let result = rpc::handle(
                    self,
                    &request.caller,
                    &request.method,
                    request.params.clone(),
                )
                .await;
                request.answer(result);
            }
            Incoming::Stop => return false,
        }
        true
    }

    /// Reads the registry, the people and the states again, and arms the flows on what it
    /// finds: what a `RegistryChanged` asks for. [`serve`] folds a burst of them into one.
    async fn read_again(&mut self) {
        // The registry changed, or this engine missed events: read both again.
        match self.client.get_registry().await {
            Ok(registry) => {
                let (registry, place) = registry_view(registry);
                self.registry = registry;
                self.home_was += 1;
                self.engine.set_place(place, now());
                // The people may have changed with the home: their rooms, their role, whether
                // they are still here.
                self.refresh_people().await;
                self.arm();
                // A time that was due as the home changed has just fired.
                self.apply_effects();
            }
            Err(error) => tracing::warn!(%error, "couldn't read the registry again"),
        }
        match self.client.get_states().await {
            Ok(states) => {
                self.engine.resync(states, now());
                self.apply_effects();
            }
            Err(error) => tracing::warn!(%error, "couldn't read the states again"),
        }
    }

    /// Whether a change to `entity` matters to a flow that can't arm yet: read the home first,
    /// or that flow would lose the change.
    fn waits_for(&self, entity: &irori_types::EntityId) -> bool {
        self.waiting.contains(entity)
    }

    /// The saved flow `id`, or the draft sent along with a request.
    pub fn flow_or_draft(
        &self,
        id: &irori_types::RuleId,
        draft: Option<Flow>,
    ) -> Result<Flow, String> {
        match draft {
            Some(flow) => Ok(flow),
            None => self
                .store
                .flow(id)
                .cloned()
                .ok_or_else(|| format!("there's no flow `{id}`")),
        }
    }
}

/// A rule's service as the core's command: the action after the dot, with `brightness_pct`
/// turned into `brightness`. Toggle takes no data.
pub fn command(
    service: RuleService,
    data: Option<&CallData>,
) -> (
    WireCommand,
    Option<serde_json::Map<String, serde_json::Value>>,
) {
    // As the core takes it: a light's `brightness_pct` worked into `brightness`. A toggle goes
    // without any, as it always has: what it becomes is the core's to decide.
    let data = service
        .service(data)
        .ok()
        .flatten()
        .and_then(|service| service.data());
    (service.action().to_owned(), data)
}

/// Runs the extension until the core says stop.
pub async fn run() -> Result<(), String> {
    let (_settings, client, incoming) = irori_protocol::engine::connect_stdio()
        .await
        .map_err(|e| e.to_string())?;
    let (flows, data) = flows_dir();
    let store = Store::open(flows, data);
    for problem in store.problems() {
        tracing::warn!(file = %problem.file, "{}", problem.reason);
    }
    let (service, answers) = Service::start(client, store).await?;
    tracing::info!(flows = service.store.flows().count(), "automations started");
    serve(service, incoming, answers).await;
    Ok(())
}

/// How long the home must go without another registry change before it is read again. An
/// extension describing its devices sends one change per device and per entity, thousands at
/// once on a big network: they are read as one.
pub const SETTLE: Duration = Duration::from_millis(250);

/// The longest a registry change waits to be read while more keep coming, so a flow on a device
/// that was just added arms even while another extension is still describing its own.
pub const SETTLE_AT_MOST: Duration = Duration::from_secs(1);

/// The least time between two readings of the home when one is asked for early. A flow that
/// names a device gone for good is always "waiting", so its busy sensor can't turn a burst back
/// into a reading per change.
const READ_EARLY_EVERY: Duration = Duration::from_millis(100);

/// A registry change not read yet: when the first and the last of them came.
#[derive(Debug, Clone, Copy)]
struct Unread {
    first: tokio::time::Instant,
    last: tokio::time::Instant,
}

impl Unread {
    fn due(self) -> tokio::time::Instant {
        (self.last + SETTLE).min(self.first + SETTLE_AT_MOST)
    }
}

/// What [`serve`] keeps about reading the home again.
#[derive(Debug, Default)]
struct Reading {
    unread: Option<Unread>,
    /// Messages taken off the queue ahead of a reading, still to handle, in order.
    held: VecDeque<Incoming>,
    last: Option<tokio::time::Instant>,
}

impl Reading {
    /// The next message, those held back first. Cancel-safe.
    async fn next(&mut self, incoming: &mut mpsc::UnboundedReceiver<Incoming>) -> Option<Incoming> {
        match self.held.pop_front() {
            Some(message) => Some(message),
            None => incoming.recv().await,
        }
    }

    /// Reads the home now. Registry changes already queued are answered by this reading, so
    /// they are dropped; everything else stays queued, in order.
    async fn read(
        &mut self,
        service: &mut Service,
        incoming: &mut mpsc::UnboundedReceiver<Incoming>,
    ) {
        if let Some(last) = self.last {
            tokio::time::sleep_until(last + READ_EARLY_EVERY).await;
        }
        self.unread = None;
        self.held
            .retain(|message| !matches!(message, Incoming::RegistryChanged));
        for _ in 0..incoming.len() {
            match incoming.try_recv() {
                Ok(Incoming::RegistryChanged) => {}
                Ok(message) => self.held.push_back(message),
                Err(_) => break,
            }
        }
        self.last = Some(tokio::time::Instant::now());
        service.read_again().await;
    }
}

/// Serves what the core sends until it says stop, then stops the runs.
///
/// State changes, the page's questions, call answers and timers are taken as they come, in
/// order. Registry changes are not read one by one: each re-read is the whole home, so a burst
/// of them is read once, [`SETTLE`] after the last or [`SETTLE_AT_MOST`] after the first.
///
/// The home is read at once when an old reading would give a wrong answer: for a page's
/// question, for a state a flow that can't arm yet watches, and after events were missed,
/// since what flows would read is then known to be out of date.
pub async fn serve(
    mut service: Service,
    mut incoming: mpsc::UnboundedReceiver<Incoming>,
    mut answers: mpsc::UnboundedReceiver<(u64, Result<(), String>)>,
) {
    let mut poll = tokio::time::interval(POLL);
    let mut reading = Reading::default();
    loop {
        let wake = service.engine.next_deadline().map(|at| {
            let wait = at.as_jiff().duration_since(now().as_jiff());
            tokio::time::Instant::now() + Duration::try_from(wait).unwrap_or(Duration::ZERO)
        });
        let due = reading.unread.map(Unread::due);
        tokio::select! {
            message = reading.next(&mut incoming) => {
                let Some(message) = message else { break };
                match message {
                    Incoming::RegistryChanged => {
                        let at = tokio::time::Instant::now();
                        reading.unread = Some(match reading.unread {
                            Some(unread) => Unread { last: at, ..unread },
                            None => Unread { first: at, last: at },
                        });
                    }
                    // Said first, as ever: the triggers that may have missed a change.
                    message @ Incoming::Missed { .. } => {
                        service.incoming(message).await;
                        reading.read(&mut service, &mut incoming).await;
                    }
                    message => {
                        let first = reading.unread.is_some()
                            && match &message {
                                Incoming::App(_) => true,
                                Incoming::StateChanged { entity_id, .. } => {
                                    service.waits_for(entity_id)
                                }
                                _ => false,
                            };
                        if first {
                            reading.read(&mut service, &mut incoming).await;
                        }
                        if !service.incoming(message).await {
                            break;
                        }
                    }
                }
            }
            Some((call_id, result)) = answers.recv() => {
                service.engine.call_finished(call_id, result, now());
                service.apply_effects();
            }
            () = sleep_until(wake) => {
                service.engine.advance(now());
                service.apply_effects();
            }
            () = sleep_until(due) => {
                reading.read(&mut service, &mut incoming).await;
            }
            _ = poll.tick() => {
                if service.store.poll() {
                    for problem in service.store.problems() {
                        tracing::warn!(file = %problem.file, "{}", problem.reason);
                    }
                    service.refresh_people().await;
                    service.arm();
                }
            }
        }
    }
    service.engine.shutdown(now());
    service.apply_effects();
}

async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// History the core keeps per entity; a backtest over a chattier entity covers less than a day.
pub const HISTORY_CAP: usize = 2_000;

/// A day, the window backtests and the timeline look back over.
pub fn a_day_before(now: Timestamp) -> Timestamp {
    Timestamp::from_jiff(
        now.as_jiff()
            .checked_sub(jiff::SignedDuration::from_hours(24))
            .unwrap_or(now.as_jiff()),
    )
}

/// Only for [`rpc`]: the history the flow watches since `since`.
pub(crate) async fn history(
    service: &Service,
    flow: &Flow,
    since: Timestamp,
) -> Result<BTreeMap<irori_types::EntityId, Vec<irori_types::EntityState>>, String> {
    // What a person's flow reads is what they have: the history of anything else is not theirs
    // to ask for, whatever a draft says.
    let view = service.view(flow.owner.as_ref())?;
    let entities: Vec<_> = validate::watched(flow, view.registry)
        .into_iter()
        .filter(|entity| view.sees(entity))
        .collect();
    if entities.is_empty() {
        return Ok(BTreeMap::new());
    }
    service
        .client
        .get_history(entities, since)
        .await
        .map_err(|e| e.to_string())
}
