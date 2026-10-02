//! JSON-lines protocol between the core and an external extension process.
//!
//! The same `Protocol` trait runs in-process (tests, and the SDK) or as a child whose
//! stdin/stdout speak these messages (`docs/specs/extensions.md`).

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use irori_types::{
    Availability, ContextId, DeviceDescription, EntityDescription, EntityId, EntityState,
    RunCommand, ServiceCall, StateReport, Timestamp, UniqueId, Unmodeled, Waiting,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot};

use crate::host::{Op, Reply, Reports};
use crate::{
    AvailabilityTarget, Health, IncomingAction, IncomingCall, Protocol, ProtocolError, Rejected,
    ServiceError, ServiceErrorCode, host,
};

/// A message from the extension process to the host.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromExt {
    DescribeDevice {
        id: u64,
        device: DeviceDescription,
    },
    DescribeEntity {
        id: u64,
        entity: EntityDescription,
    },
    RemoveDevice {
        id: u64,
        unique_id: UniqueId,
    },
    RemoveEntity {
        id: u64,
        unique_id: UniqueId,
    },
    SetAvailability {
        id: u64,
        target: AvailabilityTarget,
        availability: Availability,
    },
    SetHealth {
        health: Health,
    },
    SetWaiting {
        waiting: Vec<Waiting>,
    },
    SetUnmodeled {
        unmodeled: Vec<Unmodeled>,
    },
    SetAvailableActions {
        actions: Vec<String>,
    },
    Load {
        id: u64,
        key: String,
    },
    Store {
        id: u64,
        key: String,
        #[serde(default)]
        value: Option<serde_json::Value>,
    },
    StateReport {
        report: StateReport,
    },
    ServiceResult {
        id: u64,
        #[serde(default)]
        error: Option<WireServiceError>,
    },
    ActionResult {
        id: u64,
        #[serde(default)]
        error: Option<String>,
    },
    // --- Engine operations (`docs/specs/automations.md` §B2). Each needs an API scope and is
    // answered with `ToExt::Answer`.
    /// Every entity, and whether time and sun triggers can be armed. `registry:read`.
    GetRegistry {
        id: u64,
    },
    /// Every entity's current state. `states:read`.
    GetStates {
        id: u64,
    },
    /// The recent changes of these entities since `since`, oldest first. `history:read`.
    GetHistory {
        id: u64,
        entities: Vec<EntityId>,
        since: Timestamp,
    },
    /// Start receiving `state_changed` and/or `registry_changed`. `events:read`.
    Subscribe {
        id: u64,
        #[serde(default)]
        states: bool,
        #[serde(default)]
        registry: bool,
    },
    /// Ask an entity to do something, as a run of this engine. `services:call`.
    CallService {
        id: u64,
        entity_id: EntityId,
        /// The action: `turn_on`, `set_value`, … or `toggle`, which the core resolves.
        command: WireCommand,
        /// The action's data, as its service takes it (`docs/specs/protocols.md` §7.1).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<serde_json::Map<String, serde_json::Value>>,
        run_id: ContextId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<ContextId>,
    },
    /// The answer to an `app_request` from the extension's page.
    AppAnswer {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// What a `call_service` asks for: an action of the entity's kind, the part of a service name
/// after the dot (`turn_on`), or `toggle`. The core checks it against the entity, so the wire
/// doesn't need to know every kind's actions, and an engine built before a kind existed still
/// talks to a core that has it.
pub type WireCommand = String;

/// A message from the host to the extension process.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToExt {
    Hello {
        settings: serde_json::Value,
    },
    Reply {
        id: u64,
        #[serde(default)]
        error: Option<String>,
    },
    Loaded {
        id: u64,
        #[serde(default)]
        value: Option<serde_json::Value>,
        #[serde(default)]
        error: Option<String>,
    },
    ServiceCall {
        id: u64,
        call: ServiceCall,
    },
    ActionCall {
        id: u64,
        action_id: String,
    },
    Stop,
    /// The answer to an engine operation. `error` starts with a code for `call_service`
    /// (`unavailable: …`).
    Answer {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A state changed somewhere in the home (after `subscribe { states: true }`).
    StateChanged {
        entity_id: EntityId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        old_state: Option<Box<EntityState>>,
        new_state: Box<EntityState>,
    },
    /// An entity or device was added, changed or removed (after `subscribe { registry: true }`).
    /// Ask again with `get_registry`.
    RegistryChanged {},
    /// The extension's page asked it something.
    AppRequest {
        id: u64,
        method: String,
        #[serde(default)]
        params: serde_json::Value,
    },
}

/// A failed service call on the wire (`docs/specs/protocols.md` §7.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireServiceError {
    pub code: String,
    pub message: String,
}

impl From<ServiceError> for WireServiceError {
    fn from(error: ServiceError) -> Self {
        Self {
            code: match error.code {
                ServiceErrorCode::Unavailable => "unavailable".into(),
                ServiceErrorCode::Failed => "failed".into(),
            },
            message: error.message,
        }
    }
}

impl From<WireServiceError> for ServiceError {
    fn from(error: WireServiceError) -> Self {
        match error.code.as_str() {
            "unavailable" => ServiceError::unavailable(error.message),
            _ => ServiceError::failed(error.message),
        }
    }
}

/// Runs this protocol as an external process: read `ToExt` from stdin, write `FromExt` to
/// stdout. The first message must be `hello` with its settings.
pub async fn serve<I: Protocol>() -> Result<(), ProtocolError> {
    // Stdin and stdout each get their own thread. `tokio::io::stdin` reads by parking a
    // blocking-pool task, and that read was simply never issued while a reply sat in the pipe:
    // discovery's `describe_*` waits on that reply, so the run loop never reached permit-join
    // and the host's 10 second call timed out. A thread blocked in `read` does not depend on
    // the runtime noticing the pipe. The writer is a thread for the same reason — a full stdout
    // pipe must not park the runtime that has to keep handling the lines this thread reads.
    // The queue between them is bounded. State reports that do not fit stay in `ReportQueue`,
    // where a newer value for the same entity still replaces the one waiting.
    let (out_tx, out_rx) = mpsc::channel(STDOUT_QUEUE);
    std::thread::Builder::new()
        .name("protocol-stdout".into())
        .spawn(move || write_stdout(out_rx))
        .map_err(|e| {
            ProtocolError::new(format!("couldn't start the protocol stdout thread: {e}"))
        })?;

    let (ctx, host_end) = host::connect();
    let pending = Arc::new(Pending::default());
    let (hello_tx, hello_rx) = std::sync::mpsc::sync_channel(1);
    let pending_in = Arc::clone(&pending);
    let calls = host_end.calls;
    let actions = host_end.actions;
    let stop = host_end.stop;
    let out_in = out_tx.clone();
    let runtime = tokio::runtime::Handle::current();
    std::thread::Builder::new()
        .name("protocol-stdin".into())
        .spawn(move || read_stdin(hello_tx, pending_in, calls, actions, stop, out_in, runtime))
        .map_err(|e| {
            ProtocolError::new(format!("couldn't start the protocol stdin thread: {e}"))
        })?;

    let hello = tokio::task::spawn_blocking(move || hello_rx.recv())
        .await
        .map_err(|e| ProtocolError::new(format!("reading hello failed: {e}")))?
        .map_err(|_| ProtocolError::new("the host closed the connection before hello"))?;
    let ToExt::Hello { settings } = hello else {
        return Err(ProtocolError::new(
            "first message from the host must be hello",
        ));
    };
    let config: I::Config = serde_json::from_value(settings)
        .map_err(|e| ProtocolError::new(crate::invalid_settings(e)))?;

    let pending_out = Arc::clone(&pending);
    let outgoing = tokio::spawn(async move {
        pump_outgoing(host_end.ops, host_end.reports, out_tx, pending_out).await;
    });

    let result = I::run(config, ctx).await;
    outgoing.abort();
    result
}

/// How many encoded lines may sit ahead of the stdout thread. Past this, state reports stay
/// in `ReportQueue` (one per entity) instead of becoming another retained `String`.
const STDOUT_QUEUE: usize = 64;

async fn pump_outgoing(
    mut ops: tokio::sync::mpsc::Receiver<Op>,
    reports: Reports,
    out: mpsc::Sender<String>,
    pending: Arc<Pending>,
) {
    loop {
        tokio::select! {
            biased;
            Some(op) = ops.recv() => {
                if let Some(msg) = pending.encode_op(op)
                    && send_line(&out, &msg).await.is_err()
                {
                    return;
                }
            }
            () = reports.ready() => {
                // A drop with an empty queue also wakes `ready`. Take the count so this
                // branch does not spin; the external host never sees that counter.
                let _ = reports.take_dropped();
                while let Some(report) = reports.pop() {
                    match offer_report(&out, &reports, report) {
                        Offer::Queued => {}
                        Offer::Full => {
                            // Room comes back when the stdout thread writes a line. An op
                            // that arrives meanwhile goes out first.
                            tokio::select! {
                                biased;
                                Some(op) = ops.recv() => {
                                    if let Some(msg) = pending.encode_op(op)
                                        && send_line(&out, &msg).await.is_err()
                                    {
                                        return;
                                    }
                                }
                                permit = out.reserve() => {
                                    // The slot is for whatever is pending now, which may be a
                                    // newer report than the one that did not fit.
                                    drop(permit);
                                }
                            }
                            break;
                        }
                        Offer::Closed => return,
                    }
                }
            }
        }
    }
}

enum Offer {
    Queued,
    Full,
    Closed,
}

/// Queues one report. On a full queue the report goes back unless a newer one arrived.
fn offer_report(out: &mpsc::Sender<String>, reports: &Reports, report: StateReport) -> Offer {
    let Some(line) = encode_line(&FromExt::StateReport {
        report: report.clone(),
    }) else {
        return Offer::Queued;
    };
    match out.try_send(line) {
        Ok(()) => Offer::Queued,
        Err(TrySendError::Full(_)) => {
            reports.restore_if_absent(report);
            Offer::Full
        }
        Err(TrySendError::Closed(_)) => Offer::Closed,
    }
}

async fn send_line(out: &mpsc::Sender<String>, value: &impl Serialize) -> Result<(), ()> {
    let Some(line) = encode_line(value) else {
        return Ok(());
    };
    out.send(line).await.map_err(|_| ())
}

fn write_stdout(mut rx: mpsc::Receiver<String>) {
    let mut out = std::io::stdout().lock();
    while let Some(line) = rx.blocking_recv() {
        if out.write_all(line.as_bytes()).is_err() || out.flush().is_err() {
            return;
        }
    }
}

fn read_stdin(
    hello: std::sync::mpsc::SyncSender<ToExt>,
    pending: Arc<Pending>,
    calls: tokio::sync::mpsc::Sender<IncomingCall>,
    actions: tokio::sync::mpsc::Sender<IncomingAction>,
    stop: tokio::sync::watch::Sender<bool>,
    out: mpsc::Sender<String>,
    runtime: tokio::runtime::Handle,
) {
    // This pipe arrived nonblocking. `read` then returns `WouldBlock` instead of waiting,
    // which this loop would treat as "the host hung up" and stop reading while a `describe_*`
    // reply is still on its way.
    #[cfg(unix)]
    make_blocking(std::io::stdin());
    let mut reader = std::io::BufReader::new(std::io::stdin());
    let mut line = String::new();
    if read_one(&mut reader, &mut line).is_err() {
        return;
    }
    match serde_json::from_str::<ToExt>(line.trim()) {
        Ok(msg) => {
            if hello.send(msg).is_err() {
                return;
            }
        }
        Err(_) => return,
    }
    loop {
        if read_one(&mut reader, &mut line).is_err()
            || handle_host_line(line.trim(), &pending, &calls, &actions, &out, &runtime)
                == ReadControl::Stop
        {
            let _ = stop.send(true);
            return;
        }
    }
}

#[cfg(unix)]
fn make_blocking(fd: impl rustix::fd::AsFd) {
    let Ok(flags) = rustix::fs::fcntl_getfl(&fd) else {
        return;
    };
    let _ = rustix::fs::fcntl_setfl(&fd, flags.difference(rustix::fs::OFlags::NONBLOCK));
}

fn read_one(reader: &mut impl BufRead, line: &mut String) -> Result<(), ()> {
    loop {
        line.clear();
        match reader.read_line(line) {
            Ok(0) => return Err(()),
            Ok(_) => return Ok(()),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::Interrupted =>
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => return Err(()),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ReadControl {
    Continue,
    Stop,
}

/// Applies one host line. Replies are completed here, on the stdin thread, so a `describe_*`
/// waiting on that reply does not depend on the runtime polling stdin.
fn handle_host_line(
    line: &str,
    pending: &Pending,
    calls: &tokio::sync::mpsc::Sender<IncomingCall>,
    actions: &tokio::sync::mpsc::Sender<IncomingAction>,
    out: &mpsc::Sender<String>,
    runtime: &tokio::runtime::Handle,
) -> ReadControl {
    let msg: ToExt = match serde_json::from_str(line) {
        Ok(msg) => msg,
        Err(_) => return ReadControl::Stop,
    };
    match msg {
        ToExt::Reply { id, error } => pending.complete_reply(id, error),
        ToExt::Loaded { id, value, error } => pending.complete_load(id, value, error),
        ToExt::ServiceCall { id, call } => {
            // `send().await` would wait while the run loop is busy inside `describe_*`. The
            // reply that unblocks that wait is a later line on this same stdin, so the reader
            // has to keep going. A full queue gets an immediate error instead of a 10 second
            // silence.
            let (incoming, result) = host::incoming_call(call);
            match calls.try_send(incoming) {
                Ok(()) => {
                    let out = out.clone();
                    runtime.spawn(async move {
                        let error = match result.await {
                            Ok(Ok(())) => None,
                            Ok(Err(error)) => Some(WireServiceError::from(error)),
                            Err(_) => Some(WireServiceError {
                                code: "failed".into(),
                                message: "the protocol dropped the call".into(),
                            }),
                        };
                        enqueue_json(&out, &FromExt::ServiceResult { id, error });
                    });
                }
                Err(TrySendError::Closed(_)) => return ReadControl::Stop,
                Err(TrySendError::Full(_)) => {
                    enqueue_json(
                        out,
                        &FromExt::ServiceResult {
                            id,
                            error: Some(WireServiceError {
                                code: "unavailable".into(),
                                message: "the extension is busy and can't take this call yet"
                                    .into(),
                            }),
                        },
                    );
                }
            }
        }
        ToExt::ActionCall { id, action_id } => {
            let (incoming, result) = host::incoming_action(action_id);
            match actions.try_send(incoming) {
                Ok(()) => {
                    let out = out.clone();
                    runtime.spawn(async move {
                        let error = match result.await {
                            Ok(Ok(())) => None,
                            Ok(Err(error)) => Some(error),
                            Err(_) => Some("the protocol dropped the action call".to_owned()),
                        };
                        enqueue_json(&out, &FromExt::ActionResult { id, error });
                    });
                }
                Err(TrySendError::Closed(_)) => return ReadControl::Stop,
                Err(TrySendError::Full(_)) => {
                    enqueue_json(
                        out,
                        &FromExt::ActionResult {
                            id,
                            error: Some(
                                "the extension is busy and can't take this action yet".into(),
                            ),
                        },
                    );
                }
            }
        }
        ToExt::Stop => return ReadControl::Stop,
        // Engine traffic is read by `engine::run`, not by a protocol extension's `serve`.
        // A line that arrives here is consumed so the reader stays up; the same messages
        // were ignored by the reader this replaced.
        ToExt::Hello { .. }
        | ToExt::Answer { .. }
        | ToExt::StateChanged { .. }
        | ToExt::RegistryChanged {}
        | ToExt::AppRequest { .. } => {}
    }
    ReadControl::Continue
}

fn encode_line(value: &impl Serialize) -> Option<String> {
    let mut line = serde_json::to_string(value).ok()?;
    line.push('\n');
    Some(line)
}

fn enqueue_json(tx: &mpsc::Sender<String>, value: &impl Serialize) {
    let Some(line) = encode_line(value) else {
        return;
    };
    // The stdin thread is not on the runtime, so a full queue waits here instead of dropping
    // the action result the host is reading for. Reports use `try_send` and never take the
    // last slot this way from the run loop.
    match tx.try_send(line) {
        Ok(()) => {}
        Err(TrySendError::Full(line)) => {
            let _ = tx.blocking_send(line);
        }
        Err(TrySendError::Closed(_)) => {}
    }
}

type LoadReply = oneshot::Sender<Result<Option<serde_json::Value>, Rejected>>;

#[derive(Default)]
struct Pending {
    next_id: AtomicU64,
    replies: Mutex<HashMap<u64, Reply>>,
    loads: Mutex<HashMap<u64, LoadReply>>,
}

impl Pending {
    fn encode_op(&self, op: Op) -> Option<FromExt> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        match op {
            Op::DescribeDevice(device, reply) => {
                self.replies
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(id, reply);
                Some(FromExt::DescribeDevice { id, device })
            }
            Op::DescribeEntity(entity, reply) => {
                self.replies
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(id, reply);
                Some(FromExt::DescribeEntity { id, entity })
            }
            Op::RemoveDevice(unique_id, reply) => {
                self.replies
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(id, reply);
                Some(FromExt::RemoveDevice { id, unique_id })
            }
            Op::RemoveEntity(unique_id, reply) => {
                self.replies
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(id, reply);
                Some(FromExt::RemoveEntity { id, unique_id })
            }
            Op::SetAvailability(target, availability, reply) => {
                self.replies
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(id, reply);
                Some(FromExt::SetAvailability {
                    id,
                    target,
                    availability,
                })
            }
            Op::SetHealth(health) => Some(FromExt::SetHealth { health }),
            Op::SetWaiting(waiting) => Some(FromExt::SetWaiting { waiting }),
            Op::SetUnmodeled(unmodeled) => Some(FromExt::SetUnmodeled { unmodeled }),
            Op::SetAvailableActions(actions) => Some(FromExt::SetAvailableActions { actions }),
            Op::Load(key, reply) => {
                self.loads
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(id, reply);
                Some(FromExt::Load { id, key })
            }
            Op::Store(key, value, reply) => {
                self.replies
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(id, reply);
                Some(FromExt::Store { id, key, value })
            }
        }
    }

    fn complete_reply(&self, id: u64, error: Option<String>) {
        if let Some(reply) = self
            .replies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id)
        {
            let result = match error {
                None => Ok(()),
                Some(message) => Err(Rejected(message)),
            };
            let _ = reply.send(result);
        }
    }

    fn complete_load(&self, id: u64, value: Option<serde_json::Value>, error: Option<String>) {
        if let Some(reply) = self
            .loads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id)
        {
            let result = match error {
                None => Ok(value),
                Some(message) => Err(Rejected(message)),
            };
            let _ = reply.send(result);
        }
    }
}

pub(crate) async fn read_json<T: for<'de> Deserialize<'de>>(
    reader: &mut BufReader<impl tokio::io::AsyncRead + Unpin>,
) -> Result<T, String> {
    let mut line = String::new();
    let n = reader
        .read_line(&mut line)
        .await
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("the host closed the connection".into());
    }
    serde_json::from_str(line.trim()).map_err(|e| format!("invalid message: {e}"))
}

pub(crate) async fn write_json<T: Serialize>(
    stdout: &tokio::sync::Mutex<impl AsyncWriteExt + Unpin>,
    value: &T,
) -> Result<(), String> {
    let mut line = serde_json::to_string(value).map_err(|e| e.to_string())?;
    line.push('\n');
    let mut out = stdout.lock().await;
    out.write_all(line.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    out.flush().await.map_err(|e| e.to_string())
}

/// Creates `dir` if it isn't there and, on Unix, restricts it to its owner.
///
/// A directory created with the process umask is, on a typical system, traversable by every
/// local user. `IRORI_EXTENSION_DATA` holds things like Zigbee2MQTT's `configuration.yaml` —
/// the network key and the pairing table. Mode `0700` matches the owner-only handling of
/// `secrets.toml`. It is set again after create: a directory that already existed keeps
/// whatever mode it was given, and the mode on `DirBuilder` does not apply to that case.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.recursive(true).create(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// A running extension process, as the host talks to it.
#[derive(Debug)]
pub struct ExtProcess {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: BufReader<ChildStdout>,
}

/// Runs `command`, and tries again when the kernel says the program file is still busy.
///
/// That error is `ETXTBSY` ("Text file busy"): exec found the file open for writing. An
/// extension starts from a binary that was just copied into its package, and the tests start a
/// script they just wrote. The writer has closed the file; the kernel can take a moment to
/// agree. A few short waits cover that window. Every other error comes back at once.
fn spawn_child(command: &mut Command) -> std::io::Result<Child> {
    const ATTEMPTS: u32 = 8;
    let mut attempt = 0;
    loop {
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && attempt + 1 < ATTEMPTS =>
            {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Starts the package's `run.command` with stdin/stdout piped for the protocol, and its stderr
/// piped for the host to read.
///
/// The stderr handle comes back separately rather than inside [`ExtProcess`] because it isn't
/// part of talking to the extension: the host hands it straight to a reader task and never looks
/// at it again. **Whoever takes it must read it continuously** — an undrained pipe fills, and the
/// child blocks forever on its next line of output, which for a chatty extension is seconds.
/// `state_dir` is handed to the child as `IRORI_EXTENSION_DATA`: the one directory it may keep
/// things in that outlive the package itself. It is created here if it isn't there, and on
/// Unix only its owner may read it.
pub fn spawn(
    package_dir: &Path,
    state_dir: &Path,
    run: &RunCommand,
    env: &[(String, std::path::PathBuf)],
) -> Result<(ExtProcess, ChildStderr), String> {
    let command = package_dir.join(run.command.as_str());
    if !command.is_file() {
        return Err(format!("package has no program at `{}`", command.display()));
    }
    // `current_dir` below moves the child to `package_dir` before its own program path is
    // resolved, so a relative `command` — true whenever `package_dir` itself is relative, which
    // it is by default (`./data/extensions/<id>`) — gets looked up against the child's *new*
    // cwd instead of this process's, and `execve` fails with ENOENT even though `is_file` just
    // found it fine. Canonicalizing first makes the two independent.
    let command = command
        .canonicalize()
        .map_err(|e| format!("couldn't resolve {}: {e}", command.display()))?;
    // Absolute, because the child's own working directory is `package_dir`: a relative path here
    // would mean somewhere inside the very directory this exists to stay out of.
    let state_dir = std::path::absolute(state_dir).unwrap_or_else(|_| state_dir.to_path_buf());
    create_private_dir(&state_dir)
        .map_err(|e| format!("couldn't create {}: {e}", state_dir.display()))?;
    let mut child_command = Command::new(&command);
    child_command
        .current_dir(package_dir)
        .env("IRORI_EXTENSION_DATA", &state_dir)
        .envs(env.iter().map(|(key, value)| (key, value)))
        .args(&run.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Piped, not inherited: an extension's own diagnostics are the only account of why it
        // failed, and inheriting them sends them to Irori's stderr where nothing can show them
        // to the person looking at the extension's card.
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = spawn_child(&mut child_command)
        .map_err(|e| format!("couldn't start {}: {e}", command.display()))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "child stdin missing".to_owned())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "child stdout missing".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "child stderr missing".to_owned())?;
    Ok((
        ExtProcess {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        },
        stderr,
    ))
}

impl ExtProcess {
    pub async fn send(&mut self, message: &ToExt) -> Result<(), String> {
        Self::send_on(&mut self.stdin, message).await
    }

    pub async fn send_on(stdin: &mut ChildStdin, message: &ToExt) -> Result<(), String> {
        let mut line = serde_json::to_string(message).map_err(|e| e.to_string())?;
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())
    }

    pub async fn recv(&mut self) -> Result<FromExt, String> {
        Self::recv_on(&mut self.stdout).await
    }

    pub async fn recv_on(stdout: &mut BufReader<ChildStdout>) -> Result<FromExt, String> {
        read_json(stdout).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::PackagePath;

    /// What an automations package built before kinds could take any action sends, byte for
    /// byte: a core that reads any action must still read it, and write it back the same way.
    #[test]
    fn call_service_reads_and_writes_what_older_engines_send() {
        for old in [
            serde_json::json!({
                "type": "call_service", "id": 1, "entity_id": "light.hall",
                "command": "turn_on", "data": {"brightness": 153},
                "run_id": "01K5B2Q9A1B2C3D4E5F6G7H8J9",
            }),
            serde_json::json!({
                "type": "call_service", "id": 2, "entity_id": "switch.plug",
                "command": "toggle", "run_id": "01K5B2Q9A1B2C3D4E5F6G7H8J9",
                "parent_id": "01K5B2Q9A1B2C3D4E5F6G7H8JA",
            }),
        ] {
            let msg: FromExt = serde_json::from_value(old.clone()).expect("an old engine's call");
            assert!(matches!(msg, FromExt::CallService { .. }));
            assert_eq!(serde_json::to_value(&msg).expect("ser"), old);
        }
    }

    #[test]
    fn hello_round_trips() {
        let msg = ToExt::Hello {
            settings: serde_json::json!({"sensor_interval_secs": 3}),
        };
        let json = serde_json::to_string(&msg).expect("ser");
        let back: ToExt = serde_json::from_str(&json).expect("de");
        assert!(matches!(back, ToExt::Hello { .. }));
        assert!(json.contains("\"type\":\"hello\""));
    }

    /// The failure this guards: a `reply` was sitting unread in stdin while `describe_*` waited
    /// on it, because the async stdin read was never polled. Applying the line has to complete
    /// that reply on its own, and a permit-join behind it has to be queued rather than block
    /// the reader.
    #[tokio::test]
    async fn a_reply_is_applied_without_waiting_on_the_runtime() {
        let pending = Pending::default();
        let (reply_tx, mut reply_rx) = oneshot::channel();
        pending
            .replies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(5, reply_tx);
        let (calls, _calls_rx) = tokio::sync::mpsc::channel(1);
        let (actions, mut actions_rx) = tokio::sync::mpsc::channel(1);
        let (out_tx, mut out_rx) = mpsc::channel(STDOUT_QUEUE);
        let runtime = tokio::runtime::Handle::current();

        let control = handle_host_line(
            r#"{"type":"reply","id":5,"error":null}"#,
            &pending,
            &calls,
            &actions,
            &out_tx,
            &runtime,
        );
        assert_eq!(control, ReadControl::Continue);
        assert!(
            reply_rx
                .try_recv()
                .expect("the reply was delivered")
                .is_ok()
        );

        let control = handle_host_line(
            r#"{"type":"action_call","id":0,"action_id":"permit_join"}"#,
            &pending,
            &calls,
            &actions,
            &out_tx,
            &runtime,
        );
        assert_eq!(control, ReadControl::Continue);
        let incoming = actions_rx.try_recv().expect("permit-join was queued");
        assert_eq!(incoming.action_id, "permit_join");

        let (incoming, _result) = host::incoming_action("sitting".into());
        actions.try_send(incoming).expect("the only slot is taken");
        let control = handle_host_line(
            r#"{"type":"action_call","id":1,"action_id":"permit_join"}"#,
            &pending,
            &calls,
            &actions,
            &out_tx,
            &runtime,
        );
        assert_eq!(control, ReadControl::Continue);
        let busy = out_rx.try_recv().expect("a full queue answers immediately");
        assert!(busy.contains("can't take this action yet"));
    }

    #[tokio::test]
    async fn a_full_stdout_queue_keeps_the_newer_report() {
        use irori_types::{State, SwitchState};

        fn report(on: bool) -> StateReport {
            StateReport {
                unique_id: UniqueId::try_from("lamp").expect("valid"),
                state: Some(State::Switch(SwitchState { on })),
                attributes: Default::default(),
                caused_by: None,
                replayed: false,
            }
        }

        let (ctx, host) = host::connect();
        let (tx, _rx) = mpsc::channel(1);
        tx.try_send("held\n".into()).expect("the only slot");
        ctx.report_state(report(false));
        let pending = host.reports.pop().expect("the report is waiting");
        assert!(matches!(
            offer_report(&tx, &host.reports, pending),
            Offer::Full
        ));
        ctx.report_state(report(true));
        assert_eq!(
            host.reports.pop().expect("the report is still waiting"),
            report(true)
        );
    }

    /// Restores the process's cwd on drop, so a failed assertion below can't leave every test
    /// that runs after this one resolving relative paths against the wrong directory.
    struct RestoreCwd(std::path::PathBuf);
    impl Drop for RestoreCwd {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    #[tokio::test]
    async fn spawn_finds_a_relative_package_dirs_program() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("a temp dir");
        std::fs::create_dir_all(tmp.path().join("pkg/bin")).expect("mkdir");
        let program = tmp.path().join("pkg/bin/prog");
        std::fs::write(&program, "#!/bin/sh\nexit 0\n").expect("write the program");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("make it executable");

        // The bug this guards against only shows up when `package_dir` is relative: `spawn`
        // sets the child's cwd to it before resolving its own (still relative) program path,
        // so this process's cwd has to actually be somewhere else for the run to be faithful.
        let _restore = RestoreCwd(std::env::current_dir().expect("cwd"));
        std::env::set_current_dir(tmp.path()).expect("chdir into the temp dir");

        let run = RunCommand {
            command: PackagePath::try_from("bin/prog").expect("a valid package path"),
            args: Vec::new(),
        };
        let state = tmp.path().join("state");
        let (process, _stderr) = spawn(Path::new("pkg"), &state, &run, &[]).expect(
            "spawn should resolve `pkg/bin/prog` against this process's cwd, \
             not the child's post-chdir one",
        );
        assert!(
            process.child.id().is_some(),
            "the program should have started"
        );
    }

    /// The host has to be able to read what an extension says for itself: inherited stderr goes
    /// to Irori's own output, where nothing can put it on the extension's card.
    #[tokio::test]
    async fn spawn_hands_back_the_childs_own_stderr_to_read() {
        use std::os::unix::fs::PermissionsExt;

        use tokio::io::AsyncBufReadExt as _;

        let tmp = tempfile::tempdir().expect("a temp dir");
        std::fs::create_dir_all(tmp.path().join("bin")).expect("mkdir");
        let program = tmp.path().join("bin/prog");
        std::fs::write(&program, "#!/bin/sh\necho 'no such port' >&2\nexit 1\n")
            .expect("write the program");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("make it executable");

        let run = RunCommand {
            command: PackagePath::try_from("bin/prog").expect("a valid package path"),
            args: Vec::new(),
        };
        let state = tmp.path().join("state");
        let (_process, stderr) = spawn(tmp.path(), &state, &run, &[]).expect("starts");

        let mut lines = tokio::io::BufReader::new(stderr).lines();
        assert_eq!(
            lines.next_line().await.expect("readable"),
            Some("no such port".to_owned())
        );
    }

    /// Extension data holds Zigbee2MQTT's network key. Left at the umask, that directory is
    /// traversable by every local user on a typical system, and the key with it.
    #[tokio::test]
    async fn spawn_restricts_the_extension_data_dir_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("a temp dir");
        std::fs::create_dir_all(tmp.path().join("bin")).expect("mkdir");
        let program = tmp.path().join("bin/prog");
        std::fs::write(&program, "#!/bin/sh\nexit 0\n").expect("write the program");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("make it executable");
        let run = RunCommand {
            command: PackagePath::try_from("bin/prog").expect("a valid package path"),
            args: Vec::new(),
        };

        let mode_of = |path: &Path| {
            std::fs::metadata(path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777
        };

        // Already there, and wider than it should be: create must tighten it, not leave it.
        let widened = tmp.path().join("already");
        std::fs::create_dir(&widened).expect("mkdir");
        std::fs::set_permissions(&widened, std::fs::Permissions::from_mode(0o755))
            .expect("widen it");
        let _existing = spawn(tmp.path(), &widened, &run, &[]).expect("starts");
        assert_eq!(
            mode_of(&widened),
            0o700,
            "an existing directory is tightened, not left as it was"
        );

        let fresh = tmp.path().join("fresh");
        let _created = spawn(tmp.path(), &fresh, &run, &[]).expect("starts");
        assert_eq!(
            mode_of(&fresh),
            0o700,
            "a directory this creates is owner-only from the start"
        );
    }

    /// Exec returns "Text file busy" while something still has the program open for writing.
    /// Spawn has to outlast that, because the host starts an extension from a file it just wrote.
    #[tokio::test]
    async fn spawn_retries_while_the_program_file_is_busy() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("a temp dir");
        std::fs::create_dir_all(tmp.path().join("bin")).expect("mkdir");
        let program = tmp.path().join("bin/prog");
        std::fs::write(&program, "#!/bin/sh\nexit 0\n").expect("write the program");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("make it executable");
        // Held open across the first exec attempts, then released while spawn is still retrying.
        let writing = std::fs::OpenOptions::new()
            .write(true)
            .open(&program)
            .expect("hold the program open for writing");
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(writing);
        });

        let run = RunCommand {
            command: PackagePath::try_from("bin/prog").expect("a valid package path"),
            args: Vec::new(),
        };
        let state = tmp.path().join("state");
        let started = spawn(tmp.path(), &state, &run, &[]);
        release.join().expect("the writer finishes");
        assert!(
            started.expect("starts").0.child.id().is_some(),
            "the program should have started once its file was closed"
        );
    }
}
