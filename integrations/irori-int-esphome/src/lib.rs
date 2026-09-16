//! Devices running [ESPHome](https://esphome.io) firmware, over ESPHome's native API.
//!
//! ESPHome devices announce themselves on the local network, so this integration needs no
//! setting up: it listens for them, connects, and everything they have shows up in the home.
//! Each device gets its own task ([`node`]); this module owns the loop that talks to the core,
//! because the core's handle can't be shared.
//!
//! **Not yet:** encrypted devices. ESPHome's API can require a pre-shared key, and a key has to
//! be configured somewhere, which waits for the config dir (M0.7). Devices that want one are
//! listed in the log and left alone rather than retried forever.

#[cfg(test)]
mod fake_device;
mod map;
mod node;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use irori_integration::types::{Availability, UniqueId};
use irori_integration::{
    AvailabilityTarget, Health, IncomingCall, Integration, IntegrationContext, IntegrationError,
    NoSettings, ServiceError,
};
use tokio::sync::mpsc;

/// The ESPHome integration.
#[derive(Debug)]
pub struct Esphome;

impl Integration for Esphome {
    // Nothing to configure yet: discovery finds the devices, and the one thing that will need
    // configuring (encryption keys) waits for the config dir (M0.7).
    type Config = NoSettings;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");

    async fn run(_config: NoSettings, ctx: IntegrationContext) -> Result<(), IntegrationError> {
        run(ctx).await
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
    commands: mpsc::Sender<IncomingCall>,
    /// Kept so shutdown (and a device that moved address) can stop it, rather than leaving it
    /// running detached with a socket open.
    handle: tokio::task::JoinHandle<()>,
}

/// A device the home knows about, and the task currently speaking for it.
#[derive(Debug)]
struct Node {
    /// Which connection owns this device. A device that moves to a new address is taken over by
    /// the new task, and anything the old one says afterwards is ignored.
    address: SocketAddr,
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
}

async fn run(mut ctx: IntegrationContext) -> Result<(), IntegrationError> {
    let (events_tx, mut events) = mpsc::channel(EVENT_QUEUE);
    let (discovered, listener) = discovery()?;
    let mut discovered = discovered;
    let mut devices = Devices::default();

    let mut reported_health = Health::Running;
    ctx.set_health(reported_health.clone()).await;

    let outcome = loop {
        tokio::select! {
            call = ctx.next_call() => {
                let Some(incoming) = call else { break Ok(()) };
                route(incoming, &devices.nodes).await;
            }
            found = discovered.recv() => {
                match found {
                    Some(Ok(address)) => {
                        if devices.tasks.contains_key(&address) {
                            continue;
                        }
                        tracing::info!(%address, "found an ESPHome device");
                        let (commands, commands_rx) = mpsc::channel(COMMAND_QUEUE);
                        let handle = tokio::spawn(node::run(address, events_tx.clone(), commands_rx));
                        devices.tasks.insert(address, Task { commands, handle });
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
                // Only when it actually changes. A busy device reports constantly, and the
                // core's operations channel is bounded and shared with describing entities
                // and marking them unavailable; repeating "still fine" into it would crowd
                // out the things that matter.
                let now = health(&devices);
                if now != reported_health {
                    ctx.set_health(now.clone()).await;
                    reported_health = now;
                }
            }
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

/// Applies what a device task reported to the core.
async fn apply(
    event: node::Event,
    ctx: &IntegrationContext,
    devices: &mut Devices,
) -> Result<(), IntegrationError> {
    match event {
        node::Event::Arrived {
            address,
            device,
            entities,
        } => {
            let device_id = device.unique_id.clone();
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
                if previous.address != address {
                    tracing::info!(device = %device_id, from = %previous.address, to = %address,
                        "the device moved address");
                    // Dropping its command channel is how the old connection learns it's done.
                    // It stops at its next turn round the loop and refuses whatever it was
                    // still holding, which an abort would have thrown away un-answered.
                    devices.tasks.remove(&previous.address);
                    devices.unreachable.remove(&previous.address);
                }
            }
            // The address may have belonged to a different device until a moment ago (DHCP
            // hands addresses round). That device is not this one: its commands must stop going
            // to this connection, and it is no longer known to be present.
            let displaced: Vec<UniqueId> = devices
                .nodes
                .iter()
                .filter(|(id, node)| node.address == address && **id != device_id)
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
            devices.nodes.insert(
                device_id,
                Node {
                    address,
                    commands,
                    entities: described,
                    online: true,
                },
            );
        }
        node::Event::Reported {
            address,
            device,
            report,
        } => {
            // Only from the connection that speaks for the device now. A report queued by a
            // replaced connection is about a conversation that has already ended.
            if devices
                .nodes
                .get(&device)
                .is_none_or(|node| node.address == address)
            {
                ctx.report_state(*report);
            }
        }
        node::Event::Left {
            address,
            device,
            why,
        } => {
            // A goodbye from a connection that has since been replaced says nothing about the
            // device: the task that speaks for it now is connected.
            let Some(node) = devices.nodes.get_mut(&device) else {
                return Ok(());
            };
            if node.address != address {
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
        node::Event::Unreachable { address, why } => {
            // Not from a connection that has since been replaced: that address is somebody
            // else's business now, and counting it would leave this integration degraded over
            // a device that is perfectly well somewhere else.
            if !devices.tasks.contains_key(&address) {
                return Ok(());
            }
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
    if devices.unreachable.is_empty() && offline == 0 {
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
type Discovered = mpsc::Receiver<Result<SocketAddr, String>>;

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
                    if device.has_encryption() {
                        // Nothing to do about it yet, and retrying wouldn't help; say so once.
                        tracing::warn!(
                            device = %device.hostname(),
                            "skipping: it wants an encrypted connection, and Irori can't hold a \
                             key until the config dir lands (M0.7)"
                        );
                        continue;
                    }
                    match device.socket_address() {
                        Some(address) => Ok(address),
                        None => {
                            tracing::warn!(device = %device.hostname(), "announced with no address");
                            continue;
                        }
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
                    irori_integration::host::Op::SetHealth(_) => {}
                }
            }
        });
        let mut devices = Devices::default();
        let old_address: SocketAddr = "127.0.0.1:6053".parse().expect("valid");
        let new_address: SocketAddr = "127.0.0.2:6053".parse().expect("valid");

        // Two connections, as discovery would leave behind when a device changes address.
        let mut stopped = Vec::new();
        for address in [old_address, new_address] {
            let (commands, mut commands_rx) = mpsc::channel(1);
            let (ran_tx, ran_rx) = tokio::sync::oneshot::channel();
            stopped.push(ran_rx);
            let handle = tokio::spawn(async move {
                // Ends only when aborted or when the run loop drops the command channel.
                let _ = commands_rx.recv().await;
                let _ = ran_tx.send(());
            });
            devices.tasks.insert(address, Task { commands, handle });
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
        let arrived = |address| node::Event::Arrived {
            address,
            device: Box::new(device.clone()),
            entities: Vec::new(),
        };

        // The core end isn't being drained here, so describing is done for its bookkeeping only;
        // what matters is which task the run loop keeps.
        let first = tokio::time::timeout(
            Duration::from_millis(200),
            apply(arrived(old_address), &ctx, &mut devices),
        )
        .await;
        assert!(first.is_ok(), "the first arrival is handled");
        assert_eq!(devices.nodes[&device_id].address, old_address);

        let second = tokio::time::timeout(
            Duration::from_millis(200),
            apply(arrived(new_address), &ctx, &mut devices),
        )
        .await;
        assert!(second.is_ok(), "the second arrival is handled");
        assert_eq!(
            devices.nodes[&device_id].address, new_address,
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
                    address: old_address,
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
                    irori_integration::host::Op::SetHealth(_) => {}
                }
            }
        });

        let mut devices = Devices::default();
        let address: SocketAddr = "127.0.0.1:6053".parse().expect("valid");
        let (commands, mut commands_rx) = mpsc::channel(1);
        let handle = tokio::spawn(async move {
            let _ = commands_rx.recv().await;
        });
        devices.tasks.insert(address, Task { commands, handle });

        let arrival = |mac: &str| {
            let unique_id = UniqueId::try_from(mac).expect("valid");
            node::Event::Arrived {
                address,
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
