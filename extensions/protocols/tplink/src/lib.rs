//! TP-Link Tapo and Kasa plugs and power strips on the local network.
//!
//! Devices are found by TP-Link's discovery probe and read over KLAP, their local encrypted API.
//! Each plug is a device with its relay, meter, LED and signal; a strip's outlets are devices
//! reached through it.
//!
//! A plug's relay and each outlet of a strip can be turned on and off. The command goes to the
//! device, and what is shown afterwards is what the device then says, never what was asked for.
//! The LED is shown and not changed, and a device that isn't a plug or a strip is only read.

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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use irori_protocol::{
    AvailabilityTarget, Health, IncomingCall, Protocol, ProtocolContext, ProtocolError,
    ServiceError,
};
use irori_types::{Availability, Name, Service, State, StateReport, UniqueId, Waiting};
use tokio::sync::mpsc;

use crate::device::{Command, Event};
use crate::discover::{Discovery, Found};
use crate::klap::AuthHash;
use crate::map::{Described, Target};
use crate::settings::Settings;

/// The TP-Link protocol.
#[derive(Debug)]
pub struct Tplink;

impl Protocol for Tplink {
    type Config = Settings;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");
    const ICON: Option<&'static str> = Some(include_str!("../icon.svg"));

    async fn run(settings: Settings, ctx: ProtocolContext) -> Result<(), ProtocolError> {
        let (found, incoming) = mpsc::channel(32);
        let discovery = discover::start(&settings, found);
        let outcome = drive(settings, ctx, incoming, COMMAND).await;
        discovery.abort();
        outcome
    }
}

const EVENTS: usize = 64;

/// Commands one device can have waiting. More than this and it isn't keeping up.
const COMMANDS: usize = 8;
/// How long a command has, from the call's arrival, to be taken by the device. The core stops
/// waiting after ten seconds, and one exchange with a device can take five, so a login and the
/// switch together could outlast it: the call is answered here first.
const COMMAND: Duration = Duration::from_secs(8);

const NO_SUCH_SWITCH: &str = "no TP-Link device has that switch";
const LED_IS_READ: &str =
    "the TP-Link extension shows the LED and doesn't change it; set it in the Tapo or Kasa app";
const NOT_A_PLUG: &str = "the TP-Link extension switches plugs and power strips only; switch this device in the Tapo or Kasa app";
const BUSY: &str = "the TP-Link device has too many commands waiting; nothing was sent to it";

struct Device {
    generation: u64,
    found: Found,
    handle: tokio::task::JoinHandle<()>,
    online: bool,
    /// Missed two readings in a row and hasn't answered since.
    offline: bool,
    refused: bool,
    described: Option<Described>,
    states: BTreeMap<UniqueId, Option<State>>,
    /// Its switch entities, and what each is on the device.
    targets: BTreeMap<UniqueId, Target>,
    commands: mpsc::Sender<Command>,
    /// What it has metered, kept across a move to a new address.
    seen: Arc<Mutex<device::Seen>>,
}

#[derive(Default)]
struct Devices {
    by_mac: BTreeMap<String, Device>,
    /// Found, but there's no account to read them with.
    unread: BTreeMap<String, Found>,
    /// Found, but they speak something this doesn't read yet. Logged once each.
    unsupported: BTreeMap<String, Found>,
    /// Known hosts that haven't answered.
    failed_hosts: BTreeSet<String>,
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
    budget: Duration,
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
                command(call, &devices, budget);
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
                        devices.failed_hosts.insert(host);
                    }
                }
                // Discovery only ends if its task does: let the host restart the extension.
                None => break Err(ProtocolError::new("stopped finding TP-Link devices")),
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

/// Hands a call to the task of the device it's for. Nothing here waits on a device: the task
/// answers the call once the device has.
fn command(call: IncomingCall, devices: &Devices, budget: Duration) {
    let on = match call.call.service {
        Service::SwitchTurnOn => true,
        Service::SwitchTurnOff => false,
        _ => {
            call.reply(Err(ServiceError::failed(
                "a TP-Link switch only turns on and off",
            )));
            return;
        }
    };
    let found = devices.by_mac.values().find_map(|device| {
        let target = device.targets.get(&call.call.unique_id)?;
        Some((device, target))
    });
    let Some((device, target)) = found else {
        call.reply(Err(ServiceError::failed(NO_SUCH_SWITCH)));
        return;
    };
    let child = match target {
        Target::Led => {
            call.reply(Err(ServiceError::failed(LED_IS_READ)));
            return;
        }
        Target::Relay(child) => child.clone(),
    };
    if !device.found.switchable() {
        call.reply(Err(ServiceError::failed(NOT_A_PLUG)));
        return;
    }
    let command = Command {
        call,
        child,
        on,
        deadline: tokio::time::Instant::now() + budget,
    };
    if let Err(error) = device.commands.try_send(command) {
        let command = match error {
            mpsc::error::TrySendError::Full(command)
            | mpsc::error::TrySendError::Closed(command) => command,
        };
        command.call.reply(Err(ServiceError::unavailable(BUSY)));
    }
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
                "a {} ({}, {}) speaks a TP-Link protocol this extension doesn't read yet",
                found.model,
                found.device_type,
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
    // Which sensors a device has comes from what it has metered, and removing one deletes its
    // history: a device read before keeps that record at its new address, where the first
    // reading may miss.
    let previous = devices.by_mac.remove(&found.mac);
    let seen = previous
        .as_ref()
        .map(|device| Arc::clone(&device.seen))
        .unwrap_or_default();
    devices.generations += 1;
    let generation = devices.generations;
    let (commands, queued) = mpsc::channel(COMMANDS);
    let handle = device::spawn(
        found.clone(),
        auth,
        settings.poll(),
        generation,
        events.clone(),
        queued,
        Arc::clone(&seen),
    );
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
            targets: previous
                .as_ref()
                .map(|device| device.targets.clone())
                .unwrap_or_default(),
            states: previous.map(|device| device.states).unwrap_or_default(),
            commands,
            seen,
        },
    );
}

async fn apply(ctx: &ProtocolContext, devices: &mut Devices, event: Event) {
    match event {
        Event::Read {
            generation,
            mac,
            snapshot,
            answers,
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
                if let Some(before) = &device.described {
                    remove_gone(ctx, before, &described, &mut device.states).await;
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
            match map::targets(&mac, &snapshot) {
                Ok(targets) => device.targets = targets,
                Err(error) => {
                    tracing::warn!("a TP-Link device's switches couldn't be read: {error}")
                }
            }
            let states = match map::states(&mac, &snapshot) {
                Ok(states) => states,
                Err(error) => {
                    tracing::warn!("a TP-Link reading couldn't be used: {error}");
                    return;
                }
            };
            for (unique_id, state) in states {
                // The switch a command was for is reported even when it didn't change: the
                // device's answer to the call, which is what the core is waiting to hear.
                let caused_by = answers
                    .iter()
                    .rfind(|(switched, _)| *switched == unique_id)
                    .map(|(_, call)| call.clone());
                if caused_by.is_none() && device.states.get(&unique_id) == Some(&state) {
                    continue;
                }
                ctx.report_state(StateReport {
                    unique_id: unique_id.clone(),
                    state: state.clone(),
                    attributes: BTreeMap::new(),
                    caused_by,
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

/// Takes away the entities an earlier description had and this one doesn't, such as a meter the
/// device turned out not to have: the host keeps an entity until it's removed.
async fn remove_gone(
    ctx: &ProtocolContext,
    before: &Described,
    now: &Described,
    states: &mut BTreeMap<UniqueId, Option<State>>,
) {
    let kept: BTreeSet<&UniqueId> = now
        .entities
        .iter()
        .map(|entity| &entity.unique_id)
        .collect();
    for entity in &before.entities {
        if kept.contains(&entity.unique_id) {
            continue;
        }
        if let Err(error) = ctx.remove_entity(entity.unique_id.clone()).await {
            tracing::warn!("a TP-Link entity couldn't be removed: {}", error.0);
        }
        states.remove(&entity.unique_id);
    }
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
