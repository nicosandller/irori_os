//! Bambu Lab printers on the local network, over the printer's own MQTT (no cloud, no account).
//!
//! One printer per installed extension for now: its address, serial number and access code are
//! settings. The printer is a device with what it is doing, its progress, remaining time,
//! layers, nozzle and bed temperatures, and the filament in each slot it reports.
//!
//! **Read-only for now.** Nothing is ever sent to the printer but the request for its whole
//! status. Pausing, resuming, stopping and the light are refused with a message, until they
//! have been tried against real printers.

mod link;
mod map;
pub mod settings;
mod status;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use irori_protocol::{
    AvailabilityTarget, Health, Protocol, ProtocolContext, ProtocolError, ServiceError,
};
use irori_types::{Attributes, Availability, State, StateReport, UniqueId};

use crate::link::{LinkEvent, Timing};
use crate::map::Described;
use crate::settings::Settings;
use crate::status::Status;

/// The Bambu Lab protocol.
#[derive(Debug)]
pub struct Bambu;

impl Protocol for Bambu {
    type Config = Settings;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");
    const ICON: Option<&'static str> = Some(include_str!("../icon.svg"));

    async fn run(settings: Settings, ctx: ProtocolContext) -> Result<(), ProtocolError> {
        drive(settings, ctx, Timing::default()).await
    }
}

/// Refused while commands are untested.
const READ_ONLY: &str =
    "the Bambu Lab extension only reads the printer for now; use the printer's screen or its app";

/// The key in the extension's store for the certificate the printer last signed in with.
const PIN: &str = "certificate";

/// The certificate remembered for this printer: `{ "serial": …, "fingerprint": … }`. One kept
/// for another serial number is not this printer's.
async fn remembered(ctx: &ProtocolContext, serial: &str) -> Option<String> {
    let value = match ctx.load(PIN).await {
        Ok(value) => value?,
        Err(error) => {
            tracing::warn!("the printer's certificate couldn't be loaded: {}", error.0);
            return None;
        }
    };
    (value.get("serial")?.as_str()? == serial)
        .then(|| value.get("fingerprint")?.as_str().map(str::to_owned))?
}

#[derive(Default)]
struct Printer {
    status: Status,
    described: Option<Described>,
    available: bool,
    /// What was last reported for each entity, so only changes are reported.
    said: BTreeMap<UniqueId, (Option<State>, Attributes)>,
}

/// Runs the protocol with the given reconnect pauses. Tests use short ones.
pub(crate) async fn drive(
    settings: Settings,
    mut ctx: ProtocolContext,
    timing: Timing,
) -> Result<(), ProtocolError> {
    let mut pin = remembered(&ctx, &settings.serial).await;
    // What the owner typed wins over what was remembered: it is how a new certificate is taken.
    let expect = settings.certificate.clone().or_else(|| pin.clone());
    let (mut events, link) =
        link::connect(&settings, expect, timing).map_err(ProtocolError::new)?;
    let mut printer = Printer::default();
    let mut health = None;

    let outcome = loop {
        let now = tokio::select! {
            call = ctx.next_call() => {
                let Some(call) = call else { break Ok(()) };
                call.reply(Err(ServiceError::failed(READ_ONLY)));
                continue;
            }
            event = events.recv() => match event {
                Some(LinkEvent::Connected { certificate }) => {
                    if pin.as_deref() != Some(certificate.as_str()) {
                        let value = serde_json::json!({
                            "serial": settings.serial, "fingerprint": certificate
                        });
                        match ctx.store(PIN, value).await {
                            Ok(()) => pin = Some(certificate),
                            Err(error) => tracing::warn!(
                                "the printer's certificate couldn't be remembered: {}", error.0
                            ),
                        }
                    }
                    Health::Running
                }
                Some(LinkEvent::Report(payload)) => {
                    if printer.status.merge(&payload) {
                        show(&ctx, &settings.serial, &mut printer).await;
                    }
                    continue;
                }
                Some(LinkEvent::Down(down)) => {
                    hide(&ctx, &mut printer).await;
                    Health::Degraded(down.to_string())
                }
                None => break Err(ProtocolError::new("stopped reading the printer")),
            },
        };
        if health.as_ref() != Some(&now) {
            ctx.set_health(now.clone()).await;
            health = Some(now);
        }
    };
    link.abort();
    outcome
}

/// Describes the printer if what it has changed, marks it available, and reports what changed.
async fn show(ctx: &ProtocolContext, serial: &str, printer: &mut Printer) {
    let (described, readings) = match (
        map::describe(serial, &printer.status),
        map::readings(serial, &printer.status),
    ) {
        (Ok(described), Ok(readings)) => (described, readings),
        (Err(error), _) | (_, Err(error)) => {
            tracing::warn!("the printer's status couldn't be used: {error}");
            return;
        }
    };
    if printer.described.as_ref() != Some(&described) {
        if let Err(error) = ctx.describe_device(described.device.clone()).await {
            tracing::warn!("the printer couldn't be added: {}", error.0);
            return;
        }
        for entity in &described.entities {
            if let Err(error) = ctx.describe_entity(entity.clone()).await {
                tracing::warn!("a printer entity couldn't be added: {}", error.0);
                return;
            }
        }
        // Describing marks everything available.
        printer.available = true;
        printer.described = Some(described);
    } else if !printer.available {
        let target = AvailabilityTarget::Device(described.device.unique_id.clone());
        let _ = ctx.set_availability(target, Availability::Available).await;
        printer.available = true;
    }
    for (unique_id, state, attributes) in readings {
        let now = (state, attributes);
        if printer.said.get(&unique_id) == Some(&now) {
            continue;
        }
        ctx.report_state(StateReport {
            unique_id: unique_id.clone(),
            state: now.0.clone(),
            attributes: now.1.clone(),
            caused_by: None,
            replayed: false,
        });
        printer.said.insert(unique_id, now);
    }
}

/// Marks the printer unavailable. What it last said is kept: the next report is laid over it.
async fn hide(ctx: &ProtocolContext, printer: &mut Printer) {
    let Some(described) = &printer.described else {
        return;
    };
    if std::mem::take(&mut printer.available) {
        let target = AvailabilityTarget::Device(described.device.unique_id.clone());
        let _ = ctx
            .set_availability(target, Availability::Unavailable)
            .await;
    }
    // The next report says every value again, so nothing stale is kept back.
    printer.said.clear();
}
