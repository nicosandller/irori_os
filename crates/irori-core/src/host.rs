//! The extension host: starts built-in extensions, feeds what they say into the core, and restarts
//! them when they fail (`docs/specs/integrations.md` §3).

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Duration;

use irori_integration::Builtin;
use irori_integration::host::{HostEnd, Op, Reports, connect};
use irori_types::{EntityKind, ExtensionId, IntegrationId};
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::Instant;

use crate::{Core, ExtensionStatus};

/// Supervision timings. The defaults are the spec's; tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// Wait before the first restart; doubles after each failure.
    pub first_retry: Duration,
    /// The longest wait between restarts.
    pub max_retry: Duration,
    /// Running this long resets the wait to `first_retry`.
    pub healthy_after: Duration,
    /// How long an integration gets to finish after being told to stop.
    pub stop_grace: Duration,
}

impl Timing {
    /// The longest any single timing may be. Keeps every computed time (e.g. `retry_at`)
    /// representable, so a status never claims "no retry" while a retry is scheduled.
    pub const MAX: Duration = Duration::from_secs(24 * 60 * 60);

    fn validate(&self) -> Result<(), String> {
        let named = [
            ("first_retry", self.first_retry),
            ("max_retry", self.max_retry),
            ("healthy_after", self.healthy_after),
            ("stop_grace", self.stop_grace),
        ];
        if let Some((name, _)) = named.iter().find(|(_, d)| *d > Self::MAX) {
            return Err(format!("timing: {name} must be at most 24 hours"));
        }
        if self.first_retry.is_zero() || self.first_retry > self.max_retry {
            return Err("timing: first_retry must be above zero and at most max_retry".into());
        }
        Ok(())
    }
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            first_retry: Duration::from_secs(1),
            max_retry: Duration::from_secs(5 * 60),
            healthy_after: Duration::from_secs(10 * 60),
            stop_grace: Duration::from_secs(5),
        }
    }
}

/// Runs extensions until [`ExtensionHost::shutdown`].
#[derive(Debug)]
pub struct ExtensionHost {
    stop: watch::Sender<bool>,
    supervisors: Vec<JoinHandle<()>>,
}

impl ExtensionHost {
    /// Starts every extension, each supervised in its own task. Fails if two share an id.
    pub fn start(core: &Core, builtins: Vec<Builtin>, timing: Timing) -> Result<Self, String> {
        timing.validate()?;
        let mut ids: Vec<&ExtensionId> =
            builtins.iter().map(|b| &b.manifest.extension.id).collect();
        ids.sort();
        if let Some(pair) = ids.windows(2).find(|pair| pair[0] == pair[1]) {
            return Err(format!("two extensions share the id `{}`", pair[0]));
        }
        let (stop, stop_rx) = watch::channel(false);
        let supervisors = builtins
            .into_iter()
            .map(|builtin| {
                tokio::spawn(supervise(
                    core.clone(),
                    Arc::new(builtin),
                    timing,
                    stop_rx.clone(),
                ))
            })
            .collect();
        Ok(Self { stop, supervisors })
    }

    /// Tells every extension to stop and waits for them (each gets `stop_grace`).
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        for supervisor in self.supervisors {
            let _ = supervisor.await;
        }
    }
}

enum Outcome {
    /// We told it to stop.
    Stopped,
    /// It ended on its own; why.
    Ended(String),
}

async fn supervise(
    core: Core,
    builtin: Arc<Builtin>,
    timing: Timing,
    mut stop: watch::Receiver<bool>,
) {
    let manifest = &builtin.manifest;
    let extension = manifest.extension.id.clone();
    for warning in manifest.warnings() {
        tracing::warn!(%extension, "{warning}");
    }
    let (Some(integration), Some(contribution)) = (
        manifest.integration_id(),
        manifest.contributes.integration.first(),
    ) else {
        // `irori_integration::builtin` only builds extensions with an integration.
        core.set_status(&extension, ExtensionStatus::Disabled);
        return;
    };
    let kinds = contribution.entity_kinds.clone();
    // What it is, before anything about how it's doing: the UI names extensions by their own
    // name, not their id, and says what they're for when someone is choosing one.
    core.describe_extension(
        &extension,
        crate::ExtensionInfo {
            name: manifest.extension.name.clone(),
            description: manifest.extension.description.clone(),
            version: manifest.extension.version.clone(),
            entity_kinds: kinds.clone(),
            iot_class: Some(contribution.iot_class),
        },
    );

    if !manifest.extension.irori.matches(core.version()) {
        let reason = format!(
            "requires Irori {}, this is {}",
            manifest.extension.irori,
            core.version()
        );
        tracing::error!(%extension, "{reason}");
        core.set_status(
            &extension,
            ExtensionStatus::Failed {
                reason,
                retry_at: None,
            },
        );
        return;
    }

    let mut delay = timing.first_retry;
    loop {
        if *stop.borrow() {
            // Already stopping: don't start anything, not even the first time.
            core.set_status(&extension, ExtensionStatus::Disabled);
            return;
        }
        core.set_status(&extension, ExtensionStatus::Starting);
        let (ctx, host_end) = connect();
        // Settings come from the config dir once it exists (M0.7); until then, defaults.
        let settings = serde_json::Value::Object(serde_json::Map::new());
        // Only time spent actually running counts towards `healthy_after`; startup doesn't, and
        // a start that panics never ran at all.
        let mut running_since: Option<Instant> = None;
        // `Integration::run` may do work before returning its future; a panic there is a crash
        // like any other, not the end of supervision.
        let reason = match catch_unwind(AssertUnwindSafe(|| builtin.start(settings, ctx))) {
            Ok(Ok(run)) => {
                core.link(&integration, host_end.calls.clone());
                let task = tokio::spawn(run);
                running_since = Some(Instant::now());
                core.set_status(&extension, ExtensionStatus::Running);
                tracing::info!(%extension, "extension started");

                let outcome = pump(
                    &core,
                    &extension,
                    &integration,
                    &kinds,
                    host_end,
                    task,
                    &mut stop,
                    timing,
                )
                .await;
                core.unlink(&integration);
                core.mark_unavailable(&integration);
                match outcome {
                    Outcome::Stopped => {
                        tracing::info!(%extension, "extension stopped");
                        core.set_status(&extension, ExtensionStatus::Disabled);
                        return;
                    }
                    Outcome::Ended(reason) => reason,
                }
            }
            Ok(Err(reason)) => {
                // Invalid settings: retrying won't help until they change.
                tracing::error!(%extension, %reason, "can't start extension");
                core.set_status(
                    &extension,
                    ExtensionStatus::Failed {
                        reason,
                        retry_at: None,
                    },
                );
                return;
            }
            Err(payload) => format!("crashed while starting: {}", panic_message(payload)),
        };
        if running_since.is_some_and(|since| since.elapsed() >= timing.healthy_after) {
            delay = timing.first_retry;
        }
        let retry_at = core
            .now()
            .as_jiff()
            .checked_add(delay)
            .ok()
            .map(irori_types::Timestamp::from_jiff);
        tracing::error!(%extension, %reason, retry_in_secs = delay.as_secs_f64(), "extension failed; restarting it");
        core.set_status(&extension, ExtensionStatus::Failed { reason, retry_at });
        tokio::select! {
            // Stop first: when the wait and the stop are both ready, never start it again.
            biased;
            _ = stop.wait_for(|stop| *stop) => {
                core.set_status(&extension, ExtensionStatus::Disabled);
                return;
            }
            () = tokio::time::sleep(delay) => {}
        }
        // Saturating: a custom `Timing` with huge delays must not overflow and end supervision.
        delay = delay.saturating_mul(2).min(timing.max_retry);
    }
}

/// Feeds the integration's operations and reports into the core until it ends or we stop it.
#[allow(clippy::too_many_arguments)]
async fn pump(
    core: &Core,
    extension: &ExtensionId,
    integration: &IntegrationId,
    kinds: &[EntityKind],
    host_end: HostEnd,
    mut task: JoinHandle<Result<(), irori_integration::IntegrationError>>,
    stop: &mut watch::Receiver<bool>,
    timing: Timing,
) -> Outcome {
    let HostEnd {
        mut ops,
        reports,
        calls,
        stop: stop_integration,
    } = host_end;
    // The core's link holds its own sender; this one isn't needed.
    drop(calls);

    loop {
        tokio::select! {
            biased;
            _ = stop.wait_for(|stop| *stop) => break,
            result = &mut task => {
                drain(core, extension, integration, kinds, &mut ops, &reports);
                return Outcome::Ended(describe_end(result));
            }
            () = serve(core, extension, integration, kinds, &mut ops, &reports) => {}
        }
    }

    // Told to stop: let it finish within the grace period, still serving what it says.
    let _ = stop_integration.send(true);
    let grace = tokio::time::sleep(timing.stop_grace);
    tokio::pin!(grace);
    loop {
        tokio::select! {
            biased;
            _ = &mut task => {
                // Whatever it said on its way out still counts.
                drain(core, extension, integration, kinds, &mut ops, &reports);
                return Outcome::Stopped;
            }
            () = &mut grace => {
                tracing::warn!(%extension, "extension didn't stop in time; cancelling it");
                task.abort();
                drain(core, extension, integration, kinds, &mut ops, &reports);
                return Outcome::Stopped;
            }
            () = serve(core, extension, integration, kinds, &mut ops, &reports) => {}
        }
    }
}

/// Applies the next thing the integration says. Operations and state reports are taken in no
/// fixed order, so an integration busy with one can't starve the other.
async fn serve(
    core: &Core,
    extension: &ExtensionId,
    integration: &IntegrationId,
    kinds: &[EntityKind],
    ops: &mut mpsc::Receiver<Op>,
    reports: &Reports,
) {
    tokio::select! {
        Some(op) = ops.recv() => core.apply_op(extension, integration, kinds, op),
        // `ready` takes nothing, so losing this race can't lose reports.
        () = reports.ready() => {
            core.apply_reports(extension, integration, reports.drain(), reports.take_dropped());
        }
    }
}

/// Applies everything the integration said but the core hasn't read yet.
fn drain(
    core: &Core,
    extension: &ExtensionId,
    integration: &IntegrationId,
    kinds: &[EntityKind],
    ops: &mut mpsc::Receiver<Op>,
    reports: &Reports,
) {
    while let Ok(op) = ops.try_recv() {
        core.apply_op(extension, integration, kinds, op);
    }
    core.apply_reports(
        extension,
        integration,
        reports.drain(),
        reports.take_dropped(),
    );
}

fn describe_end(
    result: Result<Result<(), irori_integration::IntegrationError>, JoinError>,
) -> String {
    match result {
        Ok(Ok(())) => "stopped on its own without being asked to".into(),
        Ok(Err(error)) => error.to_string(),
        Err(error) if error.is_panic() => format!("crashed: {}", panic_message(error.into_panic())),
        Err(_) => "was cancelled".into(),
    }
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panicked".into())
}
