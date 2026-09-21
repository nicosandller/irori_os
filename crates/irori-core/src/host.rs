//! The extension host: starts built-in extensions, feeds what they say into the core, and restarts
//! them when they fail (`docs/specs/protocols.md` §3).

use std::any::Any;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use irori_protocol::Builtin;
use irori_protocol::host::{HostEnd, Op, Reports, connect};
use irori_protocol::{ExtProcess, FromExt, IncomingCall, ToExt, spawn};
use std::collections::BTreeSet;

use irori_types::{EntityKind, ExtensionId, ExtensionManifest, ExtensionSettings, ProtocolId};
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
    /// How long an protocol gets to finish after being told to stop.
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
#[derive(Clone, Debug)]
pub struct ExtensionHost {
    inner: Arc<HostInner>,
}

struct HostInner {
    core: Core,
    timing: Timing,
    stop: watch::Sender<bool>,
    packages_dir: PathBuf,
    running: std::sync::Mutex<BTreeMap<ExtensionId, Running>>,
}

struct Running {
    removed: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl std::fmt::Debug for HostInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostInner")
            .field("packages_dir", &self.packages_dir)
            .finish_non_exhaustive()
    }
}

impl ExtensionHost {
    /// Starts every built-in, each supervised in its own task. Fails if two share an id.
    pub fn start(core: &Core, builtins: Vec<Builtin>, timing: Timing) -> Result<Self, String> {
        Self::start_with_packages(core, builtins, timing, PathBuf::new())
    }

    /// Starts built-ins and any packages already installed under `packages_dir`.
    pub fn start_with_packages(
        core: &Core,
        builtins: Vec<Builtin>,
        timing: Timing,
        packages_dir: PathBuf,
    ) -> Result<Self, String> {
        timing.validate()?;
        let mut ids: Vec<&ExtensionId> =
            builtins.iter().map(|b| &b.manifest.extension.id).collect();
        ids.sort();
        if let Some(pair) = ids.windows(2).find(|pair| pair[0] == pair[1]) {
            return Err(format!("two extensions share the id `{}`", pair[0]));
        }
        let (stop, stop_rx) = watch::channel(false);
        let host = Self {
            inner: Arc::new(HostInner {
                core: core.clone(),
                timing,
                stop,
                packages_dir: packages_dir.clone(),
                running: std::sync::Mutex::default(),
            }),
        };
        for builtin in builtins {
            let id = builtin.manifest.extension.id.clone();
            let (removed, task_stop) = arm_stop(stop_rx.clone());
            let task = tokio::spawn(supervise(
                core.clone(),
                Arc::new(builtin),
                timing,
                task_stop,
            ));
            host.remember(id, removed, task);
        }
        if packages_dir.is_dir() {
            for package in installed_packages(&packages_dir)? {
                host.spawn_package(package)?;
            }
        }
        Ok(host)
    }

    /// Copies are cheap; shutdown stops every supervisor regardless of how many handles remain.
    pub async fn shutdown(&self) {
        let _ = self.inner.stop.send(true);
        let tasks: Vec<JoinHandle<()>> = {
            let mut running = self
                .inner
                .running
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *running)
                .into_values()
                .map(|r| r.task)
                .collect()
        };
        for task in tasks {
            let _ = task.await;
        }
    }

    /// Starts a package that has already been written under `packages_dir/<id>/`, moving it
    /// there first if it came from elsewhere. Keeps `uninstall` and the next start able to
    /// find the one directory an extension lives in.
    pub fn install_package(&self, dir: PathBuf) -> Result<ExtensionId, String> {
        let manifest = read_package_manifest(&dir)?;
        let id = manifest.extension.id.clone();
        if self.is_running(&id) {
            return Err(format!("`{id}` is already running"));
        }
        let home = self.inner.packages_dir.join(id.as_str());
        if home != dir {
            if home.exists() {
                return Err(format!("`{id}` is already installed"));
            }
            std::fs::rename(&dir, &home)
                .map_err(|e| format!("couldn't move {dir:?} into place: {e}"))?;
        }
        self.spawn_package(home)?;
        Ok(id)
    }

    /// Stops the extension, removes its devices, forgets it, and deletes its package directory.
    pub async fn uninstall(&self, id: &ExtensionId) -> Result<(), String> {
        let task = {
            let mut running = self
                .inner
                .running
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            running.remove(id).map(|r| {
                let _ = r.removed.send(true);
                r.task
            })
        };
        if let Some(task) = task {
            let _ = task.await;
        }
        let protocol = ProtocolId::try_from(id.as_str()).map_err(|e| e.to_string())?;
        self.inner.core.remove_protocol(&protocol);
        self.inner.core.clear_extension_storage(id);
        self.inner.core.forget_extension(id);
        let dir = self.inner.packages_dir.join(id.as_str());
        if dir.is_dir() {
            std::fs::remove_dir_all(&dir)
                .map_err(|e| format!("couldn't delete {}: {e}", dir.display()))?;
        }
        Ok(())
    }

    pub fn packages_dir(&self) -> &Path {
        &self.inner.packages_dir
    }

    fn is_running(&self, id: &ExtensionId) -> bool {
        self.inner
            .running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(id)
    }

    fn remember(&self, id: ExtensionId, removed: watch::Sender<bool>, task: JoinHandle<()>) {
        self.inner
            .running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, Running { removed, task });
    }

    fn spawn_package(&self, dir: PathBuf) -> Result<(), String> {
        let manifest = read_package_manifest(&dir)?;
        let id = manifest.extension.id.clone();
        if self.is_running(&id) {
            return Err(format!("two extensions share the id `{id}`"));
        }
        let (removed, stop) = arm_stop(self.inner.stop.subscribe());
        let task = tokio::spawn(supervise_package(
            self.inner.core.clone(),
            dir,
            manifest,
            self.inner.timing,
            stop,
        ));
        self.remember(id, removed, task);
        Ok(())
    }
}

fn arm_stop(mut global: watch::Receiver<bool>) -> (watch::Sender<bool>, watch::Receiver<bool>) {
    let (removed_tx, mut removed_rx) = watch::channel(false);
    let (local_tx, local_rx) = watch::channel(false);
    tokio::spawn(async move {
        tokio::select! {
            _ = global.wait_for(|stop| *stop) => {}
            _ = removed_rx.wait_for(|stop| *stop) => {}
        }
        let _ = local_tx.send(true);
    });
    (removed_tx, local_rx)
}

enum Outcome {
    /// We told it to stop.
    Stopped,
    /// Its settings changed, so it was stopped to be started again with them.
    Reconfigured,
    /// A person turned it off.
    Disabled,
    /// It ended on its own; why.
    Ended(String),
}

/// Resolves once this extension's settings are no longer `started_with`. Only its own table
/// counts: another extension's key arriving is no reason to restart this one.
async fn settings_changed(
    settings: &mut watch::Receiver<ExtensionSettings>,
    extension: &ExtensionId,
    started_with: &serde_json::Value,
) {
    if settings
        .wait_for(|all| &all.of(extension) != started_with)
        .await
        .is_err()
    {
        // The core is gone, and with it any chance of new settings.
        std::future::pending::<()>().await;
    }
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
    let (Some(protocol), Some(contribution)) = (
        manifest.protocol_id(),
        manifest.contributes.protocol.first(),
    ) else {
        // `irori_protocol::builtin` only builds extensions with an protocol.
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
            icon: builtin.icon.map(str::to_owned),
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

    let mut settings = core.extension_settings();
    let mut disabled = core.disabled_extensions();
    let mut delay = timing.first_retry;
    loop {
        // Turned off: stay off, without counting it as a failure, until it's turned back on.
        if disabled.borrow_and_update().contains(&extension) {
            core.set_status(&extension, ExtensionStatus::Disabled);
            tokio::select! {
                biased;
                _ = stop.wait_for(|stop| *stop) => return,
                result = disabled.wait_for(|disabled| !disabled.contains(&extension)) => {
                    if result.is_err() {
                        return;
                    }
                    tracing::info!(%extension, "turned on; starting extension");
                    delay = timing.first_retry;
                    continue;
                }
            }
        }
        if *stop.borrow() {
            // Already stopping: don't start anything, not even the first time.
            core.set_status(&extension, ExtensionStatus::Disabled);
            return;
        }
        core.set_status(&extension, ExtensionStatus::Starting);
        let (ctx, host_end) = connect();
        // Whatever the config dir says right now; a later change restarts it (below).
        let started_with = settings.borrow_and_update().of(&extension);
        // Only time spent actually running counts towards `healthy_after`; startup doesn't, and
        // a start that panics never ran at all.
        let mut running_since: Option<Instant> = None;
        // `Protocol::run` may do work before returning its future; a panic there is a crash
        // like any other, not the end of supervision.
        let reason = match catch_unwind(AssertUnwindSafe(|| {
            builtin.start(started_with.clone(), ctx)
        })) {
            Ok(Ok(run)) => {
                core.link(&protocol, host_end.calls.clone());
                let task = tokio::spawn(run);
                running_since = Some(Instant::now());
                core.set_status(&extension, ExtensionStatus::Running);
                tracing::info!(%extension, "extension started");

                let outcome = pump(
                    &core,
                    &extension,
                    &protocol,
                    &kinds,
                    host_end,
                    task,
                    Watching {
                        stop: &mut stop,
                        settings: &mut settings,
                        disabled: &mut disabled,
                        started_with: &started_with,
                    },
                    timing,
                )
                .await;
                core.unlink(&protocol);
                core.mark_unavailable(&protocol);
                core.set_waiting(&extension, Vec::new());
                match outcome {
                    Outcome::Stopped => {
                        tracing::info!(%extension, "extension stopped");
                        core.set_status(&extension, ExtensionStatus::Disabled);
                        return;
                    }
                    Outcome::Disabled => {
                        tracing::info!(%extension, "turned off; extension stopped");
                        continue;
                    }
                    Outcome::Reconfigured => {
                        // Not a failure, so no backoff: somebody changed its settings and
                        // expects to see the result.
                        tracing::info!(%extension, "settings changed; restarting extension");
                        delay = timing.first_retry;
                        continue;
                    }
                    Outcome::Ended(reason) => reason,
                }
            }
            Ok(Err(reason)) => {
                // Invalid settings: retrying won't help until they change, so wait for that
                // rather than giving up for good. The reason names what's wrong, never a value.
                tracing::error!(%extension, %reason, "can't start extension");
                core.set_status(
                    &extension,
                    ExtensionStatus::Failed {
                        reason,
                        retry_at: None,
                    },
                );
                tokio::select! {
                    biased;
                    _ = stop.wait_for(|stop| *stop) => {
                        core.set_status(&extension, ExtensionStatus::Disabled);
                        return;
                    }
                    () = settings_changed(&mut settings, &extension, &started_with) => {
                        tracing::info!(%extension, "settings changed; trying again");
                        continue;
                    }
                    // Turned off while its settings were wrong: the top of the loop says so.
                    Ok(_) = disabled.wait_for(|disabled| disabled.contains(&extension)) => {
                        continue;
                    }
                }
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
            () = settings_changed(&mut settings, &extension, &started_with) => {
                // New settings, not another crash: start again now, no backoff
                // (`docs/specs/protocols.md` §3 step 6).
                tracing::info!(%extension, "settings changed; restarting extension");
                delay = timing.first_retry;
                continue;
            }
            Ok(_) = disabled.wait_for(|disabled| disabled.contains(&extension)) => {
                continue;
            }
            () = tokio::time::sleep(delay) => {}
        }
        // Saturating: a custom `Timing` with huge delays must not overflow and end supervision.
        delay = delay.saturating_mul(2).min(timing.max_retry);
    }
}

/// What ends a running extension from outside: being told to stop, or its settings changing.
struct Watching<'a> {
    stop: &'a mut watch::Receiver<bool>,
    settings: &'a mut watch::Receiver<ExtensionSettings>,
    disabled: &'a mut watch::Receiver<BTreeSet<ExtensionId>>,
    started_with: &'a serde_json::Value,
}

/// Feeds the protocol's operations and reports into the core until it ends or we stop it.
#[allow(clippy::too_many_arguments)]
async fn pump(
    core: &Core,
    extension: &ExtensionId,
    protocol: &ProtocolId,
    kinds: &[EntityKind],
    host_end: HostEnd,
    mut task: JoinHandle<Result<(), irori_protocol::ProtocolError>>,
    watching: Watching<'_>,
    timing: Timing,
) -> Outcome {
    let Watching {
        stop,
        settings,
        disabled,
        started_with,
    } = watching;
    let HostEnd {
        mut ops,
        reports,
        calls,
        stop: stop_protocol,
    } = host_end;
    // The core's link holds its own sender; this one isn't needed.
    drop(calls);

    let why = loop {
        tokio::select! {
            biased;
            _ = stop.wait_for(|stop| *stop) => break Outcome::Stopped,
            result = &mut task => {
                drain(core, extension, protocol, kinds, &mut ops, &reports);
                return Outcome::Ended(describe_end(result));
            }
            () = settings_changed(settings, extension, started_with) => {
                break Outcome::Reconfigured;
            }
            Ok(_) = disabled.wait_for(|disabled| disabled.contains(extension)) => {
                break Outcome::Disabled;
            }
            () = serve(core, extension, protocol, kinds, &mut ops, &reports) => {}
        }
    };

    // Told to stop: let it finish within the grace period, still serving what it says.
    let _ = stop_protocol.send(true);
    let grace = tokio::time::sleep(timing.stop_grace);
    tokio::pin!(grace);
    loop {
        tokio::select! {
            biased;
            _ = &mut task => {
                // Whatever it said on its way out still counts.
                drain(core, extension, protocol, kinds, &mut ops, &reports);
                return why;
            }
            () = &mut grace => {
                tracing::warn!(%extension, "extension didn't stop in time; cancelling it");
                task.abort();
                drain(core, extension, protocol, kinds, &mut ops, &reports);
                return why;
            }
            () = serve(core, extension, protocol, kinds, &mut ops, &reports) => {}
        }
    }
}

/// Applies the next thing the protocol says. Operations and state reports are taken in no
/// fixed order, so an protocol busy with one can't starve the other.
async fn serve(
    core: &Core,
    extension: &ExtensionId,
    protocol: &ProtocolId,
    kinds: &[EntityKind],
    ops: &mut mpsc::Receiver<Op>,
    reports: &Reports,
) {
    tokio::select! {
        Some(op) = ops.recv() => core.apply_op(extension, protocol, kinds, op),
        // `ready` takes nothing, so losing this race can't lose reports.
        () = reports.ready() => {
            core.apply_reports(extension, protocol, reports.drain(), reports.take_dropped());
        }
    }
}

/// Applies everything the protocol said but the core hasn't read yet.
fn drain(
    core: &Core,
    extension: &ExtensionId,
    protocol: &ProtocolId,
    kinds: &[EntityKind],
    ops: &mut mpsc::Receiver<Op>,
    reports: &Reports,
) {
    while let Ok(op) = ops.try_recv() {
        core.apply_op(extension, protocol, kinds, op);
    }
    core.apply_reports(extension, protocol, reports.drain(), reports.take_dropped());
}

fn describe_end(result: Result<Result<(), irori_protocol::ProtocolError>, JoinError>) -> String {
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

fn installed_packages(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut packages = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        // Only a package id names a managed package. A leftover staging or download dir isn't
        // a slug, so a crash in the middle of an install can't make the next start run it.
        if ExtensionId::try_from(name).is_err() {
            continue;
        }
        let path = entry.path();
        if path.join("irori-extension.toml").is_file() {
            packages.push(path);
        }
    }
    packages.sort();
    Ok(packages)
}

fn read_package_manifest(dir: &Path) -> Result<ExtensionManifest, String> {
    let path = dir.join("irori-extension.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    irori_protocol::parse_manifest(&text)
}

async fn supervise_package(
    core: Core,
    dir: PathBuf,
    manifest: ExtensionManifest,
    timing: Timing,
    mut stop: watch::Receiver<bool>,
) {
    let extension = manifest.extension.id.clone();
    for warning in manifest.warnings() {
        tracing::warn!(%extension, "{warning}");
    }
    let (Some(protocol), Some(contribution)) = (
        manifest.protocol_id(),
        manifest.contributes.protocol.first(),
    ) else {
        core.set_status(&extension, crate::ExtensionStatus::Disabled);
        return;
    };
    let Some(run) = contribution.run.clone() else {
        let reason = "external packages need `run.command` in the manifest".to_owned();
        tracing::error!(%extension, "{reason}");
        core.set_status(
            &extension,
            crate::ExtensionStatus::Failed {
                reason,
                retry_at: None,
            },
        );
        return;
    };
    let kinds = contribution.entity_kinds.clone();
    let icon = manifest.extension.icon.as_ref().and_then(|path| {
        std::fs::read_to_string(dir.join(path.as_str()))
            .ok()
            .filter(|svg| svg.trim_start().starts_with("<svg"))
    });
    core.describe_extension(
        &extension,
        crate::ExtensionInfo {
            name: manifest.extension.name.clone(),
            description: manifest.extension.description.clone(),
            version: manifest.extension.version.clone(),
            entity_kinds: kinds.clone(),
            iot_class: Some(contribution.iot_class),
            icon,
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
            crate::ExtensionStatus::Failed {
                reason,
                retry_at: None,
            },
        );
        return;
    }

    let mut settings = core.extension_settings();
    let mut disabled = core.disabled_extensions();
    let mut delay = timing.first_retry;
    loop {
        if disabled.borrow_and_update().contains(&extension) {
            core.set_status(&extension, crate::ExtensionStatus::Disabled);
            tokio::select! {
                biased;
                _ = stop.wait_for(|stop| *stop) => return,
                result = disabled.wait_for(|disabled| !disabled.contains(&extension)) => {
                    if result.is_err() {
                        return;
                    }
                    delay = timing.first_retry;
                    continue;
                }
            }
        }
        if *stop.borrow() {
            core.set_status(&extension, crate::ExtensionStatus::Disabled);
            return;
        }
        core.set_status(&extension, crate::ExtensionStatus::Starting);
        let started_with = settings.borrow_and_update().of(&extension);
        let mut child = match spawn(&dir, &run) {
            Ok(child) => child,
            Err(reason) => {
                tracing::error!(%extension, %reason, "can't start extension");
                core.set_status(
                    &extension,
                    crate::ExtensionStatus::Failed {
                        reason,
                        retry_at: None,
                    },
                );
                tokio::select! {
                    biased;
                    _ = stop.wait_for(|stop| *stop) => return,
                    () = settings_changed(&mut settings, &extension, &started_with) => continue,
                    Ok(_) = disabled.wait_for(|disabled| disabled.contains(&extension)) => continue,
                }
            }
        };
        if let Err(reason) = child
            .send(&ToExt::Hello {
                settings: started_with.clone(),
            })
            .await
        {
            tracing::error!(%extension, %reason, "can't talk to extension");
            let _ = child.child.start_kill();
            core.set_status(
                &extension,
                crate::ExtensionStatus::Failed {
                    reason,
                    retry_at: None,
                },
            );
            tokio::select! {
                biased;
                _ = stop.wait_for(|stop| *stop) => return,
                () = tokio::time::sleep(delay) => {}
            }
            delay = delay.saturating_mul(2).min(timing.max_retry);
            continue;
        }

        let (calls_tx, calls_rx) = mpsc::channel(64);
        core.link(&protocol, calls_tx);
        core.set_status(&extension, crate::ExtensionStatus::Running);
        tracing::info!(%extension, "extension started");
        let outcome = pump_process(
            &core,
            &extension,
            &protocol,
            &kinds,
            &mut child,
            calls_rx,
            Watching {
                stop: &mut stop,
                settings: &mut settings,
                disabled: &mut disabled,
                started_with: &started_with,
            },
            timing,
        )
        .await;
        core.unlink(&protocol);
        core.mark_unavailable(&protocol);
        core.set_waiting(&extension, Vec::new());
        let _ = child.send(&ToExt::Stop).await;
        let _ = child.child.start_kill();
        match outcome {
            Outcome::Stopped => {
                tracing::info!(%extension, "extension stopped");
                core.set_status(&extension, crate::ExtensionStatus::Disabled);
                return;
            }
            Outcome::Disabled => {
                tracing::info!(%extension, "turned off; extension stopped");
                continue;
            }
            Outcome::Reconfigured => {
                tracing::info!(%extension, "settings changed; restarting extension");
                delay = timing.first_retry;
                continue;
            }
            Outcome::Ended(reason) => {
                tracing::error!(%extension, %reason, "extension failed; restarting it");
                let retry_at = core
                    .now()
                    .as_jiff()
                    .checked_add(delay)
                    .ok()
                    .map(irori_types::Timestamp::from_jiff);
                core.set_status(
                    &extension,
                    crate::ExtensionStatus::Failed { reason, retry_at },
                );
                tokio::select! {
                    biased;
                    _ = stop.wait_for(|stop| *stop) => {
                        core.set_status(&extension, crate::ExtensionStatus::Disabled);
                        return;
                    }
                    () = settings_changed(&mut settings, &extension, &started_with) => {
                        delay = timing.first_retry;
                        continue;
                    }
                    Ok(_) = disabled.wait_for(|disabled| disabled.contains(&extension)) => continue,
                    () = tokio::time::sleep(delay) => {}
                }
                delay = delay.saturating_mul(2).min(timing.max_retry);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn pump_process(
    core: &Core,
    extension: &ExtensionId,
    protocol: &ProtocolId,
    kinds: &[EntityKind],
    proc: &mut ExtProcess,
    mut calls: mpsc::Receiver<IncomingCall>,
    watching: Watching<'_>,
    timing: Timing,
) -> Outcome {
    use std::collections::HashMap;

    let Watching {
        stop,
        settings,
        disabled,
        started_with,
    } = watching;
    let mut pending_calls: HashMap<u64, IncomingCall> = HashMap::new();
    let mut next_call: u64 = 0;
    let ExtProcess {
        child,
        stdin,
        stdout,
    } = proc;

    let why = loop {
        tokio::select! {
            biased;
            () = async { let _ = stop.wait_for(|stop| *stop).await; } => break Outcome::Stopped,
            status = child.wait() => {
                return Outcome::Ended(match status {
                    Ok(status) if status.success() => {
                        "stopped on its own without being asked to".into()
                    }
                    Ok(status) => format!("exited {status}"),
                    Err(e) => e.to_string(),
                });
            }
            () = settings_changed(settings, extension, started_with) => {
                break Outcome::Reconfigured;
            }
            () = async {
                let _ = disabled.wait_for(|disabled| disabled.contains(extension)).await;
            } => break Outcome::Disabled,
            Some(incoming) = calls.recv() => {
                let id = next_call;
                next_call += 1;
                let call = incoming.call.clone();
                pending_calls.insert(id, incoming);
                if let Err(reason) = ExtProcess::send_on(stdin, &ToExt::ServiceCall { id, call }).await {
                    return Outcome::Ended(reason);
                }
            }
            msg = ExtProcess::recv_on(stdout) => {
                match msg {
                    Ok(from) => {
                        if let Err(reason) = apply_from_ext(
                            core, extension, protocol, kinds, stdin, from, &mut pending_calls,
                        ).await {
                            return Outcome::Ended(reason);
                        }
                    }
                    Err(reason) => return Outcome::Ended(reason),
                }
            }
        }
    };

    let _ = ExtProcess::send_on(stdin, &ToExt::Stop).await;
    let grace = tokio::time::sleep(timing.stop_grace);
    tokio::pin!(grace);
    loop {
        tokio::select! {
            biased;
            status = child.wait() => {
                let _ = status;
                return why;
            }
            () = &mut grace => {
                tracing::warn!(%extension, "extension didn't stop in time; cancelling it");
                let _ = child.start_kill();
                return why;
            }
            msg = ExtProcess::recv_on(stdout) => {
                if let Ok(from) = msg {
                    let _ = apply_from_ext(
                        core, extension, protocol, kinds, stdin, from, &mut pending_calls,
                    ).await;
                }
            }
        }
    }
}

async fn apply_from_ext(
    core: &Core,
    extension: &ExtensionId,
    protocol: &ProtocolId,
    kinds: &[EntityKind],
    stdin: &mut tokio::process::ChildStdin,
    from: FromExt,
    pending_calls: &mut std::collections::HashMap<u64, IncomingCall>,
) -> Result<(), String> {
    use tokio::sync::oneshot;
    let reply_id = match &from {
        FromExt::DescribeDevice { id, .. }
        | FromExt::DescribeEntity { id, .. }
        | FromExt::RemoveDevice { id, .. }
        | FromExt::RemoveEntity { id, .. }
        | FromExt::SetAvailability { id, .. }
        | FromExt::Load { id, .. }
        | FromExt::Store { id, .. } => Some(*id),
        _ => None,
    };
    match from {
        FromExt::DescribeDevice { device, .. } => {
            let (reply, rx) = oneshot::channel();
            core.apply_op(
                extension,
                protocol,
                kinds,
                Op::DescribeDevice(device, reply),
            );
            send_reply(stdin, reply_id, rx).await
        }
        FromExt::DescribeEntity { entity, .. } => {
            let (reply, rx) = oneshot::channel();
            core.apply_op(
                extension,
                protocol,
                kinds,
                Op::DescribeEntity(entity, reply),
            );
            send_reply(stdin, reply_id, rx).await
        }
        FromExt::RemoveDevice { unique_id, .. } => {
            let (reply, rx) = oneshot::channel();
            core.apply_op(
                extension,
                protocol,
                kinds,
                Op::RemoveDevice(unique_id, reply),
            );
            send_reply(stdin, reply_id, rx).await
        }
        FromExt::RemoveEntity { unique_id, .. } => {
            let (reply, rx) = oneshot::channel();
            core.apply_op(
                extension,
                protocol,
                kinds,
                Op::RemoveEntity(unique_id, reply),
            );
            send_reply(stdin, reply_id, rx).await
        }
        FromExt::SetAvailability {
            target,
            availability,
            ..
        } => {
            let (reply, rx) = oneshot::channel();
            core.apply_op(
                extension,
                protocol,
                kinds,
                Op::SetAvailability(target, availability, reply),
            );
            send_reply(stdin, reply_id, rx).await
        }
        FromExt::SetHealth { health } => {
            core.apply_op(extension, protocol, kinds, Op::SetHealth(health));
            Ok(())
        }
        FromExt::SetWaiting { waiting } => {
            core.apply_op(extension, protocol, kinds, Op::SetWaiting(waiting));
            Ok(())
        }
        FromExt::Load { key, .. } => {
            let (reply, rx) = oneshot::channel();
            core.apply_op(extension, protocol, kinds, Op::Load(key, reply));
            let result = rx.await.unwrap_or(Err(irori_protocol::Rejected(
                "the core is shutting down".into(),
            )));
            let (value, error) = match result {
                Ok(value) => (value, None),
                Err(rejected) => (None, Some(rejected.0)),
            };
            ExtProcess::send_on(
                stdin,
                &ToExt::Loaded {
                    id: reply_id.expect("load has id"),
                    value,
                    error,
                },
            )
            .await
        }
        FromExt::Store { key, value, .. } => {
            let (reply, rx) = oneshot::channel();
            core.apply_op(extension, protocol, kinds, Op::Store(key, value, reply));
            send_reply(stdin, reply_id, rx).await
        }
        FromExt::StateReport { report } => {
            core.apply_reports(extension, protocol, vec![report], 0);
            Ok(())
        }
        FromExt::ServiceResult { id, error } => {
            if let Some(incoming) = pending_calls.remove(&id) {
                incoming.reply(match error {
                    None => Ok(()),
                    Some(error) => Err(error.into()),
                });
            }
            Ok(())
        }
    }
}

async fn send_reply(
    stdin: &mut tokio::process::ChildStdin,
    id: Option<u64>,
    rx: tokio::sync::oneshot::Receiver<Result<(), irori_protocol::Rejected>>,
) -> Result<(), String> {
    let error = match rx.await {
        Ok(Ok(())) => None,
        Ok(Err(rejected)) => Some(rejected.0),
        Err(_) => Some("the core is shutting down".into()),
    };
    ExtProcess::send_on(
        stdin,
        &ToExt::Reply {
            id: id.expect("op has id"),
            error,
        },
    )
    .await
}
