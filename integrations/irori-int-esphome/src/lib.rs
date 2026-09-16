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

/// One connected (or connecting) device.
#[derive(Debug)]
struct Node {
    commands: mpsc::Sender<IncomingCall>,
    /// Its entities, once it has introduced itself, so calls can be routed and so a device
    /// that comes back with fewer entities can have the old ones removed.
    entities: Vec<UniqueId>,
}

async fn run(mut ctx: IntegrationContext) -> Result<(), IntegrationError> {
    let (events_tx, mut events) = mpsc::channel(EVENT_QUEUE);
    let mut discovered = discovery()?;

    // Devices by address, so a second announcement for one we're already talking to is ignored.
    let mut addresses: BTreeMap<SocketAddr, mpsc::Sender<IncomingCall>> = BTreeMap::new();
    // Devices by the id the home knows them by, for routing calls.
    let mut nodes: BTreeMap<UniqueId, Node> = BTreeMap::new();
    let mut tasks = Vec::new();
    let mut unreachable = 0_usize;

    ctx.set_health(Health::Running).await;

    loop {
        tokio::select! {
            call = ctx.next_call() => {
                let Some(incoming) = call else { break };
                route(incoming, &nodes).await;
            }
            Some(address) = discovered.recv() => {
                if addresses.contains_key(&address) {
                    continue;
                }
                tracing::info!(%address, "found an ESPHome device");
                let (commands_tx, commands_rx) = mpsc::channel(COMMAND_QUEUE);
                addresses.insert(address, commands_tx.clone());
                tasks.push(tokio::spawn(node::run(address, events_tx.clone(), commands_rx)));
            }
            Some(event) = events.recv() => {
                apply(event, &ctx, &mut nodes, &addresses, &mut unreachable).await?;
                health(&ctx, &nodes, unreachable).await;
            }
        }
    }

    // Told to stop: dropping the command channels ends the device tasks at their next await.
    drop(addresses);
    drop(nodes);
    let _ = tokio::time::timeout(STOP_GRACE, async {
        for task in tasks {
            let _ = task.await;
        }
    })
    .await;
    Ok(())
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
    nodes: &mut BTreeMap<UniqueId, Node>,
    addresses: &BTreeMap<SocketAddr, mpsc::Sender<IncomingCall>>,
    unreachable: &mut usize,
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
            if let Some(previous) = nodes.get(&device_id) {
                for gone in previous
                    .entities
                    .iter()
                    .filter(|id| !described.contains(id))
                {
                    if let Err(e) = ctx.remove_entity(gone.clone()).await {
                        tracing::warn!(entity = %gone, error = %e, "couldn't remove an entity");
                    }
                }
            }
            // The task that introduced it is the one its commands go to.
            let commands = addresses.get(&address).cloned().ok_or_else(|| {
                IntegrationError::new(format!("`{device_id}` arrived with no task behind it"))
            })?;
            nodes.insert(
                device_id,
                Node {
                    commands,
                    entities: described,
                },
            );
        }
        node::Event::Reported(report) => ctx.report_state(*report),
        node::Event::Left { device, why } => {
            tracing::info!(%device, %why, "lost an ESPHome device");
            // Its entities stay, showing their last value, marked unavailable.
            ctx.set_availability(
                AvailabilityTarget::Device(device),
                Availability::Unavailable,
            )
            .await?;
        }
        node::Event::Unreachable { address, why } => {
            *unreachable += 1;
            tracing::warn!(%address, %why, "can't reach an ESPHome device");
        }
    }
    Ok(())
}

/// What the Extensions view says about this integration.
async fn health(ctx: &IntegrationContext, nodes: &BTreeMap<UniqueId, Node>, unreachable: usize) {
    let health = if unreachable == 0 {
        Health::Running
    } else {
        Health::Degraded(format!(
            "{} device(s) connected, {unreachable} couldn't be reached",
            nodes.len()
        ))
    };
    ctx.set_health(health).await;
}

/// Listens for ESPHome devices announcing themselves (`_esphomelib._tcp`), and reports the
/// address of each one that Irori can talk to.
fn discovery() -> Result<mpsc::Receiver<SocketAddr>, IntegrationError> {
    let found = esphome_client::discovery::Client::default()
        .discover()
        .map_err(|e| IntegrationError::new(format!("can't listen for ESPHome devices: {e}")))?;
    let (tx, rx) = mpsc::channel(EVENT_QUEUE);
    tokio::spawn(async move {
        let mut found = found;
        loop {
            match found.next().await {
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
                    let Some(address) = device.socket_address() else {
                        tracing::warn!(device = %device.hostname(), "announced with no address");
                        continue;
                    };
                    if tx.send(address).await.is_err() {
                        return; // the integration stopped
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "stopped listening for ESPHome devices");
                    return;
                }
            }
        }
    });
    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

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
