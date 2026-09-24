//! JSON-lines protocol between the core and an external extension process.
//!
//! The same `Protocol` trait runs in-process (tests, and the SDK) or as a child whose
//! stdin/stdout speak these messages (`docs/specs/extensions.md`).

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use irori_types::{
    Availability, DeviceDescription, EntityDescription, RunCommand, ServiceCall, StateReport,
    UniqueId, Waiting,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::oneshot;

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
}

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
    let mut stdin = BufReader::new(tokio::io::stdin());
    let stdout = Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));

    let hello = read_json(&mut stdin).await.map_err(ProtocolError::new)?;
    let ToExt::Hello { settings } = hello else {
        return Err(ProtocolError::new(
            "first message from the host must be hello",
        ));
    };
    let config: I::Config = serde_json::from_value(settings)
        .map_err(|e| ProtocolError::new(crate::invalid_settings(e)))?;

    let (ctx, host_end) = host::connect();
    let pending = Arc::new(Pending::default());

    let out = stdout.clone();
    let pending_out = Arc::clone(&pending);
    let outgoing = tokio::spawn(async move {
        pump_outgoing(host_end.ops, host_end.reports, out, pending_out).await;
    });

    let incoming = tokio::spawn(pump_incoming(
        stdin,
        stdout,
        host_end.stop,
        host_end.calls,
        host_end.actions,
        Arc::clone(&pending),
    ));

    let result = I::run(config, ctx).await;
    outgoing.abort();
    incoming.abort();
    result
}

async fn pump_outgoing(
    mut ops: tokio::sync::mpsc::Receiver<Op>,
    reports: Reports,
    stdout: Arc<tokio::sync::Mutex<tokio::io::Stdout>>,
    pending: Arc<Pending>,
) {
    loop {
        tokio::select! {
            Some(op) = ops.recv() => {
                if let Some(msg) = pending.encode_op(op) {
                    let _ = write_json(&stdout, &msg).await;
                }
            }
            () = reports.ready() => {
                for report in reports.drain() {
                    let _ = write_json(&stdout, &FromExt::StateReport { report }).await;
                }
            }
        }
    }
}

async fn pump_incoming(
    mut stdin: BufReader<tokio::io::Stdin>,
    stdout: Arc<tokio::sync::Mutex<tokio::io::Stdout>>,
    stop: tokio::sync::watch::Sender<bool>,
    calls: tokio::sync::mpsc::Sender<IncomingCall>,
    actions: tokio::sync::mpsc::Sender<IncomingAction>,
    pending: Arc<Pending>,
) {
    loop {
        match read_json::<ToExt>(&mut stdin).await {
            Ok(ToExt::Reply { id, error }) => pending.complete_reply(id, error),
            Ok(ToExt::Loaded { id, value, error }) => pending.complete_load(id, value, error),
            Ok(ToExt::ServiceCall { id, call }) => {
                let (incoming, result) = host::incoming_call(call);
                if calls.send(incoming).await.is_err() {
                    return;
                }
                let stdout = Arc::clone(&stdout);
                tokio::spawn(async move {
                    let error = match result.await {
                        Ok(Ok(())) => None,
                        Ok(Err(error)) => Some(WireServiceError::from(error)),
                        Err(_) => Some(WireServiceError {
                            code: "failed".into(),
                            message: "the protocol dropped the call".into(),
                        }),
                    };
                    let _ = write_json(&stdout, &FromExt::ServiceResult { id, error }).await;
                });
            }
            Ok(ToExt::ActionCall { id, action_id }) => {
                let (incoming, result) = host::incoming_action(action_id);
                if actions.send(incoming).await.is_err() {
                    return;
                }
                let stdout = Arc::clone(&stdout);
                tokio::spawn(async move {
                    let error = match result.await {
                        Ok(Ok(())) => None,
                        Ok(Err(error)) => Some(error),
                        Err(_) => Some("the protocol dropped the action call".to_owned()),
                    };
                    let _ = write_json(&stdout, &FromExt::ActionResult { id, error }).await;
                });
            }
            Ok(ToExt::Stop) => {
                let _ = stop.send(true);
                return;
            }
            Ok(ToExt::Hello { .. }) => {}
            Err(_) => {
                let _ = stop.send(true);
                return;
            }
        }
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

async fn read_json<T: for<'de> Deserialize<'de>>(
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

async fn write_json<T: Serialize>(
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
    let mut child = Command::new(&command)
        .current_dir(package_dir)
        .env("IRORI_EXTENSION_DATA", &state_dir)
        .args(&run.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Piped, not inherited: an extension's own diagnostics are the only account of why it
        // failed, and inheriting them sends them to Irori's stderr where nothing can show them
        // to the person looking at the extension's card.
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
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
        let (process, _stderr) = spawn(Path::new("pkg"), &state, &run).expect(
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
        let (_process, stderr) = spawn(tmp.path(), &state, &run).expect("starts");

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
        let _existing = spawn(tmp.path(), &widened, &run).expect("starts");
        assert_eq!(
            mode_of(&widened),
            0o700,
            "an existing directory is tightened, not left as it was"
        );

        let fresh = tmp.path().join("fresh");
        let _created = spawn(tmp.path(), &fresh, &run).expect("starts");
        assert_eq!(
            mode_of(&fresh),
            0o700,
            "a directory this creates is owner-only from the start"
        );
    }
}
