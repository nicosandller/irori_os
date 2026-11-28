//! AirGradient air monitors on the local network.
//!
//! Each monitor listed in the settings is asked for its current readings over its own local HTTP
//! API, and shows as one device with a sensor per reading it gives. Nothing talks to
//! AirGradient's cloud.
//!
//! **Read-only.** A monitor's own settings (its LED bar, its display, calibration) are never
//! changed: nothing is ever sent to a monitor but the request for its readings.

mod http;
mod map;
mod measures;
mod monitor;
pub mod settings;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use irori_protocol::{
    AvailabilityTarget, Health, IncomingCall, Protocol, ProtocolContext, ProtocolError,
    ServiceError,
};
use irori_types::{Availability, State, StateReport, UniqueId};
use tokio::sync::mpsc;

use crate::map::Described;
use crate::measures::Reading;
use crate::monitor::{Event, Target};
use crate::settings::Settings;

/// The AirGradient protocol.
#[derive(Debug)]
pub struct AirGradient;

impl Protocol for AirGradient {
    type Config = Settings;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");
    const ICON: Option<&'static str> = Some(include_str!("../icon.svg"));

    async fn run(settings: Settings, ctx: ProtocolContext) -> Result<(), ProtocolError> {
        let targets = settings
            .known_hosts
            .iter()
            .map(|host| Target {
                host: host.clone(),
                port: monitor::PORT,
            })
            .collect();
        drive(settings.poll(), ctx, targets).await
    }
}

const EVENTS: usize = 64;

/// Sensors take no commands; anything sent is refused.
const READ_ONLY: &str = "the AirGradient extension only reads monitors";

/// Shown while the settings list no monitor: there is no search to wait for.
const NO_HOSTS: &str =
    "no monitors listed: add each monitor's address to `known_hosts` in this extension's settings";

/// The most monitors described in one run. Each known host is one monitor, but what it calls
/// itself is its own word: one that gave a new serial number at every reading would otherwise add
/// a device each time.
const MAX_MONITORS: usize = settings::MAX_HOSTS;

/// One entry of `known_hosts`.
#[derive(Debug, Default)]
struct Host {
    /// The serial number its last answer gave.
    serial: Option<String>,
    /// Missed two readings in a row and hasn't answered since.
    silent: bool,
}

/// One monitor, by the serial number it reports.
#[derive(Debug)]
struct Monitor {
    /// The host its latest reading came through. Two known hosts can be one monitor (its name
    /// and its address, say): it is one device, and it is only gone when the host that last
    /// reached it goes quiet.
    host: usize,
    online: bool,
    /// Every reading it has given since the extension started.
    seen: BTreeSet<&'static str>,
    described: Option<Described>,
    states: BTreeMap<UniqueId, Option<State>>,
}

#[derive(Debug, Default)]
struct Monitors {
    hosts: Vec<Host>,
    by_serial: BTreeMap<String, Monitor>,
    /// Said once: a monitor was passed over because there are already `MAX_MONITORS`.
    full: bool,
}

impl Monitors {
    fn health(&self) -> Health {
        if self.hosts.is_empty() {
            return Health::Degraded(NO_HOSTS.to_owned());
        }
        match self.hosts.iter().filter(|host| host.silent).count() {
            0 => Health::Running,
            1 => Health::Degraded("1 AirGradient monitor isn't answering".to_owned()),
            n => Health::Degraded(format!("{n} AirGradient monitors aren't answering")),
        }
    }
}

/// Runs the protocol against the given targets, reading each one every `poll`. Tests pass a fake
/// monitor's loopback port and a short poll, so nothing reaches the real network.
pub(crate) async fn drive(
    poll: Duration,
    mut ctx: ProtocolContext,
    targets: Vec<Target>,
) -> Result<(), ProtocolError> {
    let (events_tx, mut events) = mpsc::channel(EVENTS);
    let mut monitors = Monitors::default();
    let handles: Vec<_> = targets
        .into_iter()
        .take(settings::MAX_HOSTS)
        .enumerate()
        .map(|(host, target)| {
            monitors.hosts.push(Host::default());
            monitor::spawn(host, target, poll, events_tx.clone())
        })
        .collect();
    let mut health = monitors.health();
    ctx.set_health(health.clone()).await;

    let outcome = loop {
        tokio::select! {
            call = ctx.next_call() => {
                let Some(call) = call else { break Ok(()) };
                refuse(call);
            }
            event = events.recv() => match event {
                Some(event) => apply(&ctx, &mut monitors, event).await,
                None => break Err(ProtocolError::new("stopped reading AirGradient monitors")),
            },
        }
        if !events.is_empty() {
            continue;
        }
        let now = monitors.health();
        if now != health {
            ctx.set_health(now.clone()).await;
            health = now;
        }
    };

    for handle in handles {
        handle.abort();
    }
    outcome
}

fn refuse(call: IncomingCall) {
    call.reply(Err(ServiceError::failed(READ_ONLY)));
}

async fn apply(ctx: &ProtocolContext, monitors: &mut Monitors, event: Event) {
    match event {
        Event::Read { host, reading } => read(ctx, monitors, host, &reading).await,
        Event::Silent { host } => {
            let Some(entry) = monitors.hosts.get_mut(host) else {
                return;
            };
            entry.silent = true;
            let serial = entry.serial.clone();
            leave(ctx, monitors, host, serial.as_deref()).await;
        }
    }
}

/// Marks the monitor last read through `host` unavailable. One that another host has reached
/// since is still there, and is left alone.
async fn leave(ctx: &ProtocolContext, monitors: &mut Monitors, host: usize, serial: Option<&str>) {
    let Some(monitor) = serial.and_then(|serial| monitors.by_serial.get_mut(serial)) else {
        return;
    };
    if monitor.host != host {
        return;
    }
    monitor.online = false;
    // The next reading reports every value again, so nothing stale is kept back.
    monitor.states.clear();
    if let Some(described) = &monitor.described {
        let target = AvailabilityTarget::Device(described.device.unique_id.clone());
        let _ = ctx
            .set_availability(target, Availability::Unavailable)
            .await;
    }
}

async fn read(ctx: &ProtocolContext, monitors: &mut Monitors, host: usize, reading: &Reading) {
    let Some(entry) = monitors.hosts.get_mut(host) else {
        return;
    };
    entry.silent = false;
    let before = entry.serial.replace(reading.serial.clone());
    // A different monitor now answers at this address: the one that did is no longer heard, and
    // none of this reading goes to its sensors.
    if before
        .as_deref()
        .is_some_and(|serial| serial != reading.serial)
    {
        leave(ctx, monitors, host, before.as_deref()).await;
    }

    if !monitors.by_serial.contains_key(&reading.serial) && monitors.by_serial.len() >= MAX_MONITORS
    {
        if !monitors.full {
            monitors.full = true;
            tracing::warn!(
                "more than {MAX_MONITORS} AirGradient serial numbers have answered; any further one is passed over until the extension restarts"
            );
        }
        return;
    }
    let monitor = monitors
        .by_serial
        .entry(reading.serial.clone())
        .or_insert_with(|| Monitor {
            host,
            online: false,
            seen: BTreeSet::new(),
            described: None,
            states: BTreeMap::new(),
        });
    monitor.host = host;
    monitor.seen.extend(reading.values.keys().copied());

    let described = match map::describe(reading, &monitor.seen) {
        Ok(described) => described,
        Err(error) => {
            tracing::warn!("an AirGradient monitor couldn't be described: {error}");
            return;
        }
    };
    if monitor.described.as_ref() != Some(&described) {
        if !describe(ctx, &described).await {
            return;
        }
        // Describing marks everything available.
        monitor.online = true;
        monitor.described = Some(described);
    } else if !monitor.online {
        let target = AvailabilityTarget::Device(described.device.unique_id.clone());
        let _ = ctx.set_availability(target, Availability::Available).await;
        monitor.online = true;
    }

    let states = match map::states(reading, &monitor.seen) {
        Ok(states) => states,
        Err(error) => {
            tracing::warn!("an AirGradient reading couldn't be used: {error}");
            return;
        }
    };
    for (unique_id, state) in states {
        if monitor.states.get(&unique_id) == Some(&state) {
            continue;
        }
        ctx.report_state(StateReport {
            unique_id: unique_id.clone(),
            state: state.clone(),
            attributes: BTreeMap::new(),
            caused_by: None,
            replayed: false,
        });
        monitor.states.insert(unique_id, state);
    }
}

async fn describe(ctx: &ProtocolContext, described: &Described) -> bool {
    if let Err(error) = ctx.describe_device(described.device.clone()).await {
        tracing::warn!("an AirGradient monitor couldn't be added: {}", error.0);
        return false;
    }
    for entity in &described.entities {
        if let Err(error) = ctx.describe_entity(entity.clone()).await {
            tracing::warn!("an AirGradient sensor couldn't be added: {}", error.0);
            return false;
        }
    }
    true
}
