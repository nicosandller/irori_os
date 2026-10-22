//! Matter devices, through the Open Home Foundation Matter Server (matter.js), which this
//! extension installs beside its own Node.js and runs, the way Zigbee runs Zigbee2MQTT.
//!
//! On start: install Node.js and the server if they aren't there (`server`), run it on
//! loopback, connect to its WebSocket API (`client`) and listen. Every node the server knows
//! becomes devices and entities (`map`), kept up to date from its events. A service call
//! becomes the node's own commands and attribute writes (`map::command`), sent from a task of
//! its own so readings keep flowing while a device answers. A person adds a device with the
//! `add_device` action and a pairing code, over the network or Bluetooth; removing one takes
//! Irori's fabric off it. Firmware the server finds is offered as each node's `update`.
//!
//! If the server's own process exits, `run` returns an error, and the core's restart with
//! backoff brings it back (`docs/specs/protocols.md` §3), as with Zigbee.

mod client;
mod map;
mod model;
mod server;
pub mod settings;

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use irori_protocol::{
    AvailabilityTarget, Health, Incoming, IncomingAction, IncomingCall, IncomingUnpair, Protocol,
    ProtocolContext, ProtocolError, ServiceError,
};
use irori_types::{Availability, ContextId, HvacMode, Service, State, StateReport, UniqueId};
use tokio::sync::mpsc;

use crate::client::{Client, CommandError, Connection, Event, SoftwareUpdate};
use crate::map::{Described, Known, Reading};
use crate::model::{Node, NodeId};
use crate::settings::Settings;

/// The one action this protocol declares (`irori-extension.toml`).
const ADD_DEVICE: &str = "add_device";
/// How long the server has to start listening before the extension gives up on it. The first
/// start on a small machine reads its whole device database first.
const STARTUP: Duration = Duration::from_secs(180);
/// Reports on an entity while its command is carried out, and this long after the device took
/// it, are that command's result (`caused_by`). A command changes several attributes, and each
/// arrives on its own: a light's on/off and level, a fan's mode and speed.
const SETTLING: Duration = Duration::from_secs(3);
/// Past this, a command still being carried out names nothing more: a device that never
/// answers isn't left naming it.
const CARRYING_OUT: Duration = Duration::from_secs(60);
/// When firmware is first looked for after the server starts, and how often after that. The
/// server looks in the DCL by itself; this asks what it found.
const FIRST_UPDATE_CHECK: Duration = Duration::from_secs(60);
const UPDATE_CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// At most this many events are taken together.
const BATCH: usize = 256;

/// The Matter protocol.
#[derive(Debug)]
pub struct Matter;

impl Protocol for Matter {
    type Config = Settings;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");
    const ICON: Option<&'static str> = Some(include_str!("../icon.svg"));

    async fn run(settings: Settings, ctx: ProtocolContext) -> Result<(), ProtocolError> {
        run(settings, ctx).await
    }
}

/// The server's own storage: Irori's fabric, its certificates, and every node commissioned
/// onto it. Under `IRORI_EXTENSION_DATA`, never inside the package directory, which an
/// uninstall (and so an upgrade) deletes: losing it strands every device on a fabric nobody
/// holds the keys to any more.
fn storage_dir() -> std::path::PathBuf {
    match std::env::var_os("IRORI_EXTENSION_DATA") {
        Some(given) => std::path::PathBuf::from(given).join("matter-server"),
        None => std::path::PathBuf::from("matter-server"),
    }
}

fn absolute(path: std::path::PathBuf) -> std::path::PathBuf {
    std::path::absolute(&path).unwrap_or(path)
}

/// Why the extension stopped driving the server.
#[derive(Debug)]
enum Ended {
    /// Told to stop.
    Stopped,
    ServerExited(std::io::Result<std::process::ExitStatus>),
    Failed(String),
}

async fn run(settings: Settings, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
    settings.check().map_err(ProtocolError::new)?;
    ctx.set_health(Health::Degraded(
        "installing Node.js and the Matter server".to_owned(),
    ))
    .await;
    server::ensure_node().await.map_err(ProtocolError::new)?;
    server::ensure_server(settings.matter_server_version.as_deref())
        .await
        .map_err(ProtocolError::new)?;
    tracing::info!(
        version = server::installed_version().as_deref().unwrap_or("unknown"),
        "starting the Matter server"
    );

    let storage = absolute(storage_dir());
    tokio::fs::create_dir_all(&storage)
        .await
        .map_err(|e| ProtocolError::new(format!("couldn't create {}: {e}", storage.display())))?;
    let entry = absolute(server::entry_point().map_err(ProtocolError::new)?);
    if server::port_in_use(settings.port).await {
        return Err(ProtocolError::new(format!(
            "something is already listening on 127.0.0.1:{port}, so the Matter server can't start \
             there. It may be a Matter server left running from before: stop it, or restart this \
             machine. Or choose another `port` in Matter's settings.",
            port = settings.port
        )));
    }
    let mut spawned = server::spawn(
        &irori_node_runtime::install::node_binary(),
        &entry,
        server::arguments(&settings, &storage),
    )
    .map_err(ProtocolError::new)?;
    ctx.set_health(Health::Degraded("starting the Matter server".to_owned()))
        .await;

    let url = format!("ws://127.0.0.1:{}/ws", settings.port);
    let ended = tokio::select! {
        connection = connect_patiently(&url) => match connection {
            Ok(connection) => {
                let exit = async { Ended::ServerExited(spawned.child.wait().await) };
                drive(connection, &settings, &mut ctx, exit).await
            }
            Err(why) => Ended::Failed(why),
        },
        status = spawned.child.wait() => Ended::ServerExited(status),
        () = ctx.stopped() => Ended::Stopped,
    };

    server::stop(&mut spawned.child).await;
    // After the exit, before the error is built: the server logs its reason and then exits,
    // so the line can still be in the pipe. Draining is what reads it.
    let said = spawned.drain_logs().await;
    match ended {
        Ended::Stopped => Ok(()),
        Ended::ServerExited(status) => Err(ProtocolError::new(match (status, said) {
            (Ok(status), Some(said)) => format!("the Matter server stopped: {said} ({status})"),
            (Ok(status), None) => format!("the Matter server exited: {status}"),
            (Err(e), _) => format!("the Matter server: {e}"),
        })),
        Ended::Failed(why) => Err(ProtocolError::new(match said {
            Some(said) => format!("{why} (the Matter server said: {said})"),
            None => why,
        })),
    }
}

/// Connects once the server is listening, trying again every second until [`STARTUP`] runs
/// out.
async fn connect_patiently(url: &str) -> Result<Connection, String> {
    let deadline = tokio::time::Instant::now() + STARTUP;
    loop {
        match client::connect(url).await {
            Ok(connection) => return Ok(connection),
            Err(why) if tokio::time::Instant::now() >= deadline => {
                return Err(format!(
                    "the Matter server didn't start listening within {} seconds: {why}",
                    STARTUP.as_secs()
                ));
            }
            Err(_) => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
}

/// A command run in its own task, answered back to the loop: a commission takes up to two
/// minutes, and the loop keeps reporting readings meanwhile.
#[derive(Debug)]
enum Done {
    Added(IncomingAction, Result<Node, String>),
    Removed(IncomingUnpair, NodeId, Result<(), String>),
    /// What the server knows of newer firmware for a node, and the `update.check` that asked,
    /// if one did.
    Checked(
        NodeId,
        Option<IncomingCall>,
        Result<Option<SoftwareUpdate>, String>,
    ),
    /// A service call's commands were all taken, or one wasn't.
    Commanded(IncomingCall, Result<(), ServiceError>),
}

/// Listens, then keeps Irori up to date with the server until told to stop, the connection
/// ends, or `exit` (the server's process ending) resolves.
async fn drive(
    connection: Connection,
    settings: &Settings,
    ctx: &mut ProtocolContext,
    exit: impl Future<Output = Ended>,
) -> Ended {
    let Connection {
        client,
        info,
        mut events,
        reader,
    } = connection;
    tracing::info!(sdk = %info.sdk_version, "connected to the Matter server");
    let nodes = match client.start_listening().await {
        Ok(nodes) => nodes,
        Err(e) => {
            reader.abort();
            return Ended::Failed(format!("the Matter server didn't start listening: {e}"));
        }
    };
    give_networks(&client, settings).await;
    let mut home = Home::new(info.compressed_fabric_id);
    home.updates = settings.firmware_updates;
    home.bluetooth = settings.bluetooth;
    home.bluetooth_broken = settings.bluetooth && info.bluetooth_enabled == Some(false);
    for node in nodes {
        home.apply(node, ctx, true).await;
    }
    home.publish(ctx).await;
    ctx.set_available_actions(vec![ADD_DEVICE.to_owned()]).await;

    let (done_tx, mut done) = mpsc::channel(8);
    let mut update_check = tokio::time::interval_at(
        tokio::time::Instant::now() + FIRST_UPDATE_CHECK,
        UPDATE_CHECK_EVERY,
    );
    update_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tokio::pin!(exit);
    let ended = loop {
        tokio::select! {
            // A person's request is answered even while events are waiting.
            biased;
            incoming = ctx.next() => match incoming {
                None => break Ended::Stopped,
                Some(Incoming::Action(action)) => home.add_device(action, &client, &done_tx),
                Some(Incoming::Unpair(unpair)) => home.unpair(unpair, &client, &done_tx, ctx).await,
                Some(Incoming::Call(call)) => home.call(call, &client, &done_tx),
                Some(Incoming::Picture(asked)) => {
                    asked.reply(Err("Matter devices have no cameras here yet".to_owned()));
                }
            },
            Some(finished) = done.recv() => home.finish(finished, ctx).await,
            _ = update_check.tick() => home.check_updates(None, &client, &done_tx),
            event = events.recv() => match event {
                Some(Event::ServerShutdown) => {
                    break Ended::Failed("the Matter server shut down".to_owned());
                }
                Some(event) => {
                    // And whatever came with it.
                    let mut arrived = vec![event];
                    let mut shut_down = false;
                    while arrived.len() < BATCH
                        && let Ok(more) = events.try_recv()
                    {
                        shut_down |= matches!(more, Event::ServerShutdown);
                        arrived.push(more);
                    }
                    home.events(arrived, ctx).await;
                    if shut_down {
                        break Ended::Failed("the Matter server shut down".to_owned());
                    }
                }
                None => break Ended::Failed("lost the connection to the Matter server".to_owned()),
            },
            ended = &mut exit => break ended,
        }
    };
    reader.abort();
    ended
}

/// Gives the server the Wi-Fi network and the Thread dataset a device added over Bluetooth
/// joins, when settings have them. A refusal is logged, not fatal: devices on the network can
/// still be added.
async fn give_networks(client: &Client, settings: &Settings) {
    if let Some(ssid) = &settings.wifi_ssid {
        let password = settings
            .wifi_password
            .as_ref()
            .map_or("", |password| password.expose());
        if let Err(e) = client.set_wifi_credentials(ssid, password).await {
            tracing::warn!(error = %e, "the Matter server didn't take the Wi-Fi network");
        }
    }
    if let Some(dataset) = &settings.thread_dataset
        && let Err(e) = client.set_thread_dataset(dataset.expose()).await
    {
        tracing::warn!(error = %e, "the Matter server didn't take the Thread dataset");
    }
}

/// Every node, as last told to Irori.
#[derive(Debug)]
struct Home {
    fabric: u64,
    nodes: BTreeMap<NodeId, Tracked>,
    /// Firmware is offered as an `update` entity (settings).
    updates: bool,
    /// Devices can be added over Bluetooth (settings).
    bluetooth: bool,
    /// The server says its Bluetooth doesn't work: asked for, and no adapter it could open.
    bluetooth_broken: bool,
    /// Newer firmware the server found, by node.
    newer: BTreeMap<NodeId, SoftwareUpdate>,
    /// Nodes whose firmware is being looked for.
    checking: BTreeSet<NodeId>,
    /// Who asked for each entity's latest command: the reports it causes say so.
    commanded: BTreeMap<UniqueId, Commanded>,
    /// The mode each thermostat was last in that wasn't off, for `climate.turn_on`.
    last_on: BTreeMap<UniqueId, HvacMode>,
    /// A device is being added: one at a time.
    adding: bool,
    /// Nodes being removed.
    removing: BTreeSet<NodeId>,
    /// What was last said, so it's said again only when it changes.
    unmodeled: Vec<irori_types::Unmodeled>,
    health: Option<Health>,
}

/// A command sent to an entity, and when the device took it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Commanded {
    context: ContextId,
    sent: Instant,
    taken: Option<Instant>,
}

#[derive(Debug)]
struct Tracked {
    node: Node,
    described: Described,
    states: BTreeMap<UniqueId, Option<State>>,
}

impl Home {
    fn new(fabric: u64) -> Self {
        Self {
            fabric,
            nodes: BTreeMap::new(),
            updates: true,
            bluetooth: false,
            bluetooth_broken: false,
            newer: BTreeMap::new(),
            checking: BTreeSet::new(),
            commanded: BTreeMap::new(),
            last_on: BTreeMap::new(),
            adding: false,
            removing: BTreeSet::new(),
            unmodeled: Vec::new(),
            health: None,
        }
    }

    /// Events that arrived together. Attribute changes to one node become one report each for
    /// its entities: a device tells each attribute a command changed on its own, a moment
    /// apart, and Irori is told the result, not every step to it.
    async fn events(&mut self, events: Vec<Event>, ctx: &ProtocolContext) {
        let mut changed: BTreeMap<NodeId, Node> = BTreeMap::new();
        for event in events {
            match event {
                Event::AttributeUpdated { node, path, value } => {
                    let Some(tracked) = self.nodes.get(&node) else {
                        continue;
                    };
                    changed
                        .entry(node)
                        .or_insert_with(|| tracked.node.clone())
                        .set(path, value);
                }
                other => {
                    for (_, node) in std::mem::take(&mut changed) {
                        self.apply(node, ctx, false).await;
                    }
                    match other {
                        Event::Node(node) => self.apply(node, ctx, false).await,
                        Event::NodeRemoved(node) => self.forget(node, ctx).await,
                        Event::AttributeUpdated { .. } | Event::ServerShutdown => {}
                    }
                }
            }
        }
        for (_, node) in changed {
            self.apply(node, ctx, false).await;
        }
        self.publish(ctx).await;
    }

    /// Tells Irori what changed about `node`: devices and entities new, changed or gone, its
    /// reachability, and the readings that differ from those last reported.
    async fn apply(&mut self, node: Node, ctx: &ProtocolContext, replayed: bool) {
        let described = map::describe(self.fabric, &node, self.updates);
        let newer = self.newer.get(&node.id).cloned();
        let known = Known {
            update: newer.as_ref(),
        };
        let previous = self.nodes.remove(&node.id);
        let before = previous.as_ref().map(|tracked| &tracked.described);
        for device in &described.devices {
            if before.is_none_or(|before| !before.devices.contains(device))
                && let Err(e) = ctx.describe_device(device.clone()).await
            {
                tracing::warn!(device = %device.unique_id, error = %e, "the core refused a device");
            }
        }
        for mapped in &described.entities {
            let same = before.is_some_and(|before| {
                before
                    .entities
                    .iter()
                    .any(|old| old.description == mapped.description)
            });
            if !same && let Err(e) = ctx.describe_entity(mapped.description.clone()).await {
                tracing::warn!(entity = %mapped.description.unique_id, error = %e, "the core refused an entity");
            }
        }
        if let Some(before) = before {
            for old in &before.entities {
                let id = &old.description.unique_id;
                if !described
                    .entities
                    .iter()
                    .any(|m| &m.description.unique_id == id)
                    && let Err(e) = ctx.remove_entity(id.clone()).await
                {
                    tracing::warn!(entity = %id, error = %e, "couldn't remove an entity");
                }
            }
            for old in before.devices.iter().rev() {
                if !described
                    .devices
                    .iter()
                    .any(|d| d.unique_id == old.unique_id)
                    && let Err(e) = ctx.remove_device(old.unique_id.clone()).await
                {
                    tracing::warn!(device = %old.unique_id, error = %e, "couldn't remove a device");
                }
            }
        }
        // A device is reachable when its node is, and, behind a bridge, when the bridge says so.
        let reachable = |node: &Node, described: &Described, device: &UniqueId| {
            node.available && !described.unreachable.contains(device)
        };
        for device in &described.devices {
            let now = reachable(&node, &described, &device.unique_id);
            let was = previous.as_ref().and_then(|tracked| {
                tracked
                    .described
                    .devices
                    .iter()
                    .any(|d| d.unique_id == device.unique_id)
                    .then(|| reachable(&tracked.node, &tracked.described, &device.unique_id))
            });
            let availability = if now {
                Availability::Available
            } else {
                Availability::Unavailable
            };
            if was != Some(now)
                && let Err(e) = ctx
                    .set_availability(
                        AvailabilityTarget::Device(device.unique_id.clone()),
                        availability,
                    )
                    .await
            {
                tracing::warn!(device = %device.unique_id, error = %e, "couldn't set availability");
            }
        }
        let mut states = BTreeMap::new();
        for mapped in &described.entities {
            let id = mapped.description.unique_id.clone();
            let state = map::state(&node, mapped, &known);
            if let Some(State::Climate(climate)) = &state
                && climate.hvac_mode != HvacMode::Off
            {
                self.last_on.insert(id.clone(), climate.hvac_mode);
            }
            let unchanged = previous
                .as_ref()
                .and_then(|tracked| tracked.states.get(&id))
                .is_some_and(|old| old == &state);
            if !unchanged {
                ctx.report_state(StateReport {
                    unique_id: id.clone(),
                    state: state.clone(),
                    attributes: BTreeMap::new(),
                    caused_by: caused_by(&mut self.commanded, &id),
                    replayed,
                });
            }
            states.insert(id, state);
        }
        self.nodes.insert(
            node.id,
            Tracked {
                node,
                described,
                states,
            },
        );
    }

    /// The node is gone from the server: its devices, bridged ones first, go from Irori too.
    async fn forget(&mut self, node: NodeId, ctx: &ProtocolContext) {
        self.newer.remove(&node);
        let Some(tracked) = self.nodes.remove(&node) else {
            return;
        };
        for mapped in &tracked.described.entities {
            self.commanded.remove(&mapped.description.unique_id);
            self.last_on.remove(&mapped.description.unique_id);
        }
        for device in tracked.described.devices.iter().rev() {
            if let Err(e) = ctx.remove_device(device.unique_id.clone()).await {
                tracing::warn!(device = %device.unique_id, error = %e, "couldn't remove a device");
            }
        }
    }

    /// What's listed as not read yet, and the health, when either changed.
    async fn publish(&mut self, ctx: &ProtocolContext) {
        let unmodeled: Vec<_> = self
            .nodes
            .values()
            .flat_map(|tracked| tracked.described.unmodeled.iter().cloned())
            .collect();
        if unmodeled != self.unmodeled {
            ctx.set_unmodeled(unmodeled.clone()).await;
            self.unmodeled = unmodeled;
        }
        let unreachable = self.nodes.values().filter(|t| !t.node.available).count();
        let health = match unreachable {
            0 => Health::Running,
            1 if self.nodes.len() == 1 => {
                Health::Degraded("the Matter device isn't answering".to_owned())
            }
            _ => Health::Degraded(format!(
                "{unreachable} of {} Matter devices aren't answering",
                self.nodes.len()
            )),
        };
        if self.health.as_ref() != Some(&health) {
            ctx.set_health(health.clone()).await;
            self.health = Some(health);
        }
    }

    /// Adds a device from its pairing code, in a task of its own; [`Home::finish`] answers.
    fn add_device(&mut self, action: IncomingAction, client: &Client, done: &mpsc::Sender<Done>) {
        if action.action_id != ADD_DEVICE {
            let id = action.action_id.clone();
            action.reply(Err(format!("no `{id}` action")));
            return;
        }
        // A device shared from another app is on the network already. A new one may still
        // have to join it, over Bluetooth when that's on.
        let new = match action.way.as_deref() {
            Some("new") => true,
            Some("shared") | None => false,
            Some(other) => {
                let other = other.to_owned();
                action.reply(Err(format!("no `{other}` way to add a device")));
                return;
            }
        };
        let code = match action.field("code").map(pairing_code) {
            Some(Ok(code)) => code,
            Some(Err(why)) => {
                action.reply(Err(why));
                return;
            }
            None => {
                action.reply(Err("Type the device's pairing code.".to_owned()));
                return;
            }
        };
        let bluetooth = new && self.bluetooth;
        if bluetooth && self.bluetooth_broken {
            action.reply(Err(BLUETOOTH_BROKEN.to_owned()));
            return;
        }
        if self.adding {
            action.reply(Err(
                "A device is being added already. Wait for it to finish, then add the next."
                    .to_owned(),
            ));
            return;
        }
        self.adding = true;
        let client = client.clone();
        let done = done.clone();
        tokio::spawn(async move {
            let added = client
                .commission(&code, bluetooth)
                .await
                .map_err(|e| commission_failed(&e, bluetooth));
            if let Err(mpsc::error::SendError(Done::Added(action, added))) =
                done.send(Done::Added(action, added)).await
            {
                // The loop has ended; answer anyway.
                action.reply(added.map(|_| ()));
            }
        });
    }

    /// Takes Irori's fabric off a node, in a task of its own; [`Home::finish`] answers.
    async fn unpair(
        &mut self,
        unpair: IncomingUnpair,
        client: &Client,
        done: &mpsc::Sender<Done>,
        ctx: &ProtocolContext,
    ) {
        let found = self
            .nodes
            .values()
            .find(|tracked| map::belongs_to(&unpair.unique_id, self.fabric, tracked.node.id));
        let Some(tracked) = found else {
            // Not a device the server has: nothing to take off any network.
            let _ = ctx.remove_device(unpair.unique_id.clone()).await;
            unpair.reply(Ok(()));
            return;
        };
        let node = tracked.node.id;
        if unpair.unique_id != map::node_device(self.fabric, node) {
            let bridge = tracked
                .described
                .devices
                .first()
                .map_or("its bridge", |device| device.name.as_str());
            unpair.reply(Err(format!(
                "This device is reached through {bridge}. Remove it in the bridge's own app, \
                 or remove {bridge} to take all of its devices off Irori."
            )));
            return;
        }
        if !tracked.node.available && !unpair.force {
            unpair.reply(Err(
                "The device isn't answering, so it can't be told to leave Irori. Remove it \
                 anyway to forget it here; it keeps Irori in its list of controllers until it's \
                 reset."
                    .to_owned(),
            ));
            return;
        }
        if !self.removing.insert(node) {
            unpair.reply(Err("It's being removed already.".to_owned()));
            return;
        }
        let client = client.clone();
        let done = done.clone();
        tokio::spawn(async move {
            let removed = client
                .remove(node)
                .await
                .map_err(|e| format!("The Matter server couldn't remove it: {e}"));
            if let Err(mpsc::error::SendError(Done::Removed(unpair, _, removed))) =
                done.send(Done::Removed(unpair, node, removed)).await
            {
                unpair.reply(removed);
            }
        });
    }

    /// A command's task finished: the node it added or removed is told to Irori first, so
    /// it's there (or gone) by the time the answer arrives.
    async fn finish(&mut self, done: Done, ctx: &ProtocolContext) {
        match done {
            Done::Added(action, added) => {
                self.adding = false;
                match added {
                    Ok(node) => {
                        tracing::info!(node = node.id, "added a Matter device");
                        self.apply(node, ctx, false).await;
                        action.reply(Ok(()));
                    }
                    Err(why) => action.reply(Err(why)),
                }
            }
            Done::Removed(unpair, node, removed) => {
                self.removing.remove(&node);
                if removed.is_ok() {
                    self.forget(node, ctx).await;
                }
                unpair.reply(removed);
            }
            Done::Commanded(call, done) => {
                let unique_id = &call.call.unique_id;
                let ours = self
                    .commanded
                    .get(unique_id)
                    .is_some_and(|commanded| commanded.context == call.call.context.id);
                if ours {
                    if done.is_ok() {
                        if let Some(commanded) = self.commanded.get_mut(unique_id) {
                            commanded.taken = Some(Instant::now());
                        }
                    } else {
                        // Nothing it reports now is this command's doing.
                        self.commanded.remove(unique_id);
                    }
                }
                call.reply(done);
            }
            Done::Checked(node, call, found) => {
                self.checking.remove(&node);
                match found {
                    Ok(found) => {
                        let changed = match found {
                            Some(update) => self.newer.insert(node, update.clone()) != Some(update),
                            None => self.newer.remove(&node).is_some(),
                        };
                        if changed && let Some(tracked) = self.nodes.get(&node) {
                            let again = tracked.node.clone();
                            self.apply(again, ctx, false).await;
                        }
                        if let Some(call) = call {
                            call.reply(Ok(()));
                        }
                    }
                    Err(why) => {
                        tracing::debug!(node, %why, "couldn't look for newer firmware");
                        if let Some(call) = call {
                            call.reply(Err(ServiceError::failed(why)));
                        }
                    }
                }
            }
        }
        self.publish(ctx).await;
    }

    /// Carries out a service call: the node's commands, sent in order from a task of its own,
    /// or, for its firmware, the Matter server's own look and install.
    fn call(&mut self, call: IncomingCall, client: &Client, done: &mpsc::Sender<Done>) {
        let unique_id = call.call.unique_id.clone();
        let found = self.nodes.values().find_map(|tracked| {
            tracked
                .described
                .entities
                .iter()
                .find(|mapped| mapped.description.unique_id == unique_id)
                .map(|mapped| (tracked, mapped))
        });
        let Some((tracked, mapped)) = found else {
            call.reply(Err(ServiceError::failed(
                "Matter doesn't have this entity any more",
            )));
            return;
        };
        let node = tracked.node.id;
        if !tracked.node.available {
            call.reply(Err(ServiceError::unavailable(
                "The Matter device isn't answering",
            )));
            return;
        }
        if mapped.reading == Reading::Update {
            let installing = map::update::installing(&tracked.node);
            let service = call.call.service.clone();
            match service {
                Service::UpdateCheck => self.check_updates(Some((node, call)), client, done),
                Service::UpdateInstall => {
                    let Some(version) = self.newer.get(&node).map(|update| update.version) else {
                        call.reply(Err(ServiceError::failed(
                            "There's no newer firmware for it to install",
                        )));
                        return;
                    };
                    if installing {
                        call.reply(Err(ServiceError::failed(
                            "It's installing firmware already",
                        )));
                        return;
                    }
                    let client = client.clone();
                    tokio::spawn(async move {
                        let installed = client.install_update(node, version).await.map_err(|e| {
                            ServiceError::failed(format!(
                                "The Matter server couldn't start the update: {e}"
                            ))
                        });
                        call.reply(installed);
                    });
                }
                other => call.reply(Err(ServiceError::failed(format!(
                    "firmware doesn't take {}",
                    other.name()
                )))),
            }
            return;
        }
        let commands = match map::command(
            &tracked.node,
            mapped,
            &call.call.service,
            self.last_on.get(&unique_id).copied(),
        ) {
            Ok(commands) => commands,
            Err(why) => {
                call.reply(Err(ServiceError::failed(why)));
                return;
            }
        };
        // A newer command replaces an older one: what's reported from now on is its doing.
        self.commanded.insert(
            unique_id,
            Commanded {
                context: call.call.context.id.clone(),
                sent: Instant::now(),
                taken: None,
            },
        );
        let client = client.clone();
        let done = done.clone();
        tokio::spawn(async move {
            let mut sent = Ok(());
            for command in &commands {
                if let Err(e) = client.send(node, command).await {
                    sent = Err(command_failed(&e));
                    break;
                }
            }
            if let Err(mpsc::error::SendError(Done::Commanded(call, sent))) =
                done.send(Done::Commanded(call, sent)).await
            {
                call.reply(sent);
            }
        });
    }

    /// Asks the server what newer firmware it knows of: for one node, for an `update.check`,
    /// or for every node that can take firmware. One node at a time, from a task of its own.
    fn check_updates(
        &mut self,
        asked: Option<(NodeId, IncomingCall)>,
        client: &Client,
        done: &mpsc::Sender<Done>,
    ) {
        let mut nodes: Vec<(NodeId, Option<IncomingCall>)> = match asked {
            Some((node, call)) => vec![(node, Some(call))],
            None => self
                .nodes
                .values()
                .filter(|tracked| tracked.node.available)
                .filter(|tracked| {
                    tracked
                        .described
                        .entities
                        .iter()
                        .any(|mapped| mapped.reading == Reading::Update)
                })
                .map(|tracked| (tracked.node.id, None))
                .collect(),
        };
        nodes.retain(|(node, call)| call.is_some() || !self.checking.contains(node));
        if nodes.is_empty() {
            return;
        }
        self.checking.extend(nodes.iter().map(|(node, _)| *node));
        let client = client.clone();
        let done = done.clone();
        tokio::spawn(async move {
            for (node, call) in nodes {
                let found = client
                    .check_update(node)
                    .await
                    .map_err(|e| format!("The Matter server couldn't look for firmware: {e}"));
                if let Err(mpsc::error::SendError(Done::Checked(_, Some(call), found))) =
                    done.send(Done::Checked(node, call, found)).await
                {
                    call.reply(found.map(|_| ()).map_err(ServiceError::failed));
                }
            }
        });
    }
}

/// The context of the command a report on `entity` is the result of: its latest, while it's
/// carried out and for [`SETTLING`] after the device took it. Past that, the entity's reports
/// are its own again.
fn caused_by(
    commanded: &mut BTreeMap<UniqueId, Commanded>,
    entity: &UniqueId,
) -> Option<ContextId> {
    let command = commanded.get(entity)?;
    let settling = match command.taken {
        Some(taken) => taken.elapsed() < SETTLING,
        None => command.sent.elapsed() < CARRYING_OUT,
    };
    if settling {
        Some(command.context.clone())
    } else {
        commanded.remove(entity);
        None
    }
}

/// A device that didn't take a command, in words.
fn command_failed(error: &CommandError) -> ServiceError {
    if error.reason.as_deref() == Some(client::TIMED_OUT) {
        return ServiceError::unavailable("The Matter device didn't answer in time");
    }
    ServiceError::failed(format!("The Matter device didn't do it: {}", error.details))
}

/// The code as the server takes it: a QR code's text as it is, a manual code's digits
/// without the dashes and spaces it's printed with.
///
/// Checked here, not by the server: a manual code's last digit is a check digit, so a typo is
/// refused at once instead of after the network has been searched for a device that isn't
/// there.
fn pairing_code(input: &str) -> Result<String, String> {
    const NOT_A_CODE: &str = "That isn't a Matter pairing code. It's the 11 digits under the \
                              device's QR code (dashes are fine), or the code the app you're \
                              sharing it from shows.";
    let input = input.trim();
    if let Some(payload) = input
        .get(..3)
        .filter(|prefix| prefix.eq_ignore_ascii_case("MT:"))
        .map(|_| &input[3..])
    {
        // A QR code's text: base-38, upper case.
        let fine = !payload.is_empty()
            && payload
                .chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase() || matches!(c, '-' | '.'));
        return if fine {
            Ok(format!("MT:{payload}"))
        } else {
            Err(NOT_A_CODE.to_owned())
        };
    }
    let digits: String = input
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '.'))
        .collect();
    let fine = matches!(digits.len(), 11 | 21)
        && digits.chars().all(|c| c.is_ascii_digit())
        && verhoeff_valid(&digits);
    if fine {
        Ok(digits)
    } else {
        Err(NOT_A_CODE.to_owned())
    }
}

/// Whether a run of digits ends in its own Verhoeff check digit, as a Matter manual pairing
/// code does.
fn verhoeff_valid(digits: &str) -> bool {
    const D: [[u8; 10]; 10] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
        [1, 2, 3, 4, 0, 6, 7, 8, 9, 5],
        [2, 3, 4, 0, 1, 7, 8, 9, 5, 6],
        [3, 4, 0, 1, 2, 8, 9, 5, 6, 7],
        [4, 0, 1, 2, 3, 9, 5, 6, 7, 8],
        [5, 9, 8, 7, 6, 0, 4, 3, 2, 1],
        [6, 5, 9, 8, 7, 1, 0, 4, 3, 2],
        [7, 6, 5, 9, 8, 2, 1, 0, 4, 3],
        [8, 7, 6, 5, 9, 3, 2, 1, 0, 4],
        [9, 8, 7, 6, 5, 4, 3, 2, 1, 0],
    ];
    const P: [[u8; 10]; 8] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
        [1, 5, 7, 6, 2, 8, 3, 0, 9, 4],
        [5, 8, 0, 3, 7, 9, 6, 1, 4, 2],
        [8, 9, 1, 6, 0, 4, 3, 5, 2, 7],
        [9, 4, 5, 3, 1, 2, 6, 8, 7, 0],
        [4, 2, 8, 6, 5, 7, 3, 9, 0, 1],
        [2, 7, 9, 3, 8, 0, 6, 4, 1, 5],
        [7, 0, 4, 6, 9, 1, 3, 2, 5, 8],
    ];
    let mut check = 0_u8;
    for (at, digit) in digits.bytes().rev().enumerate() {
        let digit = usize::from(digit - b'0');
        check = D[usize::from(check)][usize::from(P[at % 8][digit])];
    }
    check == 0
}

/// Bluetooth is on in the settings, and the Matter server couldn't use it.
const BLUETOOTH_BROKEN: &str = "Bluetooth is on in Matter's settings, but the Matter server \
    couldn't use this machine's Bluetooth adapter. On Linux it needs permission to: run `sudo \
    setcap cap_net_raw,cap_net_admin+eip` on the Node.js it installed, or check \"Bluetooth \
    adapter\" in the settings. On a Mac, allow Bluetooth for the app Irori runs from in \
    System Settings, Privacy & Security. A device that's wired, or on the network \
    already, can be added with Bluetooth off.";

/// What the server says when a device needs a network to join and it has none to give.
const NO_NETWORK: &str = "No Wi-Fi/Thread network credentials are configured";

fn commission_failed(error: &CommandError, bluetooth: bool) -> String {
    if error.details.contains(NO_NETWORK) {
        return "This device needs a network to join, and Irori has none to give it. For a Wi-Fi \
                device, fill in the Wi-Fi network's name and password and try again. A Thread \
                device can't be added this way yet: set it up in Google Home or Apple Home \
                first, then share it with Irori."
            .to_owned();
    }
    // Either the server gave up looking, or it was still looking when the wait ran out.
    if matches!(
        error.reason.as_deref(),
        Some("not-discovered" | client::TIMED_OUT)
    ) {
        return if bluetooth {
            "No Matter device answered to that code, on the network or over Bluetooth. Check \
             that it's close to Irori and in pairing mode, and that the code is the one on it. \
             A device that joins Thread rather than Wi-Fi can't be added this way yet: set it \
             up in Google Home or Apple Home first, then share it with Irori."
                .to_owned()
        } else {
            "No Matter device on this network answered to that code. Check that it's on the \
             same network as Irori and that the code is fresh: a shared code lasts about 15 \
             minutes. A new device that isn't on a network yet joins over Bluetooth: choose \
             \"It's new, or reset\" and turn Bluetooth on."
                .to_owned()
        };
    }
    if error.details.contains("test/development certificate") {
        return "This device uses a development certificate, not one from its maker. If you \
                made it or trust it, turn on \"Allow test devices\" in Matter's settings and \
                try again."
            .to_owned();
    }
    match error.code {
        Some(_) => format!("The Matter server couldn't add it: {}", error.details),
        None => error.details.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use futures_util::{SinkExt, StreamExt};
    use irori_protocol::host;
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    use super::*;

    const FABRIC: u64 = 10_256_683_378_676_567_378;

    /// A Matter server on loopback that the test answers by hand: every command it gets
    /// arrives on `commands`, and whatever goes into `send` goes down the socket.
    struct FakeServer {
        url: String,
        commands: mpsc::UnboundedReceiver<(Value, String, Value)>,
        send: mpsc::UnboundedSender<Value>,
    }

    impl FakeServer {
        async fn start() -> Self {
            Self::start_saying(json!({})).await
        }

        /// The same, with more in the `server_info` it greets with.
        async fn start_saying(more: Value) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
            let url = format!("ws://{}/ws", listener.local_addr().expect("an address"));
            let (commands_tx, commands) = mpsc::unbounded_channel();
            let (send, mut outgoing) = mpsc::unbounded_channel::<Value>();
            tokio::spawn(async move {
                let (stream, _) = listener.accept().await.expect("a client");
                let socket = tokio_tungstenite::accept_async(stream)
                    .await
                    .expect("a handshake");
                let (mut sink, mut stream) = socket.split();
                let mut hello = json!({
                    "fabric_id": 1,
                    "compressed_fabric_id": FABRIC,
                    "schema_version": 13,
                    "sdk_version": "matter-server/1.4.0 (matter.js/0.17.9)",
                });
                if let (Some(hello), Some(more)) = (hello.as_object_mut(), more.as_object()) {
                    hello.extend(more.clone());
                }
                sink.send(Message::text(hello.to_string()))
                    .await
                    .expect("sends");
                loop {
                    tokio::select! {
                        message = stream.next() => {
                            let Some(Ok(Message::Text(text))) = message else { break };
                            let asked: Value = serde_json::from_str(text.as_str()).expect("JSON");
                            let _ = commands_tx.send((
                                asked["message_id"].clone(),
                                asked["command"].as_str().unwrap_or_default().to_owned(),
                                asked["args"].clone(),
                            ));
                        }
                        Some(value) = outgoing.recv() => {
                            if sink.send(Message::text(value.to_string())).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
            Self {
                url,
                commands,
                send,
            }
        }

        /// The next command, which must be `command`, with its id and arguments.
        async fn next(&mut self, command: &str) -> (Value, Value) {
            let (id, name, args) =
                tokio::time::timeout(Duration::from_secs(5), self.commands.recv())
                    .await
                    .expect("a command in time")
                    .expect("the client is connected");
            assert_eq!(name, command);
            (id, args)
        }

        fn answer(&self, id: &Value, result: Value) {
            self.send
                .send(json!({"message_id": id, "result": result}))
                .expect("the server runs");
        }

        fn refuse(&self, id: &Value, code: i64, reason: Option<&str>, details: &str) {
            let mut refusal = json!({"message_id": id, "error_code": code, "details": details});
            if let Some(reason) = reason {
                refusal["reason"] = json!(reason);
            }
            self.send.send(refusal).expect("the server runs");
        }

        fn event(&self, event: &str, data: Value) {
            self.send
                .send(json!({"event": event, "data": data}))
                .expect("the server runs");
        }
    }

    fn lab(name: &str) -> Value {
        serde_json::from_str(match name {
            "plug" => include_str!("../tests/fixtures/lab_plug.json"),
            "occupancy" => include_str!("../tests/fixtures/lab_occupancy.json"),
            "bridge" => include_str!("../tests/fixtures/mock_composed_bridge.json"),
            "lock" => include_str!("../tests/fixtures/lab_lock.json"),
            "light" => include_str!("../tests/fixtures/lab_light.json"),
            "thermostat" => include_str!("../tests/fixtures/lab_thermostat.json"),
            "mock_door_lock" => include_str!("../tests/fixtures/mock_door_lock.json"),
            other => panic!("no fixture {other}"),
        })
        .expect("JSON")
    }

    /// What the core was told, in order, as short lines: `device …`, `entity …`, `remove …`,
    /// `available …`, `unavailable …`, `health …`, `actions …`.
    #[derive(Debug, Clone, Default)]
    struct Told(Arc<Mutex<Vec<String>>>);

    impl Told {
        fn lines(&self) -> Vec<String> {
            self.0.lock().expect("not poisoned").clone()
        }

        async fn until(&self, line: &str) {
            let waited = tokio::time::timeout(Duration::from_secs(5), async {
                while !self.lines().iter().any(|l| l == line) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await;
            assert!(waited.is_ok(), "never told `{line}`: {:#?}", self.lines());
        }
    }

    fn record(mut ops: mpsc::Receiver<host::Op>) -> Told {
        let told = Told::default();
        let lines = told.clone();
        tokio::spawn(async move {
            while let Some(op) = ops.recv().await {
                let line = match op {
                    host::Op::DescribeDevice(device, reply) => {
                        let _ = reply.send(Ok(()));
                        format!("device {}", device.unique_id)
                    }
                    host::Op::DescribeEntity(entity, reply) => {
                        let _ = reply.send(Ok(()));
                        format!("entity {}", entity.unique_id)
                    }
                    host::Op::RemoveDevice(id, reply) | host::Op::RemoveEntity(id, reply) => {
                        let _ = reply.send(Ok(()));
                        format!("remove {id}")
                    }
                    host::Op::SetAvailability(target, availability, reply) => {
                        let _ = reply.send(Ok(()));
                        let AvailabilityTarget::Device(id) = target else {
                            panic!("by device");
                        };
                        match availability {
                            Availability::Available => format!("available {id}"),
                            Availability::Unavailable => format!("unavailable {id}"),
                        }
                    }
                    host::Op::SetHealth(Health::Running) => "health running".to_owned(),
                    host::Op::SetHealth(Health::Degraded(why)) => format!("health {why}"),
                    host::Op::SetAvailableActions(actions) => {
                        format!("actions {}", actions.join(","))
                    }
                    host::Op::SetUnmodeled(unmodeled) => format!("unmodeled {}", unmodeled.len()),
                    host::Op::Load(_, reply) => {
                        let _ = reply.send(Ok(None));
                        continue;
                    }
                    host::Op::Store(_, _, reply) => {
                        let _ = reply.send(Ok(()));
                        continue;
                    }
                    host::Op::SetWaiting(_) | host::Op::SetActionOpen(..) => continue,
                };
                lines.0.lock().expect("not poisoned").push(line);
            }
        });
        told
    }

    /// The extension connected to a fake server that already has `nodes`.
    struct Running {
        server: FakeServer,
        host: host::HostEnd,
        told: Told,
        driving: tokio::task::JoinHandle<Ended>,
    }

    async fn running(nodes: Vec<Value>) -> Running {
        let mut running = connected(Settings::default()).await;
        let (id, _) = running.server.next("start_listening").await;
        running.server.answer(&id, Value::Array(nodes));
        running.told.until("actions add_device").await;
        running
    }

    /// The extension connected with `settings`, before the server has answered anything.
    async fn connected(settings: Settings) -> Running {
        connected_to(FakeServer::start().await, settings).await
    }

    async fn connected_to(server: FakeServer, settings: Settings) -> Running {
        let (mut ctx, mut host) = host::connect();
        let told = record(std::mem::replace(&mut host.ops, mpsc::channel(1).1));
        let connection = client::connect(&server.url).await.expect("connects");
        let driving = tokio::spawn(async move {
            drive(connection, &settings, &mut ctx, std::future::pending()).await
        });
        Running {
            server,
            host,
            told,
            driving,
        }
    }

    /// Adding a device the way a person picked, with the code they typed.
    fn add(
        way: &str,
        code: &str,
    ) -> (
        IncomingAction,
        tokio::sync::oneshot::Receiver<Result<(), String>>,
    ) {
        host::incoming_action_with(
            ADD_DEVICE.to_owned(),
            irori_protocol::Filled {
                way: Some(way.to_owned()),
                fields: [("code".to_owned(), code.to_owned())].into(),
            },
        )
    }

    fn settings(given: Value) -> Settings {
        serde_json::from_value(given).expect("settings")
    }

    const ASKED_BY: &str = "01K5B2Q9A1B2C3D4E5F6G7H8J9";

    /// Calls `service` on the entity, as a person would; the answer comes on the receiver.
    async fn call(
        running: &Running,
        unique_id: &str,
        service: Service,
    ) -> tokio::sync::oneshot::Receiver<Result<(), ServiceError>> {
        let call = irori_types::ServiceCall {
            unique_id: UniqueId::try_from(unique_id.to_owned()).expect("valid"),
            service,
            context: irori_types::Context {
                id: ContextId::try_from(ASKED_BY).expect("valid"),
                parent_id: None,
                origin: irori_types::Origin::System,
            },
        };
        let (incoming, answer) = host::incoming_call(call);
        running.host.calls.send(incoming).await.expect("sent");
        answer
    }

    async fn answered(
        answer: tokio::sync::oneshot::Receiver<Result<(), ServiceError>>,
    ) -> Result<(), ServiceError> {
        tokio::time::timeout(Duration::from_secs(5), answer)
            .await
            .expect("in time")
            .expect("answered")
    }

    fn device(node: u64) -> String {
        map::node_device(FABRIC, node).to_string()
    }

    async fn reported(host: &host::HostEnd, unique_id: &str) -> Option<State> {
        report(host, unique_id).await.state
    }

    async fn report(host: &host::HostEnd, unique_id: &str) -> StateReport {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(report) = host
                    .reports
                    .drain()
                    .into_iter()
                    .rfind(|report| report.unique_id.as_str() == unique_id)
                {
                    return report;
                }
                host.reports.ready().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no report for {unique_id}"))
    }

    #[tokio::test]
    async fn the_servers_nodes_become_devices_with_their_readings() {
        let running = running(vec![lab("plug"), lab("occupancy")]).await;
        let plug = device(3);
        let motion = device(4);
        let lines = running.told.lines();
        for expected in [
            format!("device {plug}"),
            format!("entity {plug}-1-switch"),
            format!("available {plug}"),
            format!("device {motion}"),
            format!("entity {motion}-1-occupancy"),
            "health running".to_owned(),
            "actions add_device".to_owned(),
        ] {
            assert!(lines.contains(&expected), "{expected}: {lines:#?}");
        }
        let at = |line: String| lines.iter().position(|l| *l == line).expect("told");
        assert!(at(format!("device {plug}")) < at(format!("entity {plug}-1-switch")));

        let reports = running.host.reports.drain();
        let switch = reports
            .iter()
            .find(|r| r.unique_id.as_str() == format!("{plug}-1-switch"))
            .expect("the plug's state");
        assert_eq!(
            switch.state,
            Some(State::Switch(irori_types::SwitchState { on: false }))
        );
        assert!(switch.replayed, "what the server knew, not something new");
    }

    #[tokio::test]
    async fn a_changed_attribute_is_reported_and_an_unchanged_one_isnt() {
        let running = running(vec![lab("occupancy")]).await;
        let motion = format!("{}-1-occupancy", device(4));
        running.host.reports.drain();
        running
            .server
            .event("attribute_updated", json!([4, "1/1030/0", 1]));
        assert_eq!(
            reported(&running.host, &motion).await,
            Some(State::BinarySensor(irori_types::BinarySensorState {
                on: true
            }))
        );
        // A reading that doesn't change the state, then one that does: only the second.
        running
            .server
            .event("attribute_updated", json!([4, "1/1030/65533", 5]));
        running
            .server
            .event("attribute_updated", json!([4, "1/1030/0", 0]));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let reports = running.host.reports.drain();
        assert_eq!(reports.len(), 1, "{reports:?}");
        assert!(!reports[0].replayed);
    }

    #[tokio::test]
    async fn a_node_that_stops_answering_is_unavailable_and_says_so() {
        let running = running(vec![lab("plug")]).await;
        let mut gone = lab("plug");
        gone["available"] = json!(false);
        running.server.event("node_updated", gone);
        running
            .told
            .until(&format!("unavailable {}", device(3)))
            .await;
        running
            .told
            .until("health the Matter device isn't answering")
            .await;
    }

    #[tokio::test]
    async fn a_bridged_device_its_bridge_cant_reach_is_unavailable_alone() {
        let running = running(vec![lab("bridge")]).await;
        let bridge = device(99);
        let plug = format!("{bridge}-0011223344556677");
        running.told.until(&format!("available {plug}")).await;
        running
            .server
            .event("attribute_updated", json!([99, "2/57/17", false]));
        running.told.until(&format!("unavailable {plug}")).await;
        assert!(
            !running
                .told
                .lines()
                .contains(&format!("unavailable {bridge}")),
            "the bridge itself still answers"
        );
    }

    #[tokio::test]
    async fn a_pairing_code_adds_the_device_before_the_answer_comes() {
        let mut running = running(vec![]).await;
        let (action, answer) = add("shared", " 3497-011-2332 ");
        running.host.actions.send(action).await.expect("sent");
        let (id, args) = running.server.next("commission_with_code").await;
        assert_eq!(args["code"], "34970112332");
        assert_eq!(args["network_only"], true);
        running.server.answer(&id, lab("plug"));
        let answered = tokio::time::timeout(Duration::from_secs(5), answer)
            .await
            .expect("in time")
            .expect("answered");
        assert_eq!(answered, Ok(()));
        // Described before the answer went back.
        assert!(
            running
                .told
                .lines()
                .contains(&format!("entity {}-1-switch", device(3)))
        );
    }

    #[tokio::test]
    async fn readings_keep_coming_while_a_device_is_being_added() {
        let mut running = running(vec![lab("occupancy")]).await;
        let (action, mut answer) = add("shared", "34970112332");
        running.host.actions.send(action).await.expect("sent");
        let (commissioning, _) = running.server.next("commission_with_code").await;

        running.host.reports.drain();
        running
            .server
            .event("attribute_updated", json!([4, "1/1030/0", 1]));
        assert!(
            reported(&running.host, &format!("{}-1-occupancy", device(4)))
                .await
                .is_some()
        );

        // And a second code meanwhile waits its turn.
        let (second, refused) = add("shared", "34971112335");
        running.host.actions.send(second).await.expect("sent");
        let refused = refused.await.expect("answered");
        assert!(
            refused
                .expect_err("one at a time")
                .contains("being added already")
        );
        assert!(answer.try_recv().is_err(), "the first is still going");

        running.server.refuse(
            &commissioning,
            1,
            Some("not-discovered"),
            "Commission failed",
        );
        let answered = answer.await.expect("answered");
        assert!(answered.expect_err("not found").contains("Bluetooth"));
    }

    #[tokio::test]
    async fn a_code_that_isnt_one_is_refused_without_asking_the_network() {
        let running = running(vec![]).await;
        // The last digit is off by one.
        let (action, answer) = add("shared", "34970112333");
        running.host.actions.send(action).await.expect("sent");
        let answered = answer.await.expect("answered");
        assert!(
            answered
                .expect_err("not a code")
                .contains("isn't a Matter pairing code")
        );
    }

    #[tokio::test]
    async fn a_development_device_needs_test_devices_allowed() {
        let mut running = running(vec![]).await;
        let (action, answer) = add("shared", "34970112332");
        running.host.actions.send(action).await.expect("sent");
        let (id, _) = running.server.next("commission_with_code").await;
        running.server.refuse(
            &id,
            1,
            None,
            "Commission failed: This device uses a test/development certificate. To \
             commission it, enable the \"Test DCL\" option",
        );
        let answered = answer.await.expect("answered");
        assert!(
            answered
                .expect_err("refused")
                .contains("Allow test devices")
        );
    }

    #[tokio::test]
    async fn removing_a_device_takes_irori_off_it_then_out_of_the_home() {
        let mut running = running(vec![lab("plug")]).await;
        let plug = UniqueId::try_from(device(3)).expect("valid");
        let (unpair, answer) = host::incoming_unpair(plug.clone(), false);
        running.host.actions.send(unpair).await.expect("sent");
        let (id, args) = running.server.next("remove_node").await;
        assert_eq!(args, json!({"node_id": 3}));
        running.server.answer(&id, Value::Null);
        assert_eq!(answer.await.expect("answered"), Ok(()));
        assert!(running.told.lines().contains(&format!("remove {plug}")));
        // The server's own word that it's gone changes nothing more.
        running.server.event("node_removed", json!(3));
    }

    #[tokio::test]
    async fn a_bridged_device_or_one_not_answering_isnt_removed_without_asking() {
        let mut bridge = lab("bridge");
        bridge["available"] = json!(false);
        let running = running(vec![bridge]).await;
        let bridged =
            UniqueId::try_from(format!("{}-0011223344556677", device(99))).expect("valid");
        let (unpair, answer) = host::incoming_unpair(bridged, false);
        running.host.actions.send(unpair).await.expect("sent");
        assert!(
            answer
                .await
                .expect("answered")
                .expect_err("through the bridge")
                .contains("reached through Mock Bridge")
        );

        let whole = UniqueId::try_from(device(99)).expect("valid");
        let (unpair, answer) = host::incoming_unpair(whole, false);
        running.host.actions.send(unpair).await.expect("sent");
        assert!(
            answer
                .await
                .expect("answered")
                .expect_err("not answering")
                .contains("isn't answering")
        );
    }

    #[tokio::test]
    async fn a_switch_is_switched_and_the_report_it_causes_says_who_asked() {
        let mut running = running(vec![lab("plug")]).await;
        let switch = format!("{}-1-switch", device(3));
        let _ = report(&running.host, &switch).await;
        let answer = call(&running, &switch, Service::SwitchTurnOn).await;
        let (id, args) = running.server.next("device_command").await;
        assert_eq!(
            args,
            json!({
                "node_id": 3,
                "endpoint_id": 1,
                "cluster_id": 6,
                "command_name": "on",
                "payload": {},
            })
        );
        running.server.answer(&id, Value::Null);
        assert_eq!(answered(answer).await, Ok(()));
        running
            .server
            .event("attribute_updated", json!([3, "1/6/0", true]));
        let caused = report(&running.host, &switch).await;
        assert_eq!(
            caused.state,
            Some(State::Switch(irori_types::SwitchState { on: true }))
        );
        assert_eq!(
            caused.caused_by.as_ref().map(ContextId::as_str),
            Some(ASKED_BY)
        );
    }

    #[tokio::test]
    async fn changes_that_come_together_are_one_report_and_each_names_the_command() {
        let mut running = running(vec![lab("light")]).await;
        let light = format!("{}-1-light", device(1));
        let _ = report(&running.host, &light).await;
        let answer = call(
            &running,
            &light,
            Service::LightTurnOn(irori_types::LightTurnOn {
                brightness: Some(128),
                ..irori_types::LightTurnOn::default()
            }),
        )
        .await;
        let (id, _) = running.server.next("device_command").await;
        running.server.answer(&id, Value::Null);
        assert_eq!(answered(answer).await, Ok(()));
        // On, then the level: as the server sends them, one after the other.
        running
            .server
            .event("attribute_updated", json!([1, "1/6/0", true]));
        running
            .server
            .event("attribute_updated", json!([1, "1/8/0", 128]));
        let mut seen = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                seen.extend(
                    running
                        .host
                        .reports
                        .drain()
                        .into_iter()
                        .filter(|report| report.unique_id.as_str() == light),
                );
                let brightness = seen.last().and_then(|report| match &report.state {
                    Some(State::Light(light)) => light.brightness,
                    _ => None,
                });
                if brightness.is_some() {
                    break;
                }
                running.host.reports.ready().await;
            }
        })
        .await
        .expect("the level is reported");
        // Every step there names who asked, the last one too.
        assert!(
            seen.iter()
                .all(|report| report.caused_by.as_ref().map(ContextId::as_str) == Some(ASKED_BY)),
            "{seen:#?}"
        );
    }

    #[test]
    fn a_command_names_its_reports_until_it_has_settled() {
        let entity = UniqueId::try_from("ABC-1-light".to_owned()).expect("valid");
        let context = ContextId::try_from(ASKED_BY).expect("valid");
        let ago = |seconds| {
            Instant::now()
                .checked_sub(Duration::from_secs(seconds))
                .expect("after boot")
        };
        let mut commanded = BTreeMap::new();
        let mut at = |sent, taken| {
            commanded.insert(
                entity.clone(),
                Commanded {
                    context: context.clone(),
                    sent,
                    taken,
                },
            );
            caused_by(&mut commanded, &entity)
        };
        // Being carried out, and just taken: its doing.
        assert_eq!(at(ago(5), None), Some(context.clone()));
        assert_eq!(at(ago(5), Some(ago(1))), Some(context.clone()));
        // Settled, or a device that never answered: the entity's own again.
        assert_eq!(at(ago(10), Some(ago(5))), None);
        assert_eq!(at(ago(120), None), None);
    }

    #[tokio::test]
    async fn a_lock_is_sent_its_code_in_a_timed_command() {
        let mut running = running(vec![lab("lock")]).await;
        let answer = call(
            &running,
            &format!("{}-1-lock", device(2)),
            Service::LockUnlock(irori_types::LockCode {
                code: Some("1234".to_owned()),
            }),
        )
        .await;
        let (id, args) = running.server.next("device_command").await;
        assert_eq!(args["command_name"], "unlockDoor");
        assert_eq!(args["cluster_id"], 257);
        assert_eq!(args["payload"], json!({"pinCode": "MTIzNA=="}));
        assert_eq!(args["timed_request_timeout_ms"], 10_000);
        // A wrong code, as matter.js says it.
        running.server.refuse(&id, 0, None, "Failure (code 1)");
        let refused = answered(answer).await.expect_err("refused");
        assert!(refused.message.contains("didn't do it"), "{refused:?}");
    }

    #[tokio::test]
    async fn a_thermostat_is_written_and_a_write_the_device_refuses_fails_the_call() {
        let mut running = running(vec![lab("thermostat")]).await;
        let climate = format!("{}-1-climate", device(4));
        let set =
            |hvac_mode| Service::ClimateSetHvacMode(irori_types::ClimateHvacMode { hvac_mode });
        let answer = call(&running, &climate, set(HvacMode::Cool)).await;
        let (id, args) = running.server.next("write_attribute").await;
        assert_eq!(
            args,
            json!({"node_id": 4, "attribute_path": "1/513/28", "value": 3})
        );
        running
            .server
            .answer(&id, json!([{"Path": {"EndpointId": 1}, "Status": 0}]));
        assert_eq!(answered(answer).await, Ok(()));

        let answer = call(&running, &climate, set(HvacMode::Off)).await;
        let (id, _) = running.server.next("write_attribute").await;
        running
            .server
            .answer(&id, json!([{"Path": {"EndpointId": 1}, "Status": 135}]));
        let refused = answered(answer).await.expect_err("refused");
        assert!(refused.message.contains("refused"), "{refused:?}");
    }

    #[tokio::test]
    async fn a_command_to_a_device_not_answering_isnt_sent() {
        let mut node = lab("plug");
        node["available"] = json!(false);
        let mut running = running(vec![node]).await;
        let answer = call(
            &running,
            &format!("{}-1-switch", device(3)),
            Service::SwitchTurnOff,
        )
        .await;
        let refused = answered(answer).await.expect_err("not answering");
        assert!(refused.message.contains("isn't answering"), "{refused:?}");
        assert!(running.server.commands.try_recv().is_err());
    }

    #[tokio::test]
    async fn firmware_the_server_finds_is_offered_and_installed_when_asked() {
        let mut running = running(vec![lab("mock_door_lock")]).await;
        let firmware = format!("{}-0-firmware", device(16));
        let Some(State::Update(before)) = reported(&running.host, &firmware).await else {
            panic!("an update state");
        };
        assert!(!before.available);

        // Nothing newer to install yet.
        let answer = call(&running, &firmware, Service::UpdateInstall).await;
        assert!(answered(answer).await.is_err());

        let answer = call(&running, &firmware, Service::UpdateCheck).await;
        let (id, args) = running.server.next("check_node_update").await;
        assert_eq!(args, json!({"node_id": 16}));
        running.server.answer(
            &id,
            json!({
                "vid": 65521,
                "pid": 32769,
                "software_version": 2,
                "software_version_string": "2.0",
                "release_notes_url": "https://example.com/notes",
            }),
        );
        assert_eq!(answered(answer).await, Ok(()));
        let Some(State::Update(found)) = reported(&running.host, &firmware).await else {
            panic!("an update state");
        };
        assert!(found.available);
        assert_eq!(found.latest_version.as_deref(), Some("2.0"));

        let answer = call(&running, &firmware, Service::UpdateInstall).await;
        let (id, args) = running.server.next("update_node").await;
        assert_eq!(args, json!({"node_id": 16, "software_version": 2}));
        running.server.answer(&id, Value::Null);
        assert_eq!(answered(answer).await, Ok(()));
    }

    #[tokio::test]
    async fn the_networks_a_new_device_joins_are_given_to_the_server() {
        let mut running = connected(settings(json!({
            "bluetooth": true,
            "wifi_ssid": "Home",
            "wifi_password": "hunter22",
            "thread_dataset": "0e080000000000010000",
        })))
        .await;
        let (id, _) = running.server.next("start_listening").await;
        running.server.answer(&id, json!([]));
        let (id, args) = running.server.next("set_wifi_credentials").await;
        assert_eq!(args, json!({"ssid": "Home", "credentials": "hunter22"}));
        running.server.answer(&id, Value::Null);
        let (id, args) = running.server.next("set_thread_dataset").await;
        assert_eq!(args, json!({"dataset": "0e080000000000010000"}));
        // A refusal doesn't stop devices being added.
        running.server.refuse(&id, 0, None, "not a dataset");
        running.told.until("actions add_device").await;

        // With Bluetooth on, a new device's code is looked for over Bluetooth too.
        let (action, _answer) = add("new", "3497-011-2332");
        running.host.actions.send(action).await.expect("sent");
        let (_, args) = running.server.next("commission_with_code").await;
        assert_eq!(args["network_only"], false);
    }

    #[tokio::test]
    async fn a_shared_device_is_looked_for_on_the_network_only() {
        let mut running = connected(settings(json!({"bluetooth": true}))).await;
        let (id, _) = running.server.next("start_listening").await;
        running.server.answer(&id, json!([]));
        running.told.until("actions add_device").await;
        let (action, _answer) = add("shared", "3497-011-2332");
        running.host.actions.send(action).await.expect("sent");
        let (_, args) = running.server.next("commission_with_code").await;
        assert_eq!(args["network_only"], true, "it's on the network already");
    }

    #[tokio::test]
    async fn bluetooth_the_server_cant_use_is_said_before_trying() {
        let server = FakeServer::start_saying(json!({"bluetooth_enabled": false})).await;
        let mut running = connected_to(server, settings(json!({"bluetooth": true}))).await;
        let (id, _) = running.server.next("start_listening").await;
        running.server.answer(&id, json!([]));
        running.told.until("actions add_device").await;

        let (action, answer) = add("new", "3497-011-2332");
        running.host.actions.send(action).await.expect("sent");
        let refused = answer.await.expect("answered").expect_err("no adapter");
        assert!(refused.contains("Bluetooth adapter"), "{refused}");

        // A shared device doesn't need Bluetooth.
        let (action, _answer) = add("shared", "3497-011-2332");
        running.host.actions.send(action).await.expect("sent");
        running.server.next("commission_with_code").await;
    }

    #[tokio::test]
    async fn a_device_with_no_network_to_join_says_which_to_give() {
        let mut running = running(vec![]).await;
        let (action, answer) = add("new", "34970112332");
        running.host.actions.send(action).await.expect("sent");
        let (id, _) = running.server.next("commission_with_code").await;
        running.server.refuse(
            &id,
            1,
            None,
            "Commission failed: No Wi-Fi/Thread network credentials are configured for \
             commissioning",
        );
        let refused = answer.await.expect("answered").expect_err("no network");
        assert!(
            refused.contains("Wi-Fi network's name and password"),
            "{refused}"
        );
        assert!(refused.contains("Thread"), "{refused}");
    }

    #[test]
    fn every_setting_a_way_changes_is_one_of_the_settings() {
        let manifest =
            irori_protocol::parse_manifest(include_str!("../irori-extension.toml")).expect("valid");
        let schema: Value =
            serde_json::from_str(include_str!("../config.schema.json")).expect("JSON");
        let ways = &manifest.contributes.protocol[0].actions[0].ways;
        assert_eq!(ways.len(), 2);
        for way in ways {
            for setting in &way.settings {
                assert!(
                    schema["properties"].get(setting).is_some(),
                    "`{}` changes `{setting}`, which isn't a setting",
                    way.id
                );
            }
        }
    }

    #[tokio::test]
    async fn the_server_going_away_ends_the_run() {
        let running = running(vec![]).await;
        running.server.event("server_shutdown", json!({}));
        let ended = tokio::time::timeout(Duration::from_secs(5), running.driving)
            .await
            .expect("in time")
            .expect("ran");
        assert!(matches!(ended, Ended::Failed(why) if why.contains("shut down")));
    }

    #[tokio::test]
    async fn told_to_stop_it_stops() {
        let running = running(vec![]).await;
        running.host.stop.send(true).expect("sent");
        let ended = tokio::time::timeout(Duration::from_secs(5), running.driving)
            .await
            .expect("in time")
            .expect("ran");
        assert!(matches!(ended, Ended::Stopped));
    }

    #[test]
    fn a_device_not_found_in_time_is_said_as_one_not_found() {
        let refused = |code, reason: Option<&str>, details: &str| {
            commission_failed(
                &CommandError {
                    code,
                    reason: reason.map(str::to_owned),
                    details: details.to_owned(),
                },
                false,
            )
        };
        let not_found = refused(Some(1), Some("not-discovered"), "Commission failed");
        assert!(not_found.contains("No Matter device on this network answered"));
        assert_eq!(
            refused(None, Some(client::TIMED_OUT), "didn't answer in time"),
            not_found,
            "1.4.0 keeps looking until the wait runs out"
        );
        assert_eq!(
            refused(Some(1), None, "Commission failed: PASE failed"),
            "The Matter server couldn't add it: Commission failed: PASE failed"
        );
    }

    #[test]
    fn a_pairing_code_is_taken_as_printed() {
        let code = |input: &str| pairing_code(input).ok();
        assert_eq!(code(" 3497-011-2332 ").as_deref(), Some("34970112332"));
        assert_eq!(code("3497 011 2332").as_deref(), Some("34970112332"));
        // The dev lab's.
        assert_eq!(code("34971112335").as_deref(), Some("34971112335"));
        assert_eq!(code("34971212334").as_deref(), Some("34971212334"));
        assert_eq!(
            code("MT:Y.K9042C00KA0648G00").as_deref(),
            Some("MT:Y.K9042C00KA0648G00"),
            "a QR code's text is kept whole"
        );
        for typo in [
            "34970112333",
            "3497011233",
            "nonsense",
            "",
            "MT:",
            "MT:lower",
        ] {
            assert_eq!(code(typo), None, "{typo}");
        }
    }
}
