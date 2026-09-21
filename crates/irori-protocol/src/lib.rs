//! The protocol SDK: what a built-in protocol implements, and the handle it uses to talk
//! to the core. See `docs/specs/protocols.md`.
//!
//! An protocol implements [`Protocol`]. The core starts it with its settings and an
//! [`ProtocolContext`], which offers exactly the operations of the contract: describe devices
//! and entities, report state and availability, set health, and handle service calls.
//!
//! The other end of the context lives in the core ([`host`]); protocols never see it.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use irori_types::{
    Availability, DeviceDescription, EntityDescription, ExtensionManifest, ServiceCall,
    StateReport, UniqueId, Waiting,
};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use tokio::sync::{Notify, mpsc, oneshot, watch};

pub use irori_types as types;

mod process;
pub use process::{ExtProcess, FromExt, ToExt, serve, spawn};

/// A built-in protocol.
///
/// ```ignore
/// impl Protocol for Demo {
///     type Config = Config;
///     const MANIFEST: &'static str = include_str!("../irori-extension.toml");
///
///     async fn run(config: Config, ctx: ProtocolContext) -> Result<(), ProtocolError> {
///         describe_devices(&ctx).await?;
///         // … handle `ctx.next_call()` until it returns `None`
///         Ok(())
///     }
/// }
/// ```
pub trait Protocol: Send + 'static {
    /// Its settings. The core deserializes them before starting it; the config schema shown to
    /// people is generated from this type.
    type Config: DeserializeOwned + JsonSchema + Send + 'static;

    /// The extension manifest, usually `include_str!("../irori-extension.toml")`.
    const MANIFEST: &'static str;

    /// The icon the manifest names (`icon = "icon.svg"`), usually `Some(include_str!("../icon.svg"))`.
    /// A built-in has no package directory to read it from at runtime, so it carries the file.
    const ICON: Option<&'static str> = None;

    /// Runs until told to stop ([`ProtocolContext::next_call`] returns `None`). Returning an
    /// error, returning without being told to stop, or panicking marks it failed, and the core
    /// starts it again after a delay.
    fn run(
        config: Self::Config,
        ctx: ProtocolContext,
    ) -> impl Future<Output = Result<(), ProtocolError>> + Send;
}

/// The most a stored value may take, as JSON (spec §5). Small on purpose: this is for pairing
/// keys and remembered switches, not history, which the recorder keeps.
pub const MAX_STORED_VALUE: usize = 64 * 1024;

/// Where each protocol's small private values live (spec §5). The core holds one and checks
/// the limits; the binary backs it with the database, and tests use [`MemoryStorage`].
pub trait Storage: Send + Sync + fmt::Debug {
    fn load(
        &self,
        extension: &irori_types::ExtensionId,
        key: &str,
    ) -> Result<Option<serde_json::Value>, String>;

    /// `None` forgets the key.
    fn store(
        &self,
        extension: &irori_types::ExtensionId,
        key: &str,
        value: Option<&serde_json::Value>,
    ) -> Result<(), String>;

    /// Forgets every stored value for this extension (uninstall).
    fn clear(&self, extension: &irori_types::ExtensionId) -> Result<(), String>;
}

/// Storage that lasts as long as the process. For tests, and for a core nobody gave a database.
#[derive(Debug, Default)]
pub struct MemoryStorage(Mutex<BTreeMap<(irori_types::ExtensionId, String), serde_json::Value>>);

impl Storage for MemoryStorage {
    fn load(
        &self,
        extension: &irori_types::ExtensionId,
        key: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(extension.clone(), key.to_owned()))
            .cloned())
    }

    fn store(
        &self,
        extension: &irori_types::ExtensionId,
        key: &str,
        value: Option<&serde_json::Value>,
    ) -> Result<(), String> {
        let mut all = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (extension.clone(), key.to_owned());
        match value {
            Some(value) => all.insert(key, value.clone()),
            None => all.remove(&key),
        };
        Ok(())
    }

    fn clear(&self, extension: &irori_types::ExtensionId) -> Result<(), String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(id, _), _| id != extension);
        Ok(())
    }
}

/// Settings for an protocol that has none. Accepts only an empty table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoSettings {}

/// Why an protocol stopped working. Shown to people, so say what went wrong in their terms.
pub struct ProtocolError(String);

impl ProtocolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

/// Any error converts, so `?` works in [`Protocol::run`].
impl<E: std::error::Error> From<E> for ProtocolError {
    fn from(error: E) -> Self {
        Self(error.to_string())
    }
}

/// The core refused an operation, e.g. an entity of a kind the manifest doesn't declare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected(pub String);

impl fmt::Display for Rejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Rejected {}

/// What the protocol says about itself (spec §6.5).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Running,
    /// Working, with a problem worth showing, e.g. "2 of 5 devices unreachable".
    Degraded(String),
}

/// Which entities a change of availability applies to (spec §6.4).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityTarget {
    /// Every entity of this device.
    Device(UniqueId),
    Entities(Vec<UniqueId>),
}

/// A failed service call, as the protocol reports it (spec §7.3).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServiceError {
    pub code: ServiceErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceErrorCode {
    /// The device can't be reached right now.
    Unavailable,
    /// The device or service refused or failed.
    Failed,
}

impl ServiceError {
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            code: ServiceErrorCode::Unavailable,
            message: message.into(),
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            code: ServiceErrorCode::Failed,
            message: message.into(),
        }
    }
}

/// A service call waiting for its result. Reply exactly once; dropping it without replying
/// reports a failure.
#[derive(Debug)]
pub struct IncomingCall {
    pub call: ServiceCall,
    reply: oneshot::Sender<Result<(), ServiceError>>,
}

impl IncomingCall {
    /// Replies once the device accepted the command. The new state comes as a state report,
    /// with `caused_by` set to `self.call.context.id`.
    pub fn reply(self, result: Result<(), ServiceError>) {
        // The core may have given up waiting (timeout); nothing to do then.
        let _ = self.reply.send(result);
    }
}

/// The protocol's handle to the core. Offers exactly the operations of the contract.
#[derive(Debug)]
pub struct ProtocolContext {
    ops: mpsc::Sender<host::Op>,
    reports: Arc<ReportQueue>,
    calls: mpsc::Receiver<IncomingCall>,
    stop: watch::Receiver<bool>,
}

const CORE_GONE: &str = "the core is shutting down";

impl ProtocolContext {
    async fn request(&self, op: impl FnOnce(host::Reply) -> host::Op) -> Result<(), Rejected> {
        let (reply, result) = oneshot::channel();
        self.ops
            .send(op(reply))
            .await
            .map_err(|_| Rejected(CORE_GONE.into()))?;
        result.await.map_err(|_| Rejected(CORE_GONE.into()))?
    }

    /// Adds a device, or updates the one with the same `unique_id`.
    pub async fn describe_device(&self, device: DeviceDescription) -> Result<(), Rejected> {
        self.request(|reply| host::Op::DescribeDevice(device, reply))
            .await
    }

    /// Adds an entity, or updates the one with the same `unique_id`. Describe its device first.
    pub async fn describe_entity(&self, entity: EntityDescription) -> Result<(), Rejected> {
        self.request(|reply| host::Op::DescribeEntity(entity, reply))
            .await
    }

    /// Removes a device and its entities.
    pub async fn remove_device(&self, unique_id: UniqueId) -> Result<(), Rejected> {
        self.request(|reply| host::Op::RemoveDevice(unique_id, reply))
            .await
    }

    pub async fn remove_entity(&self, unique_id: UniqueId) -> Result<(), Rejected> {
        self.request(|reply| host::Op::RemoveEntity(unique_id, reply))
            .await
    }

    pub async fn set_availability(
        &self,
        target: AvailabilityTarget,
        availability: Availability,
    ) -> Result<(), Rejected> {
        self.request(|reply| host::Op::SetAvailability(target, availability, reply))
            .await
    }

    pub async fn set_health(&self, health: Health) {
        // If the core is gone there's nobody to tell.
        let _ = self.ops.send(host::Op::SetHealth(health)).await;
    }

    /// Reads a value it stored earlier, or `None` if there isn't one (spec §5). Kept across
    /// restarts of the protocol and of Irori.
    pub async fn load(&self, key: &str) -> Result<Option<serde_json::Value>, Rejected> {
        let (reply, answer) = oneshot::channel();
        self.ops
            .send(host::Op::Load(key.to_owned(), reply))
            .await
            .map_err(|_| Rejected(CORE_GONE.into()))?;
        answer.await.map_err(|_| Rejected(CORE_GONE.into()))?
    }

    /// Keeps a small value under `key`, private to this protocol: at most
    /// [`MAX_STORED_VALUE`] bytes as JSON, under a key of 1–128 characters.
    pub async fn store(&self, key: &str, value: serde_json::Value) -> Result<(), Rejected> {
        self.request(|reply| host::Op::Store(key.to_owned(), Some(value), reply))
            .await
    }

    /// Forgets what was stored under `key`. Forgetting something that isn't there is fine.
    pub async fn forget(&self, key: &str) -> Result<(), Rejected> {
        self.request(|reply| host::Op::Store(key.to_owned(), None, reply))
            .await
    }

    /// Says what it has found but can't use until a person does something (spec §6.6): the
    /// whole list, replacing the last one. Send an empty list when nothing is waiting.
    ///
    /// Like health, send it when it changes rather than on every turn of a loop.
    pub async fn set_waiting(&self, waiting: Vec<Waiting>) {
        let _ = self.ops.send(host::Op::SetWaiting(waiting)).await;
    }

    /// Reports a new value for one of its entities. Never waits: if the core is behind, an
    /// older report for the same entity that it hasn't read yet is replaced by this one. At most
    /// [`MAX_PENDING_ENTITIES`] entities' reports wait at once; reports for further entities are
    /// dropped until the core catches up. The core logs dropped and rejected reports.
    pub fn report_state(&self, report: StateReport) {
        self.reports.push(report);
    }

    /// The next service call, or `None` once the protocol should stop. After `None`, finish
    /// up and return from `run` within 5 seconds.
    pub async fn next_call(&mut self) -> Option<IncomingCall> {
        if *self.stop.borrow() {
            return None;
        }
        tokio::select! {
            biased;
            _ = self.stop.wait_for(|stop| *stop) => None,
            call = self.calls.recv() => call,
        }
    }

    /// Resolves once the protocol should stop. For protocols that don't take calls in the
    /// same loop.
    pub async fn stopped(&mut self) {
        let _ = self.stop.wait_for(|stop| *stop).await;
    }
}

/// How many entities can have a state report waiting for the core at once. Bounds the core's
/// memory even if an protocol reports for ever-new entities faster than the core keeps up.
pub const MAX_PENDING_ENTITIES: usize = 4096;

/// Pending state reports, one per entity: a newer report replaces an unread older one.
#[derive(Debug, Default)]
struct ReportQueue {
    pending: Mutex<BTreeMap<UniqueId, StateReport>>,
    dropped: AtomicU64,
    ready: Notify,
}

impl ReportQueue {
    fn push(&self, report: StateReport) {
        {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            if pending.len() >= MAX_PENDING_ENTITIES && !pending.contains_key(&report.unique_id) {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            } else {
                pending.insert(report.unique_id.clone(), report);
            }
        }
        // Wake the core for drops too, so it logs them even if nothing else is waiting.
        self.ready.notify_one();
    }

    fn is_empty(&self) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }

    fn take(&self) -> Vec<StateReport> {
        std::mem::take(&mut *self.pending.lock().unwrap_or_else(PoisonError::into_inner))
            .into_values()
            .collect()
    }
}

/// A built-in protocol, ready for the core to start.
pub struct Builtin {
    pub manifest: ExtensionManifest,
    /// JSON Schema for its settings, generated from its config type.
    pub config_schema: serde_json::Value,
    /// Its icon, an SVG document, when the manifest names one.
    pub icon: Option<&'static str>,
    start: StartFn,
}

type RunFuture = Pin<Box<dyn Future<Output = Result<(), ProtocolError>> + Send>>;
type StartFn =
    Box<dyn Fn(serde_json::Value, ProtocolContext) -> Result<RunFuture, String> + Send + Sync>;

impl fmt::Debug for Builtin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Builtin")
            .field("id", &self.manifest.extension.id)
            .finish_non_exhaustive()
    }
}

impl Builtin {
    /// Checks `config` against the protocol's config type, then returns the future that runs
    /// it. The error says what's wrong with the settings.
    pub fn start(
        &self,
        config: serde_json::Value,
        ctx: ProtocolContext,
    ) -> Result<RunFuture, String> {
        (self.start)(config, ctx)
    }
}

/// Prepares a built-in protocol: parses and checks its manifest, and generates its config
/// schema. Fails if the manifest is invalid or doesn't describe a built-in protocol.
pub fn builtin<I: Protocol>() -> Result<Builtin, String> {
    let manifest = parse_manifest(I::MANIFEST)?;
    let id = &manifest.extension.id;
    match manifest.contributes.protocol.as_slice() {
        [_] => {}
        _ => {
            return Err(format!(
                "extension `{id}`: a built-in protocol needs a `[[contributes.protocol]]` entry"
            ));
        }
    }
    let config_schema = serde_json::to_value(schemars::schema_for!(I::Config))
        .map_err(|e| format!("extension `{id}`: config schema: {e}"))?;
    // The manifest is what says there's an icon; the constant is only where a built-in keeps it.
    // Disagreeing is a mistake worth failing loudly over, not a missing picture.
    match (&manifest.extension.icon, I::ICON) {
        (Some(_), Some(svg)) if svg.trim_start().starts_with("<svg") => {}
        (Some(_), Some(_)) => {
            return Err(format!("extension `{id}`: its icon isn't an SVG document"));
        }
        (Some(path), None) => {
            return Err(format!(
                "extension `{id}`: the manifest names the icon `{path}`, but the protocol \
                 doesn't embed it (`const ICON`)"
            ));
        }
        (None, Some(_)) => {
            return Err(format!(
                "extension `{id}`: the protocol embeds an icon the manifest doesn't name \
                 (`icon = \"icon.svg\"`)"
            ));
        }
        (None, None) => {}
    }
    Ok(Builtin {
        manifest,
        config_schema,
        icon: I::ICON,
        start: Box::new(|config, ctx| {
            let config: I::Config = serde_json::from_value(config).map_err(invalid_settings)?;
            Ok(Box::pin(I::run(config, ctx)))
        }),
    })
}

/// Why settings couldn't be turned into the protocol's config type. Names the shape, never a
/// value: settings hold secrets, and the reason is logged and shown (`docs/specs/protocols.md`
/// §3).
pub(crate) fn invalid_settings(err: serde_json::Error) -> String {
    format!("invalid settings: {}", redact_serde_value(&err.to_string()))
}

/// Drops quoted strings and numeric/boolean literals from a serde error. Field names in
/// backticks stay: they are keys, not values.
fn redact_serde_value(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut chars = message.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            out.push_str("\"…\"");
            let mut escaped = false;
            for next in chars.by_ref() {
                if escaped {
                    escaped = false;
                    continue;
                }
                match next {
                    '\\' => escaped = true,
                    '"' => break,
                    _ => {}
                }
            }
            continue;
        }
        out.push(c);
    }
    redact_serde_literals(&out)
}

fn redact_serde_literals(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    while let Some((at, kind)) = ["integer ", "float ", "boolean "]
        .into_iter()
        .filter_map(|kind| rest.find(kind).map(|at| (at, kind)))
        .min_by_key(|(at, _)| *at)
    {
        out.push_str(&rest[..at + kind.len()]);
        rest = &rest[at + kind.len()..];
        rest = match kind {
            "boolean " => skip_boolean(rest),
            _ => skip_number(rest),
        };
    }
    out.push_str(rest);
    out
}

fn skip_boolean(s: &str) -> &str {
    let (s, tick) = match s.strip_prefix('`') {
        Some(inner) => (inner, true),
        None => (s, false),
    };
    let s = s
        .strip_prefix("true")
        .or_else(|| s.strip_prefix("false"))
        .unwrap_or(s);
    if tick {
        s.strip_prefix('`').unwrap_or(s)
    } else {
        s
    }
}

fn skip_number(s: &str) -> &str {
    let (s, tick) = match s.strip_prefix('`') {
        Some(inner) => (inner, true),
        None => (s, false),
    };
    let s = s.trim_start_matches(['+', '-']);
    let s = s.trim_start_matches(|c: char| c.is_ascii_digit());
    let s = s
        .strip_prefix('.')
        .map(|frac| frac.trim_start_matches(|c: char| c.is_ascii_digit()))
        .unwrap_or(s);
    let s = match s.strip_prefix(['e', 'E']) {
        Some(exp) => exp
            .trim_start_matches(['+', '-'])
            .trim_start_matches(|c: char| c.is_ascii_digit()),
        None => s,
    };
    if tick {
        s.strip_prefix('`').unwrap_or(s)
    } else {
        s
    }
}

/// Reads `irori-extension.toml` text the way Irori reads every manifest: TOML into the JSON data
/// model, then the manifest types.
pub fn parse_manifest(toml_text: &str) -> Result<ExtensionManifest, String> {
    let json: serde_json::Value =
        toml::from_str(toml_text).map_err(|e| format!("manifest is not valid TOML: {e}"))?;
    serde_json::from_value(json).map_err(|e| format!("invalid manifest: {e}"))
}

/// The core's end of an [`ProtocolContext`]. Used by the core's extension host only.
pub mod host {
    use super::*;

    pub type Reply = oneshot::Sender<Result<(), Rejected>>;

    /// An operation from the protocol that needs the core's answer (spec §5).
    #[derive(Debug)]
    pub enum Op {
        DescribeDevice(DeviceDescription, Reply),
        DescribeEntity(EntityDescription, Reply),
        RemoveDevice(UniqueId, Reply),
        RemoveEntity(UniqueId, Reply),
        SetAvailability(AvailabilityTarget, Availability, Reply),
        SetHealth(Health),
        SetWaiting(Vec<Waiting>),
        Load(
            String,
            oneshot::Sender<Result<Option<serde_json::Value>, Rejected>>,
        ),
        /// `None` forgets the key.
        Store(String, Option<serde_json::Value>, Reply),
    }

    /// Bounded, so a runaway protocol waits instead of growing the core's memory.
    const OPS_CAPACITY: usize = 256;
    const CALLS_CAPACITY: usize = 64;

    #[derive(Debug)]
    pub struct HostEnd {
        pub ops: mpsc::Receiver<Op>,
        pub reports: Reports,
        pub calls: mpsc::Sender<IncomingCall>,
        pub stop: watch::Sender<bool>,
    }

    /// State reports waiting for the core.
    #[derive(Debug)]
    pub struct Reports(Arc<ReportQueue>);

    impl Reports {
        /// Waits until there's something to do: reports pending, or reports dropped. Takes
        /// nothing, so dropping this future (as a losing `select!` branch, say) loses nothing;
        /// call [`Reports::drain`] once the caller commits to handling them.
        pub async fn ready(&self) {
            loop {
                if !self.0.is_empty() || self.0.dropped.load(Ordering::Relaxed) > 0 {
                    return;
                }
                self.0.ready.notified().await;
            }
        }

        /// Takes whatever is pending, without waiting.
        pub fn drain(&self) -> Vec<StateReport> {
            self.0.take()
        }

        /// How many reports were dropped because too many entities were waiting, since the last
        /// call.
        pub fn take_dropped(&self) -> u64 {
            self.0.dropped.swap(0, Ordering::Relaxed)
        }
    }

    /// A connected pair: the context goes to the protocol, the other end stays in the core.
    pub fn connect() -> (ProtocolContext, HostEnd) {
        let (ops_tx, ops_rx) = mpsc::channel(OPS_CAPACITY);
        let (calls_tx, calls_rx) = mpsc::channel(CALLS_CAPACITY);
        let (stop_tx, stop_rx) = watch::channel(false);
        let reports = Arc::new(ReportQueue::default());
        let ctx = ProtocolContext {
            ops: ops_tx,
            reports: Arc::clone(&reports),
            calls: calls_rx,
            stop: stop_rx,
        };
        let host = HostEnd {
            ops: ops_rx,
            reports: Reports(reports),
            calls: calls_tx,
            stop: stop_tx,
        };
        (ctx, host)
    }

    /// Builds a call for the protocol, with the channel its reply comes back on.
    pub fn incoming_call(
        call: ServiceCall,
    ) -> (IncomingCall, oneshot::Receiver<Result<(), ServiceError>>) {
        let (reply, result) = oneshot::channel();
        (IncomingCall { call, reply }, result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::{State, SwitchState};

    fn report(unique_id: &str, on: bool) -> StateReport {
        StateReport {
            unique_id: UniqueId::try_from(unique_id).expect("valid"),
            state: Some(State::Switch(SwitchState { on })),
            attributes: BTreeMap::new(),
            caused_by: None,
        }
    }

    #[tokio::test]
    async fn newer_reports_replace_unread_ones_for_the_same_entity() {
        let (ctx, host) = host::connect();
        ctx.report_state(report("a", true));
        ctx.report_state(report("b", true));
        ctx.report_state(report("a", false));
        host.reports.ready().await;
        assert_eq!(
            host.reports.drain(),
            vec![report("a", false), report("b", true)]
        );
        assert!(host.reports.drain().is_empty());
    }

    #[tokio::test]
    async fn waiting_reports_are_bounded() {
        let (ctx, host) = host::connect();
        for i in 0..MAX_PENDING_ENTITIES + 10 {
            ctx.report_state(report(&format!("e{i}"), true));
        }
        // An entity that's already waiting can still be updated.
        ctx.report_state(report("e0", false));
        let batch = host.reports.drain();
        assert_eq!(batch.len(), MAX_PENDING_ENTITIES);
        assert!(batch.contains(&report("e0", false)));
        assert_eq!(host.reports.take_dropped(), 10);
        assert_eq!(host.reports.take_dropped(), 0);

        // Drops alone still wake the core, so it can log them.
        for i in 0..MAX_PENDING_ENTITIES + 1 {
            ctx.report_state(report(&format!("f{i}"), true));
        }
        host.reports.ready().await;
        assert_eq!(host.reports.drain().len(), MAX_PENDING_ENTITIES);
        tokio::time::timeout(std::time::Duration::from_secs(1), host.reports.ready())
            .await
            .expect("woken by the drop alone");
        assert!(host.reports.drain().is_empty());
        assert_eq!(host.reports.take_dropped(), 1);
    }

    /// Readiness must take nothing: a `select!` that drops this branch loses no reports.
    #[tokio::test]
    async fn readiness_takes_nothing() {
        let (ctx, host) = host::connect();
        ctx.report_state(report("a", true));
        tokio::select! {
            () = host.reports.ready() => {}
            () = std::future::ready(()) => {}
        }
        assert_eq!(host.reports.drain(), vec![report("a", true)]);
    }

    #[tokio::test]
    async fn next_call_ends_when_told_to_stop() {
        let (mut ctx, host) = host::connect();
        host.stop.send(true).expect("context alive");
        assert!(ctx.next_call().await.is_none());
    }

    #[tokio::test]
    async fn operations_fail_cleanly_once_the_core_is_gone() {
        let (ctx, host) = host::connect();
        drop(host);
        let err = ctx
            .remove_entity(UniqueId::try_from("a").expect("valid"))
            .await
            .expect_err("no core");
        assert_eq!(err, Rejected(CORE_GONE.into()));
    }

    struct NoRun;
    impl Protocol for NoRun {
        type Config = NoSettings;
        const MANIFEST: &'static str = r#"
            [extension]
            id = "external_thing"
            name = "External thing"
            version = "0.1.0"
            irori = ">=0.1.0"

            [[contributes.protocol]]
            iot_class = "local_push"
            entity_kinds = ["switch"]
            run = { command = "bin/thing" }
        "#;
        async fn run(_: NoSettings, _: ProtocolContext) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    #[test]
    fn builtins_may_have_a_run_command() {
        // Official packages declare `run` so the host can start them as a process. The same
        // crate can still be started in-process in tests; `run` is ignored then.
        assert!(builtin::<NoRun>().is_ok());
    }

    macro_rules! with_icon {
        ($name:ident, $manifest_icon:expr, $icon:expr) => {
            struct $name;
            impl Protocol for $name {
                type Config = NoSettings;
                const MANIFEST: &'static str = concat!(
                    "[extension]\nid = \"lamp\"\nname = \"Lamp\"\nversion = \"0.1.0\"\n",
                    "irori = \">=0.0.0\"\n",
                    $manifest_icon,
                    "\n[[contributes.protocol]]\niot_class = \"local_push\"\n",
                    "entity_kinds = [\"light\"]\n"
                );
                const ICON: Option<&'static str> = $icon;
                async fn run(_: NoSettings, _: ProtocolContext) -> Result<(), ProtocolError> {
                    Ok(())
                }
            }
        };
    }
    with_icon!(
        Agreed,
        "icon = \"icon.svg\"",
        Some("<svg xmlns=\"http://www.w3.org/2000/svg\"/>")
    );
    with_icon!(NamedNotEmbedded, "icon = \"icon.svg\"", None);
    with_icon!(EmbeddedNotNamed, "", Some("<svg/>"));
    with_icon!(
        NotAnSvg,
        "icon = \"icon.svg\"",
        Some("<script>alert(1)</script>")
    );

    /// The manifest says whether there's an icon; a built-in carries the file. They have to
    /// agree, and the file has to be an SVG.
    #[test]
    fn a_builtins_icon_is_the_one_its_manifest_names() {
        assert!(builtin::<Agreed>().expect("valid").icon.is_some());
        for (err, says) in [
            (
                builtin::<NamedNotEmbedded>().expect_err("missing"),
                "doesn't embed",
            ),
            (
                builtin::<EmbeddedNotNamed>().expect_err("unnamed"),
                "doesn't name",
            ),
            (builtin::<NotAnSvg>().expect_err("not svg"), "isn't an SVG"),
        ] {
            assert!(err.contains(says), "{err}");
        }
    }

    /// Settings errors are shown. They must name the field and the type, never the value that
    /// was there — that's where secrets live.
    #[test]
    fn a_settings_error_names_the_shape_not_the_value() {
        #[derive(Debug, serde::Deserialize)]
        struct NeedsString {
            #[allow(dead_code)]
            key: String,
        }
        #[derive(Debug, serde::Deserialize)]
        struct NeedsNumber {
            #[allow(dead_code)]
            key: i64,
        }

        let number = invalid_settings(
            serde_json::from_value::<NeedsString>(serde_json::json!({"key": 1})).expect_err("type"),
        );
        assert!(number.starts_with("invalid settings:"), "{number}");
        assert!(!number.contains('1'), "{number}");
        assert!(number.contains("integer"), "{number}");

        let secret = invalid_settings(
            serde_json::from_value::<NeedsNumber>(serde_json::json!({"key": "s3cret"}))
                .expect_err("type"),
        );
        assert!(!secret.contains("s3cret"), "{secret}");
        assert!(secret.contains("\"…\""), "{secret}");
    }
}
