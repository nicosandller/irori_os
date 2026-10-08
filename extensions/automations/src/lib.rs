//! The Automations extension (`docs/specs/flows.md`): the flow engine as a process, with its
//! flows on disk and a page to build, test and debug them.
//!
//! One task owns everything: the engine, the store, and what it knows of the home. It wakes for
//! the core's pushes (state changes, registry changes, the page's questions), for the engine's
//! next timer, for a call's answer, and every couple of seconds to notice edited flow files.

pub mod brief;
pub mod rpc;
pub mod store;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use irori_flow_types::Flow;
use irori_flows::{Arm, Effect, Engine, IdGen, ulid, validate};
use irori_protocol::WireCommand;
use irori_protocol::engine::{EngineClient, Incoming, Registry};
use irori_rules::clock::Place;
use irori_rules::{CallData, MapRegistry, RuleService};
use irori_types::{ContextId, Timestamp};
use tokio::sync::mpsc;

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
    calls: mpsc::UnboundedSender<(u64, Result<(), String>)>,
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
            calls,
        };
        service.arm();
        service.engine.start(now());
        service.apply_effects();
        Ok((service, answers))
    }

    /// Checks every flow against the home and hands them to the engine. A definition not seen
    /// before — a hand edit, say — is kept as a version, so its runs can be drawn on it.
    pub fn arm(&mut self) {
        let at = now();
        for flow in self.store.flows() {
            if let Err(error) = self.store.keep_version(flow, at) {
                tracing::warn!(flow = %flow.id, %error, "couldn't keep this version");
            }
        }
        let arms: Vec<Arm> = self
            .store
            .flows()
            .map(|flow| Arm {
                flow: flow.clone(),
                problems: validate::check(flow, &self.registry),
            })
            .collect();
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
                let result = rpc::handle(self, &request.method, request.params.clone()).await;
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
    let entities: Vec<_> = validate::watched(flow, &service.registry)
        .into_iter()
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
