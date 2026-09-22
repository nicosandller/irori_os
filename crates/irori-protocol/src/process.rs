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
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::oneshot;

use crate::host::{Op, Reply, Reports};
use crate::{
    AvailabilityTarget, Health, IncomingCall, Protocol, ProtocolError, Rejected, ServiceError,
    ServiceErrorCode, host,
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

/// A running extension process, as the host talks to it.
#[derive(Debug)]
pub struct ExtProcess {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: BufReader<ChildStdout>,
}

/// Starts the package's `run.command` with stdin/stdout piped for the protocol.
pub fn spawn(package_dir: &Path, run: &RunCommand) -> Result<ExtProcess, String> {
    let command = package_dir.join(run.command.as_str());
    if !command.is_file() {
        return Err(format!("package has no program at `{}`", command.display()));
    }
    let mut child = Command::new(&command)
        .current_dir(package_dir)
        .args(&run.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
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
    Ok(ExtProcess {
        child,
        stdin,
        stdout: BufReader::new(stdout),
    })
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
}
