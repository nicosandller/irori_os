//! TP-Link Tapo and Kasa plugs and power strips on the local network.
//!
//! Devices are found by TP-Link's discovery probe and read over KLAP, their local encrypted API.
//! Each plug is a device with its relay, meter, LED and signal; a strip's outlets are devices
//! reached through it.
//!
//! **Read-only for now.** Switches report what the device says; turning one on or off is refused
//! with a message, until switching has been tested against real devices.

mod device;
mod discover;
mod http;
mod klap;
mod map;
pub mod settings;
mod smart;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use irori_protocol::{
    AvailabilityTarget, Health, IncomingCall, Protocol, ProtocolContext, ProtocolError,
    ServiceError,
};
use irori_types::{Availability, Name, State, StateReport, UniqueId, Waiting};
use tokio::sync::mpsc;

use crate::device::Event;
use crate::discover::{Discovery, Found};
use crate::klap::AuthHash;
use crate::map::Described;
use crate::settings::Settings;

/// The TP-Link protocol.
#[derive(Debug)]
pub struct Tplink;

impl Protocol for Tplink {
    type Config = Settings;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");

    async fn run(settings: Settings, ctx: ProtocolContext) -> Result<(), ProtocolError> {
        let (found, incoming) = mpsc::channel(32);
        let discovery = discover::start(&settings, found);
        let outcome = drive(settings, ctx, incoming).await;
        discovery.abort();
        outcome
    }
}

const EVENTS: usize = 64;

/// Refused while switching is untested.
const READ_ONLY: &str =
    "the TP-Link extension only reads devices for now; switch this one in the Tapo or Kasa app";

struct Device {
    generation: u64,
    found: Found,
    handle: tokio::task::JoinHandle<()>,
    online: bool,
    /// Missed readings in a row; not set before the first try.
    offline: bool,
    refused: bool,
    described: Option<Described>,
    states: BTreeMap<UniqueId, Option<State>>,
}

#[derive(Default)]
struct Devices {
    by_mac: BTreeMap<String, Device>,
    /// Found, but there's no account to read them with.
    unread: BTreeMap<String, Found>,
    /// Found, but they speak something this doesn't read yet. Logged once each.
    unsupported: BTreeMap<String, Found>,
    /// Known hosts that haven't answered.
    failed_hosts: BTreeMap<String, ()>,
    generations: u64,
}

impl Devices {
    fn health(&self) -> Health {
        let offline =
            self.by_mac.values().filter(|device| device.offline).count() + self.failed_hosts.len();
        let refused =
            self.by_mac.values().filter(|device| device.refused).count() + self.unread.len();
        let mut problems = Vec::new();
        match offline {
            0 => {}
            1 => problems.push("1 TP-Link device can't be reached".to_owned()),
            n => problems.push(format!("{n} TP-Link devices can't be reached")),
        }
        match refused {
            0 => {}
            1 => problems.push("1 needs the TP-Link account".to_owned()),
            n => problems.push(format!("{n} need the TP-Link account")),
        }
        if problems.is_empty() {
            Health::Running
        } else {
            Health::Degraded(problems.join("; "))
        }
    }

    fn waiting(&self) -> Vec<Waiting> {
        let missing = self.unread.values().map(|found| {
            (
                found,
                "it needs the TP-Link account it was set up with: `username` and `password` in this extension's settings",
            )
        });
        let refused = self
            .by_mac
            .values()
            .filter(|device| device.refused)
            .map(|device| {
                (
                    &device.found,
                    "it doesn't accept the TP-Link account in this extension's settings",
                )
            });
        missing
            .chain(refused)
            .filter_map(|(found, reason)| {
                let name = format!("{} {}", found.model, found.mac);
                Some(Waiting {
                    unique_id: UniqueId::try_from(found.mac.as_str()).ok()?,
                    name: Name::try_from(name.as_str()).ok()?,
                    reason: reason.to_owned(),
                    secret: None,
                })
            })
            .collect()
    }
}

/// Runs the protocol against an injected discovery stream. Tests use this so nothing probes the
/// real network.
pub(crate) async fn drive(
    settings: Settings,
    mut ctx: ProtocolContext,
    mut discovery: mpsc::Receiver<Discovery>,
) -> Result<(), ProtocolError> {
    let auth = settings
        .account()
        .map(|(username, password)| AuthHash::new(username, password));
    let (events_tx, mut events) = mpsc::channel(EVENTS);
    let mut devices = Devices::default();
    let mut health = Health::Running;
    let mut waiting = Vec::new();
    ctx.set_health(health.clone()).await;

    let outcome = loop {
        tokio::select! {
            call = ctx.next_call() => {
                let Some(call) = call else { break Ok(()) };
                refuse(call);
            }
            found = discovery.recv() => match found {
                Some(Discovery::Seen(found)) => {
                    adopt(found, auth, &settings, &mut devices, &events_tx);
                }
                Some(Discovery::Unreachable { host }) => {
                    let known = devices.by_mac.values().any(|device| {
                        device.found.known_host.as_deref() == Some(host.as_str())
                    });
                    if !known {
                        devices.failed_hosts.insert(host, ());
                    }
                }
                None => break Ok(()),
            },
            event = events.recv() => match event {
                Some(event) => apply(&ctx, &mut devices, event).await,
                None => break Err(ProtocolError::new("stopped reading TP-Link devices")),
            },
        }
        // A discovery round arrives as a burst: say what changed once it's all in.
        if !discovery.is_empty() || !events.is_empty() {
            continue;
        }
        let now = devices.health();
        if now != health {
            ctx.set_health(now.clone()).await;
            health = now;
        }
        let now = devices.waiting();
        if now != waiting {
            ctx.set_waiting(now.clone()).await;
            waiting = now;
        }
    };

    for device in devices.by_mac.into_values() {
        device.handle.abort();
    }
    outcome
}

fn refuse(call: IncomingCall) {
    call.reply(Err(ServiceError::failed(READ_ONLY)));
}

fn adopt(
    found: Found,
    auth: Option<AuthHash>,
    settings: &Settings,
    devices: &mut Devices,
    events: &mpsc::Sender<Event>,
) {
    if let Some(host) = &found.known_host {
        devices.failed_hosts.remove(host);
    }
    if !found.readable() {
        if !devices.unsupported.contains_key(&found.mac) {
            tracing::info!(
                "a {} ({}) speaks a TP-Link protocol this extension doesn't read yet",
                found.model,
                found.encryption
            );
            devices.unsupported.insert(found.mac.clone(), found);
        }
        return;
    }
    let Some(auth) = auth else {
        devices.unread.insert(found.mac.clone(), found);
        return;
    };
    if let Some(device) = devices.by_mac.get_mut(&found.mac) {
        if device.found.address == found.address {
            device.found.known_host = found.known_host;
            return;
        }
        // A new address: start reading it there.
        device.handle.abort();
    }
    devices.generations += 1;
    let generation = devices.generations;
    let handle = device::spawn(
        found.clone(),
        auth,
        settings.poll(),
        generation,
        events.clone(),
    );
    let previous = devices.by_mac.remove(&found.mac);
    devices.by_mac.insert(
        found.mac.clone(),
        Device {
            generation,
            found,
            handle,
            online: previous.as_ref().is_some_and(|device| device.online),
            offline: previous.as_ref().is_some_and(|device| device.offline),
            refused: false,
            described: previous
                .as_ref()
                .and_then(|device| device.described.clone()),
            states: previous.map(|device| device.states).unwrap_or_default(),
        },
    );
}

async fn apply(ctx: &ProtocolContext, devices: &mut Devices, event: Event) {
    match event {
        Event::Read {
            generation,
            mac,
            snapshot,
        } => {
            let Some(device) = devices.by_mac.get_mut(&mac) else {
                return;
            };
            if device.generation != generation {
                return;
            }
            device.refused = false;
            device.offline = false;
            let described = match map::describe(&mac, &snapshot) {
                Ok(described) => described,
                Err(error) => {
                    tracing::warn!("a TP-Link device couldn't be described: {error}");
                    return;
                }
            };
            if device.described.as_ref() != Some(&described) {
                if !describe(ctx, &described).await {
                    return;
                }
                // Describing marks everything available.
                device.online = true;
                device.described = Some(described);
            } else if !device.online {
                for id in described.device_ids() {
                    let _ = ctx
                        .set_availability(AvailabilityTarget::Device(id), Availability::Available)
                        .await;
                }
                device.online = true;
            }
            let states = match map::states(&mac, &snapshot) {
                Ok(states) => states,
                Err(error) => {
                    tracing::warn!("a TP-Link reading couldn't be used: {error}");
                    return;
                }
            };
            for (unique_id, state) in states {
                if device.states.get(&unique_id) == Some(&state) {
                    continue;
                }
                ctx.report_state(StateReport {
                    unique_id: unique_id.clone(),
                    state: state.clone(),
                    attributes: BTreeMap::new(),
                    caused_by: None,
                    replayed: false,
                });
                device.states.insert(unique_id, state);
            }
        }
        Event::Offline { generation, mac } => {
            let Some(device) = devices.by_mac.get_mut(&mac) else {
                return;
            };
            if device.generation != generation {
                return;
            }
            device.online = false;
            device.offline = true;
            mark_unavailable(ctx, device).await;
        }
        Event::Refused { generation, mac } => {
            let Some(device) = devices.by_mac.get_mut(&mac) else {
                return;
            };
            if device.generation != generation {
                return;
            }
            device.refused = true;
            device.online = false;
            mark_unavailable(ctx, device).await;
        }
    }
}

async fn describe(ctx: &ProtocolContext, described: &Described) -> bool {
    for device in &described.devices {
        if let Err(error) = ctx.describe_device(device.clone()).await {
            tracing::warn!("a TP-Link device couldn't be added: {}", error.0);
            return false;
        }
    }
    for entity in &described.entities {
        if let Err(error) = ctx.describe_entity(entity.clone()).await {
            tracing::warn!("a TP-Link entity couldn't be added: {}", error.0);
            return false;
        }
    }
    true
}

async fn mark_unavailable(ctx: &ProtocolContext, device: &mut Device) {
    let Some(described) = &device.described else {
        return;
    };
    for id in described.device_ids() {
        let _ = ctx
            .set_availability(AvailabilityTarget::Device(id), Availability::Unavailable)
            .await;
    }
    // The next reading reports every value again, so nothing stale is kept back.
    device.states.clear();
}
