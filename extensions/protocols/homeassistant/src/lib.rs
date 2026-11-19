//! The Home Assistant extension: a Home Assistant's devices, rooms and floors in Irori, while
//! Home Assistant keeps running them (milestone 11, issue #128).
//!
//! It signs in to Home Assistant's WebSocket API with a long-lived token, reads its floors, areas,
//! devices, entities and states, and follows their changes (`subscribe_entities`, and the
//! registries' own events). A command given in Irori becomes a `call_service`. It never changes
//! anything else in Home Assistant (see [`client`]).
//!
//! It goes easy on the core (see [`throttle`]): at most [`model::MAX_ENTITIES`] entities, each
//! sensor reporting at most once a second, about 150 reports a second in all, and nothing
//! reported that hasn't changed. Connection trouble is its own to handle: it reconnects with its
//! own backoff (a second, doubling to a minute) and marks what it brought over unavailable
//! meanwhile, so the host's restart is never needed.

pub mod client;
pub mod http;
pub mod model;
pub mod settings;
pub mod throttle;

#[cfg(any(test, feature = "fake"))]
pub mod fake;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::time::Duration;

use irori_protocol::{
    AvailabilityTarget, Health, Incoming, IncomingCall, IncomingPicture, Picture, Protocol,
    ProtocolContext, ProtocolError, ServiceError,
};
use irori_types::{
    Availability, Capabilities, ContextId, DeviceDescription, EntityDescription, SecretRequest,
    State, StateReport, SuggestedLayout, UniqueId, Unmodeled, Waiting,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::client::{Client, ConnectError, Connection, Event, HaError, Outgoing, REGISTRY_EVENTS};
use crate::model::{Bridged, HaState, Plan, Reading, Registry};
use crate::settings::Settings;
use crate::throttle::Throttle;

/// The extension, for [`irori_protocol::serve`].
#[derive(Debug)]
pub struct HomeAssistant;

impl Protocol for HomeAssistant {
    type Config = Settings;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");
    const ICON: Option<&'static str> = Some(include_str!("../icon.svg"));

    fn run(
        config: Settings,
        ctx: ProtocolContext,
    ) -> impl Future<Output = Result<(), ProtocolError>> + Send {
        drive(config, ctx, Timing::default())
    }
}

/// How long it waits for things. Tests make it quick.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// The first wait before connecting again.
    pub first_retry: Duration,
    /// The longest wait between tries.
    pub most_retry: Duration,
    /// The wait after Home Assistant refused the token, or it can't read the registries.
    pub refused_retry: Duration,
    /// How long registry changes settle before they're read again: a restart of an integration
    /// sends dozens at once. Each further change waits this long again, up to
    /// [`SETTLE_MOST`] times it in all.
    pub settle: Duration,
    /// How often it looks again while Home Assistant is starting.
    pub starting_retry: Duration,
    /// A connection that lasted this long starts the backoff over.
    pub steady: Duration,
    /// How often a batch of reports leaves ([`throttle::TICK`]).
    pub tick: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            first_retry: Duration::from_secs(1),
            most_retry: Duration::from_secs(60),
            refused_retry: Duration::from_secs(300),
            settle: Duration::from_secs(2),
            starting_retry: Duration::from_secs(5),
            steady: Duration::from_secs(60),
            tick: throttle::TICK,
        }
    }
}

/// The longest a burst of registry changes puts off reading them again, in [`Timing::settle`]s.
const SETTLE_MOST: u32 = 5;
/// How many events are handled before a person's request gets another look.
const BATCH: usize = 256;
/// How long a command's Home Assistant context is remembered, to tell which change it caused.
const CAUSE_KEPT: Duration = Duration::from_secs(30);
/// The waiting item while the token can't read the registries.
const NOT_ADMIN: &str = "homeassistant-not-admin";
/// What a request is told while Home Assistant isn't there.
const NOT_CONNECTED: &str = "Home Assistant isn't connected right now";
/// The most a picture can be (`docs/specs/protocols.md` §5.2), and how long it can take.
const PICTURE_MOST: usize = 2_000_000;
const PICTURE_WITHIN: Duration = Duration::from_millis(4500);
/// The picture types Irori takes.
const PICTURE_TYPES: [&str; 4] = ["image/jpeg", "image/png", "image/webp", "image/svg+xml"];

/// Answers what this extension doesn't do: actions, unpairing, live video and talking.
fn turn_away(incoming: Incoming) {
    match incoming {
        Incoming::Action(action) => action.reply(Err("Home Assistant has no actions here".into())),
        Incoming::Unpair(unpair) => {
            unpair.reply(Err(
                "remove it in Home Assistant; it leaves Irori too".into()
            ));
        }
        Incoming::Live(live) => live.feed.end(Some(
            "this camera can't be watched live through Home Assistant yet".into(),
        )),
        Incoming::Talk(talk) => talk.line.end(Some(
            "you can't talk through a camera in Home Assistant from Irori".into(),
        )),
        Incoming::Call(_) | Incoming::Picture(_) => {}
    }
}

/// Runs until told to stop. Never fails: Home Assistant going away is waited out here.
pub async fn drive(
    settings: Settings,
    mut ctx: ProtocolContext,
    timing: Timing,
) -> Result<(), ProtocolError> {
    let mut bridge = Bridge::default();
    let mut retry = timing.first_retry;
    loop {
        let url = settings.websocket_url();
        let Some(connected) =
            while_waiting(&mut ctx, client::connect(&url, settings.token.expose())).await
        else {
            return Ok(());
        };
        let wait = match connected {
            Ok(connection) => {
                let started = Instant::now();
                match session(&settings, &mut ctx, &mut bridge, connection, timing).await {
                    Ended::Stopped => return Ok(()),
                    Ended::NotAdmin => timing.refused_retry,
                    Ended::Lost(why) => {
                        tracing::warn!(%why, "lost Home Assistant");
                        bridge.lost(&ctx).await;
                        ctx.set_health(Health::Degraded(format!(
                            "lost Home Assistant ({why}); connecting again"
                        )))
                        .await;
                        if started.elapsed() >= timing.steady {
                            retry = timing.first_retry;
                        }
                        retry
                    }
                }
            }
            Err(ConnectError::Refused(why)) => {
                bridge.lost(&ctx).await;
                ctx.set_health(Health::Degraded(why)).await;
                timing.refused_retry
            }
            Err(ConnectError::Unreachable(why)) => {
                bridge.lost(&ctx).await;
                ctx.set_health(Health::Degraded(why)).await;
                retry
            }
        };
        if while_waiting(&mut ctx, tokio::time::sleep(wait))
            .await
            .is_none()
        {
            return Ok(());
        }
        if wait == retry {
            retry = (retry * 2).min(timing.most_retry);
        }
    }
}

/// Waits for `future`, answering commands meanwhile: Home Assistant isn't there to take them.
/// `None` once the extension should stop.
async fn while_waiting<T>(ctx: &mut ProtocolContext, future: impl Future<Output = T>) -> Option<T> {
    let future = std::pin::pin!(future);
    let mut future = future;
    loop {
        tokio::select! {
            biased;
            incoming = ctx.next() => match incoming {
                Some(Incoming::Call(call)) => call.reply(Err(ServiceError::unavailable(
                    NOT_CONNECTED,
                ))),
                Some(Incoming::Picture(picture)) => picture.reply(Err(NOT_CONNECTED.into())),
                Some(other) => turn_away(other),
                None => return None,
            },
            out = &mut future => return Some(out),
        }
    }
}

#[derive(Debug)]
enum Ended {
    Stopped,
    /// The token can't read the registries: said as a waiting item.
    NotAdmin,
    Lost(String),
}

/// What the core has been told, kept across connections so a reconnect only sends what changed.
#[derive(Debug, Default)]
struct Bridge {
    devices: HashMap<UniqueId, DeviceDescription>,
    entities: HashMap<UniqueId, EntityDescription>,
    unmodeled: Option<Vec<Unmodeled>>,
    waiting: Option<Vec<Waiting>>,
    layout: Option<SuggestedLayout>,
    /// Each entity's availability as last told.
    told: HashMap<UniqueId, Availability>,
    /// What the last plan brought over, by `entity_id`: kept while Home Assistant has no state
    /// for one (see [`model::plan_keeping`]).
    brought: HashMap<String, Bridged>,
}

impl Bridge {
    /// Tells the core what changed since it was last told.
    async fn apply(&mut self, ctx: &ProtocolContext, plan: &Plan) {
        for device in &plan.devices {
            if self.devices.get(&device.unique_id) != Some(device) {
                match ctx.describe_device(device.clone()).await {
                    Ok(()) => {
                        self.devices
                            .insert(device.unique_id.clone(), device.clone());
                    }
                    Err(why) => {
                        tracing::warn!(device = %device.unique_id, %why, "refused a device")
                    }
                }
            }
        }
        for bridged in &plan.entities {
            let entity = &bridged.description;
            if self.entities.get(&entity.unique_id) != Some(entity) {
                match ctx.describe_entity(entity.clone()).await {
                    Ok(()) => {
                        self.entities
                            .insert(entity.unique_id.clone(), entity.clone());
                    }
                    Err(why) => {
                        tracing::warn!(entity = %bridged.entity_id, %why, "refused an entity")
                    }
                }
            }
        }
        let keep: std::collections::HashSet<&UniqueId> = plan
            .entities
            .iter()
            .map(|b| &b.description.unique_id)
            .collect();
        let gone: Vec<UniqueId> = self
            .entities
            .keys()
            .filter(|id| !keep.contains(id))
            .cloned()
            .collect();
        for id in gone {
            let _ = ctx.remove_entity(id.clone()).await;
            self.entities.remove(&id);
            self.told.remove(&id);
        }
        let keep: std::collections::HashSet<&UniqueId> =
            plan.devices.iter().map(|d| &d.unique_id).collect();
        let gone: Vec<UniqueId> = self
            .devices
            .keys()
            .filter(|id| !keep.contains(id))
            .cloned()
            .collect();
        for id in gone {
            let _ = ctx.remove_device(id.clone()).await;
            self.devices.remove(&id);
        }
        if self.unmodeled.as_ref() != Some(&plan.unmodeled) {
            ctx.set_unmodeled(plan.unmodeled.clone()).await;
            self.unmodeled = Some(plan.unmodeled.clone());
        }
        self.set_waiting(ctx, plan.waiting.clone()).await;
        if self.layout.as_ref() != Some(&plan.layout) {
            ctx.suggest_layout(plan.layout.clone()).await;
            self.layout = Some(plan.layout.clone());
        }
    }

    async fn set_waiting(&mut self, ctx: &ProtocolContext, waiting: Vec<Waiting>) {
        if self.waiting.as_ref() != Some(&waiting) {
            ctx.set_waiting(waiting.clone()).await;
            self.waiting = Some(waiting);
        }
    }

    /// Home Assistant is gone: everything brought over is unavailable until it's back. One
    /// message, however many entities.
    async fn lost(&mut self, ctx: &ProtocolContext) {
        let available: Vec<UniqueId> = self
            .entities
            .keys()
            .filter(|id| self.told.get(*id) != Some(&Availability::Unavailable))
            .cloned()
            .collect();
        if available.is_empty() {
            return;
        }
        let _ = ctx
            .set_availability(
                AvailabilityTarget::Entities(available.clone()),
                Availability::Unavailable,
            )
            .await;
        for id in available {
            self.told.insert(id, Availability::Unavailable);
        }
    }
}

/// A command Home Assistant carried out: which entity, and the context its change will carry.
#[derive(Debug)]
struct Done {
    unique_id: UniqueId,
    ha_context: String,
    irori_context: ContextId,
}

/// One connection's worth of following Home Assistant.
struct Session<'a> {
    client: Client,
    bridge: &'a mut Bridge,
    entities: Vec<Bridged>,
    by_entity_id: BTreeMap<String, usize>,
    by_unique_id: HashMap<UniqueId, usize>,
    /// Each entity's state and attributes, as the compact changes build them up.
    cache: HashMap<String, (HaState, Option<String>)>,
    /// The value last reported per entity, so one that didn't change isn't reported again.
    reported: HashMap<UniqueId, Option<State>>,
    throttle: Throttle,
    availability: BTreeMap<UniqueId, Availability>,
    /// Home Assistant's context of a command Irori gave, to Irori's.
    causes: HashMap<String, (ContextId, Instant)>,
    entities_subscription: Option<u64>,
    /// Whether the entities subscription's first event has come: what it says is the state
    /// already there, not something happening now.
    heard_first: bool,
    registry_subscriptions: Vec<u64>,
}

async fn session(
    settings: &Settings,
    ctx: &mut ProtocolContext,
    bridge: &mut Bridge,
    connection: Connection,
    timing: Timing,
) -> Ended {
    let Connection {
        client,
        version,
        mut events,
        reader,
    } = connection;
    // However the session ends: the reader stops, and a command still waiting for an answer
    // hears at once that it won't come.
    struct Abort(tokio::task::JoinHandle<()>, Client);
    impl Drop for Abort {
        fn drop(&mut self) {
            self.0.abort();
            self.1.give_up();
        }
    }
    let _reader = Abort(reader, client.clone());
    tracing::info!(
        version = version.as_deref().unwrap_or("unknown"),
        "connected to Home Assistant"
    );

    // Registry changes first, so one made while the registries are read isn't missed.
    let mut registry_subscriptions = Vec::new();
    for event_type in REGISTRY_EVENTS {
        let subscribed = while_waiting(
            ctx,
            client.subscribe(Outgoing::SubscribeEvents { event_type }),
        )
        .await;
        match subscribed {
            None => return Ended::Stopped,
            Some(Ok(id)) => registry_subscriptions.push(id),
            Some(Err(why)) if why.is_unauthorized() => {
                return not_admin(ctx, bridge).await;
            }
            Some(Err(why)) => return Ended::Lost(why.to_string()),
        }
    }
    let registry = loop {
        match while_waiting(ctx, read(client.clone())).await {
            None => return Ended::Stopped,
            Some(Ok(registry)) => break registry,
            Some(Err(why)) if why.is_unauthorized() => return not_admin(ctx, bridge).await,
            // Read now, the entities still loading would seem gone, and be removed.
            Some(Err(why)) if why.is_starting() => {
                ctx.set_health(Health::Degraded(format!("{why}; waiting for it")))
                    .await;
                if while_waiting(ctx, tokio::time::sleep(timing.starting_retry))
                    .await
                    .is_none()
                {
                    return Ended::Stopped;
                }
            }
            Some(Err(why)) => return Ended::Lost(why.to_string()),
        }
    };
    let mut session = Session {
        client,
        bridge,
        entities: Vec::new(),
        by_entity_id: BTreeMap::new(),
        by_unique_id: HashMap::new(),
        cache: HashMap::new(),
        reported: HashMap::new(),
        throttle: Throttle::default(),
        availability: BTreeMap::new(),
        causes: HashMap::new(),
        entities_subscription: None,
        heard_first: false,
        registry_subscriptions,
    };
    if let Err(why) = session.take(ctx, &registry, settings).await {
        return Ended::Lost(why.to_string());
    }
    ctx.set_health(Health::Running).await;

    let (done_tx, mut done) = mpsc::channel::<Done>(64);
    let mut tick = tokio::time::interval(timing.tick);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // When the registries are read again, and when the first change since the last read came.
    let mut settle_at: Option<Instant> = None;
    let mut first_change: Option<Instant> = None;
    // A read under way: commands are answered meanwhile.
    let mut reading: Option<RegistryRead> = None;
    loop {
        let flushing = !session.throttle.is_empty() || !session.availability.is_empty();
        let idle = reading.is_none();
        let settle = async {
            match settle_at {
                Some(at) if idle => tokio::time::sleep_until(at).await,
                _ => std::future::pending().await,
            }
        };
        let read_done = async {
            match reading.as_mut() {
                Some(read) => read.await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            // A person's command is answered even while events are waiting.
            biased;
            incoming = ctx.next() => match incoming {
                Some(Incoming::Call(call)) => session.call(call, &done_tx),
                Some(Incoming::Picture(picture)) => session.picture(picture, settings),
                Some(other) => turn_away(other),
                None => return Ended::Stopped,
            },
            Some(finished) = done.recv() => session.done(finished),
            event = events.recv() => match event {
                Some(event) => {
                    let mut arrived = vec![event];
                    while arrived.len() < BATCH
                        && let Ok(more) = events.try_recv()
                    {
                        arrived.push(more);
                    }
                    for event in arrived {
                        if session.heard(event) {
                            let now = Instant::now();
                            let first = *first_change.get_or_insert(now);
                            settle_at = Some((now + timing.settle).min(first + timing.settle * SETTLE_MOST));
                        }
                    }
                }
                None => return Ended::Lost("the connection closed".to_owned()),
            },
            _ = tick.tick(), if flushing => session.flush(ctx).await,
            () = settle => {
                settle_at = None;
                first_change = None;
                reading = Some(Box::pin(read(session.client.clone())));
            }
            read = read_done => {
                reading = None;
                match read {
                    Ok(registry) => {
                        if let Err(why) = session.take(ctx, &registry, settings).await {
                            return Ended::Lost(why.to_string());
                        }
                    }
                    // Stopping, most likely: what's there now isn't what it brought over.
                    Err(why) if why.is_starting() => {
                        settle_at.get_or_insert_with(|| Instant::now() + timing.starting_retry);
                    }
                    Err(why) => return Ended::Lost(why.to_string()),
                }
            }
        }
    }
}

async fn not_admin(ctx: &ProtocolContext, bridge: &mut Bridge) -> Ended {
    let reason = "This access token's user isn't an administrator, so it can't read Home Assistant's devices, areas and entities. Make a long-lived token as an administrator and enter it here.";
    let item = Waiting {
        unique_id: UniqueId::try_from(NOT_ADMIN).expect("a valid id"),
        name: model::name("Home Assistant").expect("a valid name"),
        reason: reason.to_owned(),
        secret: Some(SecretRequest {
            path: vec!["token".to_owned()],
            label: "Access token".to_owned(),
            hint: Some(
                "In Home Assistant, signed in as an administrator: your profile → Security → Long-lived access tokens → Create token".to_owned(),
            ),
            username: None,
        }),
    };
    bridge.set_waiting(ctx, vec![item]).await;
    ctx.set_health(Health::Degraded(reason.to_owned())).await;
    Ended::NotAdmin
}

/// Reads everything this extension needs from Home Assistant.
/// A read of the registries under way.
type RegistryRead = std::pin::Pin<Box<dyn Future<Output = Result<Registry, HaError>> + Send>>;

/// Refuses while Home Assistant isn't `RUNNING`: starting, its entities appear one integration
/// at a time; stopping, they go.
async fn read(client: Client) -> Result<Registry, HaError> {
    let ask = |message| client.request(message, client::READ_TIMEOUT);
    let config = ask(Outgoing::GetConfig).await?;
    tracing::debug!(
        location = config["location_name"].as_str().unwrap_or(""),
        "read Home Assistant's config"
    );
    if let Some(state) = config["state"].as_str()
        && state != "RUNNING"
    {
        return Err(HaError::starting(state));
    }
    let floors = ask(Outgoing::FloorRegistry).await?;
    let areas = ask(Outgoing::AreaRegistry).await?;
    let devices = ask(Outgoing::DeviceRegistry).await?;
    let entities = ask(Outgoing::EntityRegistry).await?;
    let states = ask(Outgoing::GetStates).await?;
    Ok(Registry {
        floors: model::list(&floors),
        areas: model::list(&areas),
        devices: model::list(&devices),
        entities: model::list(&entities),
        states: model::states(&states),
    })
}

impl Session<'_> {
    /// Brings over what `registry` says, and follows those entities' changes.
    async fn take(
        &mut self,
        ctx: &ProtocolContext,
        registry: &Registry,
        settings: &Settings,
    ) -> Result<(), HaError> {
        let plan = model::plan_keeping(registry, settings, &self.bridge.brought);
        self.bridge.apply(ctx, &plan).await;
        self.bridge.brought = plan
            .entities
            .iter()
            .map(|b| (b.entity_id.clone(), b.clone()))
            .collect();
        for id in &plan.absent {
            self.tell(id.clone(), Availability::Unavailable);
        }
        let before: Vec<&str> = self.entities.iter().map(|b| b.entity_id.as_str()).collect();
        let after: Vec<&str> = plan.entities.iter().map(|b| b.entity_id.as_str()).collect();
        let resubscribe = before != after || self.entities_subscription.is_none();
        let kept: std::collections::HashSet<&UniqueId> = plan
            .entities
            .iter()
            .map(|b| &b.description.unique_id)
            .collect();
        for gone in self
            .entities
            .iter()
            .filter(|b| !kept.contains(&b.description.unique_id))
        {
            self.throttle.forget(&gone.description.unique_id);
            self.reported.remove(&gone.description.unique_id);
        }
        self.entities = plan.entities;
        self.by_entity_id = model::by_entity_id(&self.entities);
        self.by_unique_id = self
            .entities
            .iter()
            .enumerate()
            .map(|(index, b)| (b.description.unique_id.clone(), index))
            .collect();
        // A capability that changed (a new color mode) can change how a value reads.
        self.reported
            .retain(|id, _| self.by_unique_id.contains_key(id));
        if !resubscribe {
            return Ok(());
        }
        if let Some(old) = self.entities_subscription.take() {
            self.client
                .request(
                    Outgoing::Unsubscribe { subscription: old },
                    client::READ_TIMEOUT,
                )
                .await?;
        }
        if self.entities.is_empty() {
            return Ok(());
        }
        let entity_ids = self.entities.iter().map(|b| b.entity_id.clone()).collect();
        let id = self
            .client
            .subscribe(Outgoing::SubscribeEntities { entity_ids })
            .await?;
        self.entities_subscription = Some(id);
        self.heard_first = false;
        Ok(())
    }

    /// Handles one event. `true` when the registries changed and want reading again.
    fn heard(&mut self, event: Event) -> bool {
        if self.registry_subscriptions.contains(&event.subscription) {
            return true;
        }
        if Some(event.subscription) != self.entities_subscription {
            return false;
        }
        let replayed = !std::mem::replace(&mut self.heard_first, true);
        let now = Instant::now();
        if let Some(added) = event.event.get("a").and_then(Value::as_object) {
            for (entity_id, compact) in added {
                let state = HaState {
                    state: compact["s"].as_str().unwrap_or("unknown").to_owned(),
                    attributes: compact["a"].as_object().cloned().unwrap_or_default(),
                };
                let context = model::context_id(compact.get("c")).map(str::to_owned);
                self.cache.insert(entity_id.clone(), (state, context));
                self.read_out(entity_id, replayed, true, now);
            }
        }
        if let Some(changed) = event.event.get("c").and_then(Value::as_object) {
            for (entity_id, diff) in changed {
                let Some((state, context)) = self.cache.get_mut(entity_id) else {
                    continue;
                };
                let mut new_state = false;
                if let Some(plus) = diff.get("+") {
                    if let Some(s) = plus["s"].as_str() {
                        new_state = true;
                        s.clone_into(&mut state.state);
                    }
                    if let Some(attributes) = plus["a"].as_object() {
                        for (key, value) in attributes {
                            state.attributes.insert(key.clone(), value.clone());
                        }
                    }
                    if let Some(id) = model::context_id(plus.get("c")) {
                        *context = Some(id.to_owned());
                    }
                }
                if let Some(removed) = diff["-"]["a"].as_array() {
                    for key in removed.iter().filter_map(Value::as_str) {
                        state.attributes.remove(key);
                    }
                }
                self.read_out(entity_id, false, new_state, now);
            }
        }
        if let Some(removed) = event.event.get("r").and_then(Value::as_array) {
            for entity_id in removed.iter().filter_map(Value::as_str) {
                self.cache.remove(entity_id);
                if let Some(&index) = self.by_entity_id.get(entity_id) {
                    let id = self.entities[index].description.unique_id.clone();
                    self.tell(id, Availability::Unavailable);
                }
            }
        }
        false
    }

    /// Reports what the cached state of `entity_id` says, if it says something new.
    /// `new_state`: Home Assistant's state itself changed (an event's press is only that).
    fn read_out(&mut self, entity_id: &str, replayed: bool, new_state: bool, now: Instant) {
        let Some(&index) = self.by_entity_id.get(entity_id) else {
            return;
        };
        let Some((state, context)) = self.cache.get(entity_id) else {
            return;
        };
        let bridged = &self.entities[index];
        let id = bridged.description.unique_id.clone();
        let caps = &bridged.description.capabilities;
        let caused_by = context
            .as_ref()
            .and_then(|context| self.causes.get(context))
            .map(|(irori, _)| irori.clone());
        let reading = model::reading(caps, &state.state, &state.attributes);
        let event = matches!(caps, Capabilities::Event(_));
        let sensor = matches!(caps, Capabilities::Sensor(_));
        match reading {
            Reading::Unavailable => self.tell(id, Availability::Unavailable),
            Reading::Nothing => self.tell(id, Availability::Available),
            Reading::Value(value) => {
                self.tell(id.clone(), Availability::Available);
                if event && !new_state && !replayed {
                    // Only an attribute changed: nothing was pressed.
                    return;
                }
                let unchanged = self.reported.get(&id) == Some(&value);
                if unchanged && !event && caused_by.is_none() {
                    return;
                }
                self.reported.insert(id.clone(), value.clone());
                self.throttle.push(
                    StateReport {
                        unique_id: id,
                        state: value,
                        attributes: Default::default(),
                        caused_by,
                        replayed,
                    },
                    sensor,
                    now,
                );
            }
        }
    }

    fn tell(&mut self, id: UniqueId, availability: Availability) {
        if self.bridge.told.get(&id) == Some(&availability) {
            self.availability.remove(&id);
        } else {
            self.availability.insert(id, availability);
        }
    }

    /// Sends what's due: availability first, then a batch of reports.
    async fn flush(&mut self, ctx: &ProtocolContext) {
        let mut available = Vec::new();
        let mut unavailable = Vec::new();
        for (id, availability) in std::mem::take(&mut self.availability) {
            self.bridge.told.insert(id.clone(), availability);
            match availability {
                Availability::Available => available.push(id),
                Availability::Unavailable => unavailable.push(id),
            }
        }
        for (ids, availability) in [
            (unavailable, Availability::Unavailable),
            (available, Availability::Available),
        ] {
            if !ids.is_empty() {
                let _ = ctx
                    .set_availability(AvailabilityTarget::Entities(ids), availability)
                    .await;
            }
        }
        let now = Instant::now();
        for report in self.throttle.take(throttle::PER_TICK, now) {
            ctx.report_state(report);
        }
        let replaced = self.throttle.take_replaced();
        if replaced > 0 {
            tracing::trace!(replaced, "values replaced before they were reported");
        }
        self.causes
            .retain(|_, (_, at)| now.duration_since(*at) < CAUSE_KEPT);
    }

    /// A command from Irori: carried out by Home Assistant, answered here once it has been.
    fn call(&mut self, call: IncomingCall, done: &mpsc::Sender<Done>) {
        let Some(&index) = self.by_unique_id.get(&call.call.unique_id) else {
            call.reply(Err(ServiceError::unavailable(
                "Home Assistant no longer has this entity",
            )));
            return;
        };
        let bridged = &self.entities[index];
        let (domain, service, data) = match model::command(bridged, &call.call.service) {
            Ok(found) => found,
            Err(why) => {
                call.reply(Err(why));
                return;
            }
        };
        let message = Outgoing::CallService {
            domain,
            service,
            entity_id: bridged.entity_id.clone(),
            data,
        };
        let client = self.client.clone();
        let done = done.clone();
        tokio::spawn(async move {
            match client.request(message, client::CALL_TIMEOUT).await {
                Ok(answer) => {
                    if let Some(ha_context) = model::context_id(answer.get("context")) {
                        let _ = done
                            .send(Done {
                                unique_id: call.call.unique_id.clone(),
                                ha_context: ha_context.to_owned(),
                                irori_context: call.call.context.id.clone(),
                            })
                            .await;
                    }
                    call.reply(Ok(()));
                }
                Err(why) => call.reply(Err(ServiceError::failed(format!(
                    "Home Assistant didn't do it: {why}"
                )))),
            }
        });
    }

    /// A camera's picture as it is now, from Home Assistant's camera proxy. Its token stays in
    /// Home Assistant: the request signs in with the extension's own.
    fn picture(&self, picture: IncomingPicture, settings: &Settings) {
        let camera = self
            .by_unique_id
            .get(&picture.unique_id)
            .map(|&index| &self.entities[index])
            .filter(|bridged| matches!(bridged.description.capabilities, Capabilities::Camera(_)));
        let Some(camera) = camera else {
            picture.reply(Err("that isn't one of Home Assistant's cameras".into()));
            return;
        };
        let path = format!("/api/camera_proxy/{}", camera.entity_id);
        let base = settings.url.clone();
        let token = settings.token.expose().to_owned();
        tokio::spawn(async move {
            let answer = http::get(&base, &path, &token, PICTURE_MOST, PICTURE_WITHIN).await;
            picture.reply(answer.and_then(|answer| {
                if PICTURE_TYPES.contains(&answer.content_type.as_str()) {
                    Ok(Picture {
                        content_type: answer.content_type,
                        bytes: answer.body,
                    })
                } else {
                    Err(format!(
                        "Home Assistant sent a {} rather than a picture",
                        if answer.content_type.is_empty() {
                            "reply"
                        } else {
                            answer.content_type.as_str()
                        }
                    ))
                }
            }));
        });
    }

    /// A command was carried out. Its change may already have arrived, before the answer did:
    /// then it's reported again, now saying what caused it.
    fn done(&mut self, done: Done) {
        let now = Instant::now();
        self.causes
            .insert(done.ha_context.clone(), (done.irori_context, now));
        let Some(&index) = self.by_unique_id.get(&done.unique_id) else {
            return;
        };
        let entity_id = self.entities[index].entity_id.clone();
        let arrived = self
            .cache
            .get(&entity_id)
            .is_some_and(|(_, context)| context.as_deref() == Some(done.ha_context.as_str()));
        if arrived {
            self.read_out(&entity_id, false, true, now);
        }
    }
}
