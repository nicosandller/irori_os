//! Google Cast receivers on the local network: Chromecast, speakers, and speaker groups.
//!
//! Each receiver is one media player. Discovery is mDNS, plus any known host in the settings.
//! The device fetches what it's asked to play; this protocol only tells it the address.

mod discover;
mod map;
mod proto;
mod session;
pub mod settings;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::net::SocketAddr;

use irori_protocol::{
    AvailabilityTarget, Health, IncomingCall, Protocol, ProtocolContext, ProtocolError,
    ServiceError,
};
use irori_types::{Availability, ContextId, MediaPlayerState, State, StateReport, UniqueId};
use tokio::sync::mpsc;

use crate::discover::{Discovery, Found};
use crate::map::device_class;
use crate::session::SessionEvent;
use crate::settings::Settings;

/// The Google Cast protocol.
#[derive(Debug)]
pub struct Cast;

impl Protocol for Cast {
    type Config = Settings;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");
    const ICON: Option<&'static str> = Some(include_str!("../icon.svg"));

    async fn run(settings: Settings, ctx: ProtocolContext) -> Result<(), ProtocolError> {
        let (found, incoming) = mpsc::channel(32);
        let discovery = discover::start(&settings, found).map_err(ProtocolError::new)?;
        let outcome = drive(settings, ctx, incoming).await;
        discovery.abort();
        outcome
    }
}

const COMMANDS: usize = 8;
const EVENTS: usize = 64;

struct Device {
    generation: u64,
    address: SocketAddr,
    name: String,
    model: String,
    version: String,
    known_host: Option<String>,
    online: bool,
    entity: UniqueId,
    commands: mpsc::Sender<IncomingCall>,
    handle: tokio::task::JoinHandle<()>,
}

impl Device {
    fn same(&self, found: &Found) -> bool {
        self.address == found.address
            && self.name == found.name
            && self.model == found.model
            && self.version == found.version
    }
}

struct Devices {
    by_uuid: BTreeMap<String, Device>,
    /// Known hosts that have never introduced themselves.
    failed_hosts: BTreeMap<String, ()>,
    generations: u64,
}

impl Devices {
    fn health(&self) -> Health {
        let offline = self
            .by_uuid
            .values()
            .filter(|device| !device.online)
            .count();
        let count = offline + self.failed_hosts.len();
        match count {
            0 => Health::Running,
            1 => Health::Degraded("1 Cast device can't be reached".to_owned()),
            n => Health::Degraded(format!("{n} Cast devices can't be reached")),
        }
    }
}

/// Runs the protocol against an injected discovery stream. Tests use this so nothing browses the
/// real network.
pub(crate) async fn drive(
    settings: Settings,
    mut ctx: ProtocolContext,
    mut discovery: mpsc::Receiver<Discovery>,
) -> Result<(), ProtocolError> {
    let (events_tx, mut events) = mpsc::channel(EVENTS);
    let mut devices = Devices {
        by_uuid: BTreeMap::new(),
        failed_hosts: BTreeMap::new(),
        generations: 0,
    };
    let mut reported = Health::Running;
    ctx.set_health(reported.clone()).await;

    let outcome = loop {
        tokio::select! {
            call = ctx.next_call() => {
                let Some(call) = call else { break Ok(()) };
                route(call, &devices.by_uuid).await;
            }
            found = discovery.recv() => {
                match found {
                    Some(Discovery::Seen(found)) => {
                        adopt(found, &settings, &mut devices, &events_tx, &ctx).await;
                    }
                    Some(Discovery::Unreachable { host }) => {
                        let known = devices.by_uuid.values().any(|device| {
                            device.known_host.as_deref() == Some(host.as_str())
                        });
                        if !known {
                            devices.failed_hosts.insert(host, ());
                        }
                    }
                    None => break Ok(()),
                }
            }
            event = events.recv() => {
                match event {
                    Some(SessionEvent::Status { generation, uuid, state, caused_by }) => {
                        apply_status(&ctx, &mut devices, generation, &uuid, state, caused_by).await?;
                    }
                    Some(SessionEvent::Offline { generation, uuid }) => {
                        apply_offline(&ctx, &mut devices, generation, &uuid).await?;
                    }
                    None => break Err(ProtocolError::new("stopped talking to Cast devices")),
                }
            }
        }
        let health = devices.health();
        if health != reported {
            ctx.set_health(health.clone()).await;
            reported = health;
        }
    };

    for device in devices.by_uuid.into_values() {
        device.handle.abort();
    }
    outcome
}

fn allowed(settings: &Settings, uuid: &str) -> bool {
    settings.uuids.is_empty() || settings.uuids.iter().any(|id| id == uuid)
}

fn ignores_cec(settings: &Settings, found: &Found) -> bool {
    settings
        .ignore_cec
        .iter()
        .any(|entry| entry == &found.uuid || entry == &found.name)
}

async fn adopt(
    found: Found,
    settings: &Settings,
    devices: &mut Devices,
    events: &mpsc::Sender<SessionEvent>,
    ctx: &ProtocolContext,
) {
    if !allowed(settings, &found.uuid) {
        return;
    }
    if let Some(host) = &found.known_host {
        devices.failed_hosts.remove(host);
    }
    if let Some(device) = devices.by_uuid.get(&found.uuid)
        && device.same(&found)
    {
        return;
    }

    let (device, entity) = match map::descriptions(&found) {
        Ok(pair) => pair,
        Err(error) => {
            tracing::warn!("a Cast device couldn't be added: {error}");
            return;
        }
    };
    if ctx.describe_device(device).await.is_err() {
        tracing::warn!("a Cast device couldn't be added");
        return;
    }
    if ctx.describe_entity(entity).await.is_err() {
        tracing::warn!("a Cast player couldn't be added");
        return;
    }

    let restart = match devices.by_uuid.get(&found.uuid) {
        Some(device) => {
            device.address != found.address
                || device.name != found.name
                || device_class(&device.model) != device_class(&found.model)
        }
        None => true,
    };
    if !restart {
        if let Some(device) = devices.by_uuid.get_mut(&found.uuid) {
            device.version = found.version;
            device.model = found.model;
            device.known_host = found.known_host;
        }
        return;
    }

    let entity_id =
        UniqueId::try_from(format!("media:{}", found.uuid)).expect("the id was just built");
    let device_id = UniqueId::try_from(found.uuid.as_str()).expect("the id was just built");
    if ctx
        .set_availability(
            AvailabilityTarget::Device(device_id),
            Availability::Unavailable,
        )
        .await
        .is_err()
    {
        tracing::warn!("a Cast device couldn't be marked unavailable");
    }

    devices.generations += 1;
    let generation = devices.generations;
    if let Some(old) = devices.by_uuid.remove(&found.uuid) {
        old.handle.abort();
    }
    let (commands, incoming) = mpsc::channel(COMMANDS);
    let handle = session::spawn(
        found.clone(),
        generation,
        ignores_cec(settings, &found),
        incoming,
        events.clone(),
    );
    devices.by_uuid.insert(
        found.uuid.clone(),
        Device {
            generation,
            address: found.address,
            name: found.name,
            model: found.model,
            version: found.version,
            known_host: found.known_host,
            online: false,
            entity: entity_id,
            commands,
            handle,
        },
    );
}

async fn apply_status(
    ctx: &ProtocolContext,
    devices: &mut Devices,
    generation: u64,
    uuid: &str,
    state: MediaPlayerState,
    caused_by: Option<ContextId>,
) -> Result<(), ProtocolError> {
    let Some(device) = devices.by_uuid.get_mut(uuid) else {
        return Ok(());
    };
    if device.generation != generation {
        return Ok(());
    }
    let entity = device.entity.clone();
    let device_id =
        UniqueId::try_from(uuid).map_err(|error| ProtocolError::new(error.to_string()))?;
    let came_back = !device.online;
    device.online = true;
    if came_back {
        ctx.set_availability(
            AvailabilityTarget::Device(device_id),
            Availability::Available,
        )
        .await
        .map_err(|error| ProtocolError::new(error.to_string()))?;
    }
    ctx.report_state(StateReport {
        unique_id: entity,
        state: Some(State::MediaPlayer(state)),
        attributes: std::collections::BTreeMap::new(),
        caused_by,
        replayed: false,
    });
    Ok(())
}

async fn apply_offline(
    ctx: &ProtocolContext,
    devices: &mut Devices,
    generation: u64,
    uuid: &str,
) -> Result<(), ProtocolError> {
    let Some(device) = devices.by_uuid.get_mut(uuid) else {
        return Ok(());
    };
    if device.generation != generation || !device.online {
        return Ok(());
    }
    device.online = false;
    let device_id =
        UniqueId::try_from(uuid).map_err(|error| ProtocolError::new(error.to_string()))?;
    ctx.set_availability(
        AvailabilityTarget::Device(device_id),
        Availability::Unavailable,
    )
    .await
    .map_err(|error| ProtocolError::new(error.to_string()))?;
    Ok(())
}

async fn route(call: IncomingCall, devices: &BTreeMap<String, Device>) {
    let Some(uuid) = call.call.unique_id.as_str().strip_prefix("media:") else {
        call.reply(Err(ServiceError::failed("no Cast device has that player")));
        return;
    };
    let Some(device) = devices.get(uuid) else {
        call.reply(Err(ServiceError::failed("no Cast device has that player")));
        return;
    };
    if !device.online {
        call.reply(Err(ServiceError::unavailable(
            "the Cast device can't be reached",
        )));
        return;
    }
    if let Err(error) = device.commands.try_send(call) {
        let call = match error {
            mpsc::error::TrySendError::Full(call) | mpsc::error::TrySendError::Closed(call) => call,
        };
        call.reply(Err(ServiceError::unavailable(
            "the Cast device can't be reached",
        )));
    }
}
