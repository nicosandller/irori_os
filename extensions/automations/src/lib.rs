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

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use irori_flow_types::Flow;
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
    /// Where each person who has a flow, or has asked, stands now (`docs/specs/access.md` §4).
    pub people: BTreeMap<UserId, Standing>,
    calls: mpsc::UnboundedSender<(u64, Result<(), String>)>,
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
                problem.message = problem
                    .message
                    .replace("no such entity in the home", "no such entity in your rooms");
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
            people: BTreeMap::new(),
            calls,
        };
        service.refresh_people().await;
        service.arm();
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

    /// Asks the core where this person stands, and keeps the answer. What it knew stays when
    /// the core can't say.
    pub async fn learn(&mut self, user: &UserId) {
        match self.client.get_reach(user.clone()).await {
            Ok(reach) => {
                let standing = Standing::of(reach, &self.registry);
                self.people.insert(user.clone(), standing);
            }
            Err(error) => {
                tracing::warn!(%user, %error, "couldn't ask what this person can reach");
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
    /// A person's flow that was running and no longer can is said once, to the activity log:
    /// the home changed under it (`docs/specs/access.md` §4).
    pub fn arm(&mut self) {
        let at = now();
        for flow in self.store.flows() {
            if let Err(error) = self.store.keep_version(flow, at, None) {
                tracing::warn!(flow = %flow.id, %error, "couldn't keep this version");
            }
        }
        let before = self.engine.armed();
        let arms: Vec<Arm> = self
            .store
            .flows()
            .map(|flow| Arm {
                flow: flow.clone(),
                problems: self.problems(flow),
            })
            .collect();
        for arm in &arms {
            let was_running = matches!(
                before.get(&arm.flow.id),
                Some((irori_flow_types::api::Armed::Armed, _))
            );
            let reason = arm.problems.iter().find(|problem| problem.is_error());
            if let (true, true, Some(reason), Some(_)) =
                (was_running, arm.flow.enabled, reason, &arm.flow.owner)
            {
                self.client
                    .note("flow_turned_off", arm.flow.id.as_str(), &reason.message);
            }
        }
        self.engine.set_flows(arms, now());
        self.apply_effects();
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
            Incoming::RegistryChanged => {
                // The registry changed, or this engine missed events: read both again.
                match self.client.get_registry().await {
                    Ok(registry) => {
                        let (registry, place) = registry_view(registry);
                        self.registry = registry;
                        self.engine.set_place(place, now());
                        // The people may have changed with the home: their rooms, their role,
                        // whether they are still here.
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
    let (_settings, client, mut incoming) = irori_protocol::engine::connect_stdio()
        .await
        .map_err(|e| e.to_string())?;
    let (flows, data) = flows_dir();
    let store = Store::open(flows, data);
    for problem in store.problems() {
        tracing::warn!(file = %problem.file, "{}", problem.reason);
    }
    let (mut service, mut answers) = Service::start(client, store).await?;
    tracing::info!(flows = service.store.flows().count(), "automations started");
    let mut poll = tokio::time::interval(POLL);
    loop {
        let wake = service.engine.next_deadline().map(|at| {
            let wait = at.as_jiff().duration_since(now().as_jiff());
            tokio::time::Instant::now() + Duration::try_from(wait).unwrap_or(Duration::ZERO)
        });
        tokio::select! {
            message = incoming.recv() => {
                let Some(message) = message else { break };
                if !service.incoming(message).await {
                    break;
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
    Ok(())
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
