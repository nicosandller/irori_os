//! Devices running [ESPHome](https://esphome.io) firmware, over ESPHome's native API.
//!
//! ESPHome devices announce themselves on the local network, so this integration needs no
//! setting up: it listens for them, connects, and everything they have shows up in the home.
//! Each device gets its own task ([`node`]); this module owns the loop that talks to the core,
//! because the core's handle can't be shared.
//!
//! **Encrypted devices** need their key, kept in `secrets.toml` ([`settings`]). A device that
//! wants one and has none, or whose key doesn't match, isn't retried: it's listed as waiting
//! (`docs/specs/integrations.md` §6.6), where the UI offers to take the key, and a new key
//! restarts the integration.

#[cfg(test)]
mod fake_device;
mod map;
mod node;
mod settings;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use irori_integration::types::{Availability, Name, SecretRequest, UniqueId, Waiting};
use irori_integration::{
    AvailabilityTarget, Health, IncomingCall, Integration, IntegrationContext, IntegrationError,
    ServiceError,
};

use crate::settings::{Mac, Settings};
use tokio::sync::mpsc;

/// The ESPHome integration.
#[derive(Debug)]
pub struct Esphome;

impl Integration for Esphome {
    // Discovery finds the devices; the one thing a person has to supply is the key for a device
    // that encrypts its connection.
    type Config = Settings;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");
    const ICON: Option<&'static str> = Some(include_str!("../icon.svg"));

    async fn run(settings: Settings, ctx: IntegrationContext) -> Result<(), IntegrationError> {
        run(settings, ctx).await
    }
}

/// How many device events can be waiting before their tasks slow down. Each is one connection's
/// worth of news; the run loop drains it far faster than devices produce it.
const EVENT_QUEUE: usize = 256;

/// Commands waiting for one device. Small on purpose: if a device is this far behind, the call
/// times out in the core anyway (10 s).
const COMMAND_QUEUE: usize = 8;

/// How long to let device tasks finish after being told to stop. The contract allows 5 s.
const STOP_GRACE: Duration = Duration::from_secs(2);

/// A device task: the connection to one address, and the device it turned out to be.
#[derive(Debug)]
struct Task {
    /// Which connection this is. Everything it says carries the same identity, so a message
    /// from a connection that has since been replaced can be told apart from the one speaking
    /// now — even when the replacement is at the same address.
    connection: node::Connection,
    commands: mpsc::Sender<IncomingCall>,
    /// Kept so shutdown can stop it, rather than leaving it running detached with a socket open.
    handle: tokio::task::JoinHandle<()>,
}

/// A device the home knows about, and the task currently speaking for it.
#[derive(Debug)]
struct Node {
    /// Which connection speaks for this device. A device that moves is taken over by its new
    /// connection, and anything the old one says afterwards is ignored.
    connection: node::Connection,
    commands: mpsc::Sender<IncomingCall>,
    /// Its entities, so calls can be routed and so a device that comes back with fewer entities
    /// can have the old ones removed.
    entities: Vec<UniqueId>,
    /// Whether it's connected right now. Commands for a device that's away are refused rather
    /// than queued: by the time it reconnects, the caller has long since timed out, and acting
    /// on a minutes-old command would be worse than not acting at all.
    online: bool,
}

/// Everything the run loop keeps track of.
#[derive(Debug, Default)]
struct Devices {
    /// One per address being talked to, whether or not it has introduced itself yet.
    tasks: BTreeMap<SocketAddr, Task>,
    /// One per device that has introduced itself.
    nodes: BTreeMap<UniqueId, Node>,
    /// Addresses that didn't answer, and why. Cleared when one finally does, so "couldn't be
    /// reached" counts devices rather than attempts.
    unreachable: BTreeMap<SocketAddr, String>,
    /// Handed out to each new connection; never reused.
    connections: u64,
    /// What each address announced about itself, so a device that turns out to be locked can be
    /// named by what it called itself rather than by an address.
    announced: BTreeMap<SocketAddr, Announced>,
    /// Devices that need a key before they can be used, by MAC. Not retried in this run: the
    /// settings can't change without the integration being restarted.
    waiting: BTreeMap<Mac, Waiting>,
}

/// What a device says about itself on the network, before anyone connects to it.
#[derive(Debug, Clone)]
struct Announced {
    address: SocketAddr,
    /// From mDNS's `mac` record. Firmware old enough not to send one can still be used unless it
    /// needs a key, which has nothing to be attached to without it.
    mac: Option<Mac>,
    /// Its `friendly_name`, else its hostname.
    name: String,
    /// Whether it announced `api_encryption`.
    encrypted: bool,
}

/// What the Add device panel shows for a device that needs its key.
fn needs_key(mac: &Mac, name: &str, why: &str) -> Option<Waiting> {
    Some(Waiting {
        // The same handle the device will have in the registry once it's in (`map::device_id`).
        unique_id: UniqueId::try_from(mac.as_str()).ok()?,
        name: Name::try_from(name.trim())
            .or_else(|_| Name::try_from(mac.as_str()))
            .ok()?,
        reason: why.to_owned(),
        secret: Some(SecretRequest {
            path: vec!["keys".to_owned(), mac.to_string()],
            label: "Encryption key".to_owned(),
            hint: Some("`api: encryption: key:` in the device's ESPHome YAML".to_owned()),
        }),
    })
}

async fn run(settings: Settings, mut ctx: IntegrationContext) -> Result<(), IntegrationError> {
    let (events_tx, mut events) = mpsc::channel(EVENT_QUEUE);
    let (discovered, listener) = discovery()?;
    let mut discovered = discovered;
    let mut devices = Devices::default();
    if !settings.keys.is_empty() {
        tracing::info!(keys = settings.keys.len(), "encryption keys configured");
    }

    let mut reported_health = Health::Running;
    ctx.set_health(reported_health.clone()).await;
    let mut reported_waiting: Vec<Waiting> = Vec::new();

    let outcome = loop {
        tokio::select! {
            call = ctx.next_call() => {
                let Some(incoming) = call else { break Ok(()) };
                route(incoming, &devices.nodes).await;
            }
            found = discovered.recv() => {
                match found {
                    Some(Ok(announced)) => {
                        arrive(announced, &settings, &mut devices, &events_tx);
                    }
                    // Discovery is how this integration finds anything, so losing it is not
                    // something to carry on quietly with: fail, and let the core restart us
                    // (`docs/specs/integrations.md` §5).
                    Some(Err(why)) => break Err(IntegrationError::new(why)),
                    None => {
                        break Err(IntegrationError::new(
                            "stopped listening for ESPHome devices",
                        ));
                    }
                }
            }
            Some(event) = events.recv() => {
                apply(event, &ctx, &mut devices).await?;
            }
        }
        // Only when they actually change. A busy device reports constantly, and the core's
        // operations channel is bounded and shared with describing entities and marking them
        // unavailable; repeating "still fine" into it would crowd out the things that matter.
        let now = health(&devices);
        if now != reported_health {
            ctx.set_health(now.clone()).await;
            reported_health = now;
        }
        let waiting: Vec<Waiting> = devices.waiting.values().cloned().collect();
        if waiting != reported_waiting {
            ctx.set_waiting(waiting.clone()).await;
            reported_waiting = waiting;
        }
    };

    // Stop listening first, so nothing new arrives while we're winding down.
    listener.abort();
    let handles: Vec<_> = std::mem::take(&mut devices.tasks)
        .into_values()
        .map(|task| task.handle)
        .collect();
    // Dropping the command senders is how a device task learns to stop.
    devices.nodes.clear();
    stop(handles).await;
    outcome
}

/// Starts talking to a device that announced itself — unless it needs a key it doesn't have, in
/// which case it's listed as waiting instead.
fn arrive(
    announced: Announced,
    settings: &Settings,
    devices: &mut Devices,
    events: &mpsc::Sender<node::Event>,
) {
    let address = announced.address;
    if devices.tasks.contains_key(&address) {
        return;
    }
    // Already known to be locked. Its key can't change without a restart, so trying again at
    // every announcement would only fail the same way, every time.
    if announced
        .mac
        .as_ref()
        .is_some_and(|mac| devices.waiting.contains_key(mac))
    {
        return;
    }
    devices.announced.insert(address, announced.clone());
    if announced.encrypted {
        let Some(mac) = &announced.mac else {
            tracing::warn!(%address, name = %announced.name,
                "skipping: it wants an encrypted connection but didn't announce its MAC address, so there's nothing to attach a key to");
            return;
        };
        let why = match settings.keys.get(mac).map(|given| &given.0) {
            Some(Ok(key)) => return connect(address, Some(key.clone()), devices, events),
            Some(Err(bad)) => format!("the key it was given can't be used: {bad}"),
            None => "it wants an encryption key".to_owned(),
        };
        tracing::info!(%address, device = %mac, name = %announced.name, %why,
            "found an ESPHome device that needs its encryption key");
        if let Some(waiting) = needs_key(mac, &announced.name, &why) {
            devices.waiting.insert(mac.clone(), waiting);
        }
        return;
    }
    connect(address, None, devices, events);
}

/// Starts the task that talks to one address.
fn connect(
    address: SocketAddr,
    key: Option<crate::settings::Key>,
    devices: &mut Devices,
    events: &mpsc::Sender<node::Event>,
) {
    tracing::info!(%address, encrypted = key.is_some(), "found an ESPHome device");
    devices.connections += 1;
    let connection = node::Connection {
        id: devices.connections,
        address,
    };
    let (commands, commands_rx) = mpsc::channel(COMMAND_QUEUE);
    let handle = tokio::spawn(node::run(connection, key, events.clone(), commands_rx));
    devices.tasks.insert(
        address,
        Task {
            connection,
            commands,
            handle,
        },
    );
}

/// Gives the device tasks a moment to finish, then stops them. A task can be waiting on a
/// connection that will never answer, and the contract gives the integration 5 seconds to be
/// gone; waiting for ever isn't one of the options.
async fn stop(tasks: Vec<tokio::task::JoinHandle<()>>) {
    let aborts: Vec<_> = tasks
        .iter()
        .map(tokio::task::JoinHandle::abort_handle)
        .collect();
    let finish = async {
        for task in tasks {
            let _ = task.await;
        }
    };
    if tokio::time::timeout(STOP_GRACE, finish).await.is_err() {
        tracing::debug!("some device connections didn't stop in time; dropping them");
        for abort in aborts {
            abort.abort();
        }
    }
}

/// Sends a service call to the device that owns the entity.
async fn route(incoming: IncomingCall, nodes: &BTreeMap<UniqueId, Node>) {
    let owner = nodes
        .iter()
        .find(|(_, node)| node.entities.contains(&incoming.call.unique_id));
    let Some((device, node)) = owner else {
        let why = format!(
            "`{}` belongs to a device Irori isn't connected to right now",
            incoming.call.unique_id
        );
        incoming.reply(Err(ServiceError::unavailable(why)));
        return;
    };
    // A device that's away doesn't get a queue of orders waiting for it. The caller has ten
    // seconds before the core gives up, and a light that switches itself on when a device
    // reconnects half an hour later is worse than one that didn't switch at all.
    if !node.online {
        let why = format!("`{device}` isn't connected right now");
        incoming.reply(Err(ServiceError::unavailable(why)));
        return;
    }
    // The queue is bounded, so a device that has stopped reading can't block the run loop: if
    // it's full, the call is refused rather than waited on.
    if let Err(e) = node.commands.try_send(incoming) {
        let refused = match e {
            mpsc::error::TrySendError::Full(call) => (
                call,
                format!("`{device}` has too many commands queued already"),
            ),
            mpsc::error::TrySendError::Closed(call) => (call, format!("`{device}` just went away")),
        };
        refused.0.reply(Err(ServiceError::unavailable(refused.1)));
    }
}

/// Whether this is still the connection running at its address, rather than one that has been
/// replaced since it spoke.
fn current(devices: &Devices, connection: node::Connection) -> bool {
    devices
        .tasks
        .get(&connection.address)
        .is_some_and(|task| task.connection == connection)
}

/// Applies what a device task reported to the core.
async fn apply(
    event: node::Event,
    ctx: &IntegrationContext,
    devices: &mut Devices,
) -> Result<(), IntegrationError> {
    match event {
        node::Event::Arrived {
            connection,
            device,
            entities,
        } => {
            let device_id = device.unique_id.clone();
            let address = connection.address;
            // A connection that has already been replaced can still have an arrival in the
            // queue — and by now another one may be running at the very same address. Identity
            // is what tells them apart: nothing from a connection that is no longer the one at
            // this address should reach the core.
            if !current(devices, connection) {
                tracing::debug!(device = %device_id, %address,
                    "ignoring an arrival from a connection that has already ended");
                return Ok(());
            }
            ctx.describe_device(*device).await?;
            let mut described = Vec::new();
            for entity in entities {
                let id = entity.unique_id.clone();
                match ctx.describe_entity(entity).await {
                    Ok(()) => described.push(id),
                    // One entity the core won't accept shouldn't cost us the whole device.
                    Err(e) => tracing::warn!(device = %device_id, entity = %id, error = %e,
                        "the core refused an entity"),
                }
            }
            // An entity that was there before and isn't now (the device was reflashed) goes.
            if let Some(previous) = devices.nodes.get(&device_id) {
                for gone in previous
                    .entities
                    .iter()
                    .filter(|id| !described.contains(id))
                {
                    if let Err(e) = ctx.remove_entity(gone.clone()).await {
                        tracing::warn!(entity = %gone, error = %e, "couldn't remove an entity");
                    }
                }
                // The same device answering from a new address: the old connection is stopped,
                // so it can't go on reporting for a device it no longer speaks for.
                let moved_from = previous.connection.address;
                if previous.connection != connection {
                    tracing::info!(device = %device_id, from = %moved_from, to = %address,
                        "the device moved address");
                    // Only if the address it left is still its own. Two devices can swap
                    // addresses, and the connection at the old one may already speak for
                    // somebody else — stopping that would disconnect a device that is fine.
                    let taken_over = devices.nodes.iter().any(|(id, node)| {
                        node.connection.address == moved_from && *id != device_id
                    });
                    if taken_over {
                        tracing::debug!(%moved_from,
                            "leaving the old address alone; another device answers there now");
                    } else {
                        // Dropping its command channel is how the old connection learns it's
                        // done. It stops at its next turn round the loop and refuses whatever
                        // it was still holding, which an abort would have thrown away.
                        devices.tasks.remove(&moved_from);
                        devices.unreachable.remove(&moved_from);
                    }
                }
            }
            // The address may have belonged to a different device until a moment ago (DHCP
            // hands addresses round). That device is not this one: its commands must stop going
            // to this connection, and it is no longer known to be present.
            let displaced: Vec<UniqueId> = devices
                .nodes
                .iter()
                .filter(|(id, node)| node.connection.address == address && **id != device_id)
                .map(|(id, _)| id.clone())
                .collect();
            for id in displaced {
                tracing::info!(device = %id, %address, now = %device_id,
                    "another device answers at this address now");
                if let Some(node) = devices.nodes.get_mut(&id) {
                    node.online = false;
                }
                // Its entities keep their last value, marked unavailable: the device may well
                // still exist, somewhere else on the network, and be found again.
                ctx.set_availability(AvailabilityTarget::Device(id), Availability::Unavailable)
                    .await?;
            }
            // The task that introduced it is the one its commands go to.
            let commands = devices
                .tasks
                .get(&address)
                .map(|task| task.commands.clone())
                .ok_or_else(|| {
                    IntegrationError::new(format!("`{device_id}` arrived with no task behind it"))
                })?;
            devices.unreachable.remove(&address);
            // Whatever it was waiting for, it has it now.
            devices
                .waiting
                .retain(|mac, _| mac.as_str() != device_id.as_str());
            devices.nodes.insert(
                device_id,
                Node {
                    connection,
                    commands,
                    entities: described,
                    online: true,
                },
            );
        }
        node::Event::Reported {
            connection,
            device,
            report,
        } => {
            // Only from the connection that speaks for the device now. A report queued by a
            // replaced connection is about a conversation that has already ended.
            if devices
                .nodes
                .get(&device)
                .is_none_or(|node| node.connection == connection)
            {
                ctx.report_state(*report);
            }
        }
        node::Event::Left {
            connection,
            device,
            why,
        } => {
            // A goodbye from a connection that has since been replaced says nothing about the
            // device: the one that speaks for it now is connected.
            let Some(node) = devices.nodes.get_mut(&device) else {
                return Ok(());
            };
            if node.connection != connection {
                return Ok(());
            }
            if !node.online {
                return Ok(()); // already known to be away; retrying is not news
            }
            tracing::info!(%device, %why, "lost an ESPHome device");
            node.online = false;
            // Its entities stay, showing their last value, marked unavailable.
            ctx.set_availability(
                AvailabilityTarget::Device(device),
                Availability::Unavailable,
            )
            .await?;
        }
        node::Event::Locked { connection, why } => {
            if !current(devices, connection) {
                return Ok(());
            }
            let address = connection.address;
            // Its task has already stopped; forget it so the address isn't counted as busy.
            devices.tasks.remove(&address);
            devices.unreachable.remove(&address);
            let announced = devices.announced.get(&address);
            match announced.and_then(|announced| announced.mac.clone()) {
                Some(mac) => {
                    let name = announced.map_or_else(|| mac.to_string(), |a| a.name.clone());
                    tracing::warn!(%address, device = %mac, %why, "an ESPHome device is locked");
                    if let Some(waiting) = needs_key(&mac, &name, &why) {
                        devices.waiting.insert(mac, waiting);
                    }
                }
                None => tracing::warn!(%address, %why,
                    "an ESPHome device is locked, and didn't announce a MAC address to attach a \
                     key to"),
            }
        }
        node::Event::Unreachable { connection, why } => {
            // Not from a connection that has since been replaced: that address is somebody
            // else's business now, and counting it would leave this integration degraded over
            // a device that is perfectly well somewhere else.
            if !current(devices, connection) {
                return Ok(());
            }
            let address = connection.address;
            // One entry per address, whatever the number of attempts, so the count is of
            // devices rather than tries.
            if devices.unreachable.insert(address, why.clone()).is_none() {
                tracing::warn!(%address, %why, "can't reach an ESPHome device");
            }
        }
    }
    Ok(())
}

/// What the Extensions view says about this integration.
fn health(devices: &Devices) -> Health {
    let offline = devices.nodes.values().filter(|node| !node.online).count();
    if devices.unreachable.is_empty() && offline == 0 && devices.waiting.is_empty() {
        Health::Running
    } else {
        let connected = devices.nodes.len() - offline;
        let mut trouble = Vec::new();
        if offline > 0 {
            trouble.push(format!("{offline} away"));
        }
        if !devices.unreachable.is_empty() {
            trouble.push(format!("{} unreachable", devices.unreachable.len()));
        }
        if !devices.waiting.is_empty() {
            trouble.push(format!("{} waiting for a key", devices.waiting.len()));
        }
        Health::Degraded(format!("{connected} connected, {}", trouble.join(", ")))
    }
}

/// Listens for ESPHome devices announcing themselves (`_esphomelib._tcp`), and reports the
/// address of each one Irori can talk to. Returns the stream and the listener's handle, so
/// shutdown can stop it rather than leave it holding the network listener open.
///
/// **Every device found is connected to.** Plain ESPHome has no authentication of its own, so
/// anything on this network that announces itself convincingly is believed. That's the same
/// trust an ESPHome device gets from Home Assistant on a home LAN, but it is a real limit:
/// until keys can be configured (M0.7), Irori's ESPHome support is only as trustworthy as the
/// network it runs on. See the README.
type Discovered = mpsc::Receiver<Result<Announced, String>>;

fn discovery() -> Result<(Discovered, tokio::task::JoinHandle<()>), IntegrationError> {
    let found = esphome_client::discovery::Client::default()
        .discover()
        .map_err(|e| IntegrationError::new(format!("can't listen for ESPHome devices: {e}")))?;
    let (tx, rx) = mpsc::channel(EVENT_QUEUE);
    let listener = tokio::spawn(async move {
        let mut found = found;
        loop {
            let message = match found.next().await {
                Ok(device) => {
                    let Some(address) = device.socket_address() else {
                        tracing::warn!(device = %device.hostname(), "announced with no address");
                        continue;
                    };
                    Ok(announced(address, &device))
                }
                // Discovery is the only way this integration finds anything, so the run loop
                // needs to hear about this rather than simply going quiet.
                Err(e) => Err(format!("stopped listening for ESPHome devices: {e}")),
            };
            let fatal = message.is_err();
            if tx.send(message).await.is_err() || fatal {
                return; // the integration stopped, or there's nothing more to say
            }
        }
    });
    Ok((rx, listener))
}

/// What a device's mDNS record says about it.
fn announced(address: SocketAddr, device: &esphome_client::discovery::DeviceInfo) -> Announced {
    let attributes = device.attributes();
    let hostname = device
        .hostname()
        .trim_end_matches('.')
        .trim_end_matches(".local");
    Announced {
        address,
        mac: attributes.get("mac").and_then(|mac| mac.parse().ok()),
        name: attributes
            .get("friendly_name")
            .filter(|name| !name.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| hostname.to_owned()),
        encrypted: device.has_encryption(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announced(port: u16, mac: &str, encrypted: bool) -> Announced {
        Announced {
            address: SocketAddr::from(([127, 0, 0, 1], port)),
            mac: Some(mac.parse().expect("a MAC")),
            name: "Garage door".to_owned(),
            encrypted,
        }
    }

    /// A device that wants a key it doesn't have is listed as waiting — named as it announced
    /// itself, with the place its key goes — and nothing tries to connect to it.
    #[tokio::test]
    async fn a_device_that_needs_a_key_waits_instead_of_being_connected_to() {
        let (events, _events) = mpsc::channel(8);
        let mut devices = Devices::default();

        arrive(
            announced(1, "aa:bb:cc:00:00:01", true),
            &Settings::default(),
            &mut devices,
            &events,
        );

        assert!(devices.tasks.is_empty(), "nothing to connect with");
        let waiting = devices.waiting.values().next().expect("waiting");
        assert_eq!(waiting.name.as_str(), "Garage door");
        assert_eq!(waiting.unique_id.as_str(), "AA:BB:CC:00:00:01");
        assert_eq!(
            waiting.secret.as_ref().map(|secret| secret.path.clone()),
            Some(vec!["keys".to_owned(), "AA:BB:CC:00:00:01".to_owned()])
        );
        assert_eq!(
            health(&devices),
            Health::Degraded("0 connected, 1 waiting for a key".to_owned())
        );

        // Announced again, as mDNS does: still one entry, still no connection.
        arrive(
            announced(1, "aa:bb:cc:00:00:01", true),
            &Settings::default(),
            &mut devices,
            &events,
        );
        assert_eq!(devices.waiting.len(), 1);
        assert!(devices.tasks.is_empty());
    }

    /// With its key, the same device is connected to; without encryption, no key is needed.
    #[tokio::test]
    async fn a_device_with_its_key_or_without_encryption_is_connected_to() {
        let (events, _events) = mpsc::channel(8);
        let mut devices = Devices::default();
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "keys": { "aa:bb:cc:00:00:01": "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=" }
        }))
        .expect("valid");

        arrive(
            announced(1, "aa:bb:cc:00:00:01", true),
            &settings,
            &mut devices,
            &events,
        );
        arrive(
            announced(2, "aa:bb:cc:00:00:02", false),
            &settings,
            &mut devices,
            &events,
        );

        assert_eq!(devices.tasks.len(), 2);
        assert!(devices.waiting.is_empty());
        stop(
            std::mem::take(&mut devices.tasks)
                .into_values()
                .map(|t| t.handle)
                .collect(),
        )
        .await;
    }

    /// A key that isn't a key keeps only its own device waiting, and says why. The other devices
    /// are none of its business — failing the settings would disconnect all of them.
    #[tokio::test]
    async fn a_malformed_key_keeps_only_its_own_device_waiting() {
        let (events, _events) = mpsc::channel(8);
        let mut devices = Devices::default();
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "keys": { "aa:bb:cc:00:00:01": "far too short" }
        }))
        .expect("the settings still load");

        arrive(
            announced(1, "aa:bb:cc:00:00:01", true),
            &settings,
            &mut devices,
            &events,
        );
        arrive(
            announced(2, "aa:bb:cc:00:00:02", false),
            &settings,
            &mut devices,
            &events,
        );

        assert_eq!(
            devices.tasks.len(),
            1,
            "the unencrypted device is connected regardless"
        );
        let waiting = devices.waiting.values().next().expect("waiting");
        assert!(
            waiting.reason.contains("can't be used"),
            "{}",
            waiting.reason
        );
        assert!(
            !waiting.reason.contains("far too short"),
            "{}",
            waiting.reason
        );
        stop(
            std::mem::take(&mut devices.tasks)
                .into_values()
                .map(|t| t.handle)
                .collect(),
        )
        .await;
    }

    /// A key that turns out to be wrong puts the device back to waiting, under the name it
    /// announced, and says why — so the page can ask for the right one.
    #[tokio::test]
    async fn a_wrong_key_puts_the_device_back_to_waiting() {
        let (ctx, _host) = irori_integration::host::connect();
        let (events, _events) = mpsc::channel(8);
        let mut devices = Devices::default();
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "keys": { "aa:bb:cc:00:00:01": "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=" }
        }))
        .expect("valid");
        arrive(
            announced(1, "aa:bb:cc:00:00:01", true),
            &settings,
            &mut devices,
            &events,
        );
        let connection = devices.tasks.values().next().expect("a task").connection;

        apply(
            node::Event::Locked {
                connection,
                why: "the encryption key doesn't match the one on the device".to_owned(),
            },
            &ctx,
            &mut devices,
        )
        .await
        .expect("applied");

        assert!(devices.tasks.is_empty());
        let waiting = devices.waiting.values().next().expect("waiting");
        assert_eq!(waiting.name.as_str(), "Garage door");
        assert!(
            waiting.reason.contains("doesn't match"),
            "{}",
            waiting.reason
        );
    }

    /// A device that answers from a new address is taken over by the new connection, and the
    /// old one is stopped: otherwise both keep reporting, and the old one's goodbye marks a
    /// perfectly healthy device unavailable.
    #[tokio::test]
    async fn a_device_that_moves_address_leaves_no_stale_connection_behind() {
        let (ctx, host) = irori_integration::host::connect();
        // Stand in for the core: accept whatever the integration describes. The point of this
        // test is which connection the run loop keeps, not what the registry ends up holding.
        let mut ops = host.ops;
        tokio::spawn(async move {
            while let Some(op) = ops.recv().await {
                match op {
                    irori_integration::host::Op::DescribeDevice(_, reply)
                    | irori_integration::host::Op::DescribeEntity(_, reply)
                    | irori_integration::host::Op::RemoveDevice(_, reply)
                    | irori_integration::host::Op::RemoveEntity(_, reply)
                    | irori_integration::host::Op::SetAvailability(_, _, reply) => {
                        let _ = reply.send(Ok(()));
                    }
                    irori_integration::host::Op::SetHealth(_)
                    | irori_integration::host::Op::SetWaiting(_) => {}
                    irori_integration::host::Op::Load(_, reply) => {
                        let _ = reply.send(Ok(None));
                    }
                    irori_integration::host::Op::Store(_, _, reply) => {
                        let _ = reply.send(Ok(()));
                    }
                }
            }
        });
        let mut devices = Devices::default();
        let old_address: SocketAddr = "127.0.0.1:6053".parse().expect("valid");
        let new_address: SocketAddr = "127.0.0.2:6053".parse().expect("valid");

        // Two connections, as discovery would leave behind when a device changes address.
        let mut stopped = Vec::new();
        let mut connections = Vec::new();
        for (id, address) in [old_address, new_address].into_iter().enumerate() {
            let connection = node::Connection {
                id: id as u64 + 1,
                address,
            };
            connections.push(connection);
            let (commands, mut commands_rx) = mpsc::channel(1);
            let (ran_tx, ran_rx) = tokio::sync::oneshot::channel();
            stopped.push(ran_rx);
            let handle = tokio::spawn(async move {
                // Ends only when aborted or when the run loop drops the command channel.
                let _ = commands_rx.recv().await;
                let _ = ran_tx.send(());
            });
            devices.tasks.insert(
                address,
                Task {
                    connection,
                    commands,
                    handle,
                },
            );
        }

        let device = irori_integration::types::DeviceDescription {
            unique_id: UniqueId::try_from("AA:BB:CC:DD:EE:FF").expect("valid"),
            name: irori_integration::types::Name::try_from("Moving device").expect("valid"),
            manufacturer: None,
            model: None,
            sw_version: None,
            hw_version: None,
            suggested_area: None,
            via_device_unique_id: None,
        };
        let device_id = device.unique_id.clone();
        let arrived = |connection| node::Event::Arrived {
            connection,
            device: Box::new(device.clone()),
            entities: Vec::new(),
        };

        // The core end isn't being drained here, so describing is done for its bookkeeping only;
        // what matters is which task the run loop keeps.
        let first = tokio::time::timeout(
            Duration::from_millis(200),
            apply(arrived(connections[0]), &ctx, &mut devices),
        )
        .await;
        assert!(first.is_ok(), "the first arrival is handled");
        assert_eq!(devices.nodes[&device_id].connection, connections[0]);

        let second = tokio::time::timeout(
            Duration::from_millis(200),
            apply(arrived(connections[1]), &ctx, &mut devices),
        )
        .await;
        assert!(second.is_ok(), "the second arrival is handled");
        assert_eq!(
            devices.nodes[&device_id].connection, connections[1],
            "the new connection speaks for the device"
        );
        assert!(
            !devices.tasks.contains_key(&old_address),
            "the old connection is gone"
        );

        // And a goodbye from the connection that was replaced doesn't mark the device away.
        let left = tokio::time::timeout(
            Duration::from_millis(200),
            apply(
                node::Event::Left {
                    connection: connections[0],
                    device: device_id.clone(),
                    why: "the old socket noticed".to_owned(),
                },
                &ctx,
                &mut devices,
            ),
        )
        .await;
        assert!(left.is_ok(), "the stale goodbye is handled");
        assert!(
            devices.nodes[&device_id].online,
            "a healthy device stays online when its old connection says goodbye"
        );
    }

    /// Addresses get handed round: the one a device had yesterday can belong to a different
    /// device today. The old one must stop being "connected" at an address that isn't its own,
    /// or its commands would be sent to a stranger.
    #[tokio::test]
    async fn a_device_that_loses_its_address_to_another_stops_claiming_it() {
        let (ctx, host) = irori_integration::host::connect();
        let mut ops = host.ops;
        tokio::spawn(async move {
            while let Some(op) = ops.recv().await {
                match op {
                    irori_integration::host::Op::DescribeDevice(_, reply)
                    | irori_integration::host::Op::DescribeEntity(_, reply)
                    | irori_integration::host::Op::RemoveDevice(_, reply)
                    | irori_integration::host::Op::RemoveEntity(_, reply)
                    | irori_integration::host::Op::SetAvailability(_, _, reply) => {
                        let _ = reply.send(Ok(()));
                    }
                    irori_integration::host::Op::SetHealth(_)
                    | irori_integration::host::Op::SetWaiting(_) => {}
                    irori_integration::host::Op::Load(_, reply) => {
                        let _ = reply.send(Ok(None));
                    }
                    irori_integration::host::Op::Store(_, _, reply) => {
                        let _ = reply.send(Ok(()));
                    }
                }
            }
        });

        let mut devices = Devices::default();
        let address: SocketAddr = "127.0.0.1:6053".parse().expect("valid");
        let (commands, mut commands_rx) = mpsc::channel(1);
        let handle = tokio::spawn(async move {
            let _ = commands_rx.recv().await;
        });
        let connection = node::Connection { id: 1, address };
        devices.tasks.insert(
            address,
            Task {
                connection,
                commands,
                handle,
            },
        );

        let arrival = |mac: &str| {
            let unique_id = UniqueId::try_from(mac).expect("valid");
            node::Event::Arrived {
                connection,
                device: Box::new(irori_integration::types::DeviceDescription {
                    unique_id,
                    name: irori_integration::types::Name::try_from("A device").expect("valid"),
                    manufacturer: None,
                    model: None,
                    sw_version: None,
                    hw_version: None,
                    suggested_area: None,
                    via_device_unique_id: None,
                }),
                entities: Vec::new(),
            }
        };

        let first = UniqueId::try_from("AA:AA:AA:AA:AA:AA").expect("valid");
        let second = UniqueId::try_from("BB:BB:BB:BB:BB:BB").expect("valid");
        apply(arrival(first.as_str()), &ctx, &mut devices)
            .await
            .expect("the first device arrives");
        assert!(devices.nodes[&first].online);

        // The same address, a different device.
        apply(arrival(second.as_str()), &ctx, &mut devices)
            .await
            .expect("the second device arrives");
        assert!(devices.nodes[&second].online, "the new one is connected");
        assert!(
            !devices.nodes[&first].online,
            "the old one is no longer reachable at an address that isn't its own"
        );

        // And it won't take commands, which would otherwise reach the wrong device.
        let (incoming, answer) =
            irori_integration::host::incoming_call(irori_integration::types::ServiceCall {
                unique_id: UniqueId::try_from("AA:AA:AA:AA:AA:AA-switch-1").expect("valid"),
                service: irori_integration::types::Service::SwitchTurnOn,
                context: irori_integration::types::Context {
                    id: irori_integration::types::ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9")
                        .expect("valid"),
                    parent_id: None,
                    origin: irori_integration::types::Origin::System,
                },
            });
        // The displaced device still owns the entity, so this is the routing that matters.
        devices
            .nodes
            .get_mut(&first)
            .expect("still known")
            .entities
            .push(UniqueId::try_from("AA:AA:AA:AA:AA:AA-switch-1").expect("valid"));
        route(incoming, &devices.nodes).await;
        let answered = answer.await.expect("answered rather than dropped");
        assert!(
            matches!(answered, Err(ref e) if e.code == irori_integration::ServiceErrorCode::Unavailable),
            "got {answered:?}"
        );
    }

    /// The case an address alone can't catch: the old connection is replaced by a new one at
    /// the *same* address, and the old one's arrival is still in the queue behind it.
    #[tokio::test]
    async fn an_arrival_from_a_replaced_connection_is_ignored() {
        let (ctx, host) = irori_integration::host::connect();
        let mut ops = host.ops;
        let described = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&described);
        tokio::spawn(async move {
            while let Some(op) = ops.recv().await {
                match op {
                    irori_integration::host::Op::DescribeDevice(_, reply) => {
                        counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let _ = reply.send(Ok(()));
                    }
                    irori_integration::host::Op::DescribeEntity(_, reply)
                    | irori_integration::host::Op::RemoveDevice(_, reply)
                    | irori_integration::host::Op::RemoveEntity(_, reply)
                    | irori_integration::host::Op::SetAvailability(_, _, reply) => {
                        let _ = reply.send(Ok(()));
                    }
                    irori_integration::host::Op::SetHealth(_)
                    | irori_integration::host::Op::SetWaiting(_) => {}
                    irori_integration::host::Op::Load(_, reply) => {
                        let _ = reply.send(Ok(None));
                    }
                    irori_integration::host::Op::Store(_, _, reply) => {
                        let _ = reply.send(Ok(()));
                    }
                }
            }
        });

        let mut devices = Devices::default();
        let address: SocketAddr = "127.0.0.1:6053".parse().expect("valid");
        let old = node::Connection { id: 1, address };
        let new = node::Connection { id: 2, address };
        // Only the newer connection is running: the older one was replaced a moment ago.
        let (commands, mut commands_rx) = mpsc::channel(1);
        let handle = tokio::spawn(async move {
            let _ = commands_rx.recv().await;
        });
        devices.tasks.insert(
            address,
            Task {
                connection: new,
                commands,
                handle,
            },
        );

        let arrival = |connection| node::Event::Arrived {
            connection,
            device: Box::new(irori_integration::types::DeviceDescription {
                unique_id: UniqueId::try_from("AA:BB:CC:DD:EE:FF").expect("valid"),
                name: irori_integration::types::Name::try_from("A device").expect("valid"),
                manufacturer: None,
                model: None,
                sw_version: None,
                hw_version: None,
                suggested_area: None,
                via_device_unique_id: None,
            }),
            entities: Vec::new(),
        };

        apply(arrival(old), &ctx, &mut devices)
            .await
            .expect("a stale arrival is ignored, not an error");
        assert!(
            devices.nodes.is_empty(),
            "the replaced connection doesn't get to introduce anything"
        );

        apply(arrival(new), &ctx, &mut devices)
            .await
            .expect("the running connection introduces its device");
        assert_eq!(devices.nodes.len(), 1);
        // Waiting for the describe to have gone through before counting it.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            described.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the core heard about the device once, from the connection that is actually running"
        );
    }

    #[test]
    fn the_manifest_is_valid() {
        let builtin = irori_integration::builtin::<Esphome>().expect("valid built-in");
        assert_eq!(builtin.manifest.extension.id.as_str(), "esphome");
        assert!(builtin.manifest.warnings().is_empty());
        // Finding and talking to devices happens on the local network, and nowhere else.
        assert!(builtin.manifest.permissions.lan);
        assert!(builtin.manifest.permissions.network.is_empty());
    }
}
