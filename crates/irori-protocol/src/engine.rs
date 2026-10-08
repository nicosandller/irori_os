//! The engine SDK: what an automation engine's process uses to talk to the core
//! (`docs/specs/automations.md` §B2).
//!
//! An engine doesn't implement [`crate::Protocol`]: it has no devices. It reads its settings from
//! `hello`, then asks the core for the registry and states, subscribes to changes, and calls
//! services, while answering what its own page asks it.
//!
//! ```ignore
//! let (settings, engine, mut incoming) = irori_protocol::engine::connect_stdio().await?;
//! engine.subscribe(true, true).await?;
//! while let Some(message) = incoming.recv().await {
//!     match message {
//!         Incoming::StateChanged { .. } => {}
//!         Incoming::App(request) => request.answer(Ok(serde_json::json!({}))),
//!         Incoming::RegistryChanged | Incoming::Stop => {}
//!     }
//! }
//! ```

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use irori_types::{ContextId, Entity, EntityId, EntityState, Timestamp};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncWrite, BufReader};
use tokio::sync::{mpsc, oneshot};

/// How long a request waits for the core's answer. Longer than any service call (10 s) or
/// history read; past it, the core is taken to have lost the request.
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

use crate::process::{FromExt, ToExt, WireCommand, read_json, write_json};

/// What `get_registry` answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Registry {
    pub entities: Vec<Entity>,
    /// Whether `irori.toml` has a timezone, so time triggers can be armed.
    #[serde(default)]
    pub timezone: bool,
    /// Whether it has a location, so sun triggers can be armed.
    #[serde(default)]
    pub location: bool,
    /// The time zone and the coordinates themselves. Absent from a core older than this field,
    /// which also answered `false` to both flags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<Place>,
}

/// Where the home is, as much as has been said (`home.toml`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Place {
    /// An IANA name, `Europe/Brussels`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
}

impl Place {
    /// `None` when the home says nothing about where it is.
    pub fn of(home: &irori_types::HomeSettings) -> Option<Self> {
        (!home.is_empty()).then(|| Self {
            time_zone: home.time_zone.as_ref().map(|zone| zone.as_str().to_owned()),
            latitude: home.location.as_ref().map(|at| at.latitude),
            longitude: home.location.as_ref().map(|at| at.longitude),
        })
    }
}

/// Why an engine request didn't work, as the core said it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineError(pub String);

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EngineError {}

/// A request from the engine's own page.
#[derive(Debug)]
pub struct AppRequest {
    pub method: String,
    pub params: serde_json::Value,
    id: u64,
    out: Outbox,
}

impl AppRequest {
    /// Answers the page. `Err` is shown to the person, so say what went wrong in their terms.
    pub fn answer(self, result: Result<serde_json::Value, String>) {
        let (value, error) = match result {
            Ok(value) => (Some(value), None),
            Err(error) => (None, Some(error)),
        };
        self.out.send(FromExt::AppAnswer {
            id: self.id,
            value,
            error,
        });
    }
}

/// What the core pushes to the engine.
#[derive(Debug)]
pub enum Incoming {
    StateChanged {
        entity_id: EntityId,
        old_state: Option<Box<EntityState>>,
        new_state: Box<EntityState>,
    },
    RegistryChanged,
    App(AppRequest),
    /// The core asked it to stop, or went away.
    Stop,
}

type Answer = oneshot::Sender<Result<serde_json::Value, EngineError>>;

#[derive(Debug, Clone)]
struct Outbox(mpsc::UnboundedSender<FromExt>);

impl Outbox {
    fn send(&self, message: FromExt) {
        let _ = self.0.send(message);
    }
}

/// The engine's handle to the core. Cheap to clone.
#[derive(Debug, Clone)]
pub struct EngineClient {
    out: Outbox,
    next_id: Arc<AtomicU64>,
    pending: Arc<Mutex<HashMap<u64, Answer>>>,
}

impl EngineClient {
    async fn ask(
        &self,
        build: impl FnOnce(u64) -> FromExt,
    ) -> Result<serde_json::Value, EngineError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, tx);
        self.out.send(build(id));
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(answer) => answer.unwrap_or_else(|_| Err(EngineError("the core went away".into()))),
            Err(_) => {
                self.pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&id);
                Err(EngineError(
                    "the core didn't answer within 30 seconds".into(),
                ))
            }
        }
    }

    async fn ask_as<T: for<'de> Deserialize<'de>>(
        &self,
        build: impl FnOnce(u64) -> FromExt,
    ) -> Result<T, EngineError> {
        let value = self.ask(build).await?;
        serde_json::from_value(value)
            .map_err(|e| EngineError(format!("the core's answer didn't fit: {e}")))
    }

    pub async fn get_registry(&self) -> Result<Registry, EngineError> {
        self.ask_as(|id| FromExt::GetRegistry { id }).await
    }

    pub async fn get_states(&self) -> Result<Vec<EntityState>, EngineError> {
        self.ask_as(|id| FromExt::GetStates { id }).await
    }

    pub async fn get_history(
        &self,
        entities: Vec<EntityId>,
        since: Timestamp,
    ) -> Result<BTreeMap<EntityId, Vec<EntityState>>, EngineError> {
        self.ask_as(|id| FromExt::GetHistory {
            id,
            entities,
            since,
        })
        .await
    }

    pub async fn subscribe(&self, states: bool, registry: bool) -> Result<(), EngineError> {
        self.ask(|id| FromExt::Subscribe {
            id,
            states,
            registry,
        })
        .await
        .map(|_| ())
    }

    /// Calls a service as run `run_id` of this engine. The error starts with a code
    /// (`unavailable: …`, see the spec).
    pub async fn call_service(
        &self,
        entity_id: EntityId,
        command: WireCommand,
        data: Option<serde_json::Map<String, serde_json::Value>>,
        run_id: ContextId,
        parent_id: Option<ContextId>,
    ) -> Result<(), EngineError> {
        self.ask(|id| FromExt::CallService {
            id,
            entity_id,
            command,
            data,
            run_id,
            parent_id,
        })
        .await
        .map(|_| ())
    }
}

/// Connects over this process's stdin and stdout, the way the core starts an engine. Returns its
/// settings (from `hello`), the client, and what the core pushes.
pub async fn connect_stdio() -> Result<
    (
        serde_json::Value,
        EngineClient,
        mpsc::UnboundedReceiver<Incoming>,
    ),
    EngineError,
> {
    connect(tokio::io::stdin(), tokio::io::stdout()).await
}

/// [`connect_stdio`] over any pair of streams, for tests.
pub async fn connect<R, W>(
    reader: R,
    writer: W,
) -> Result<
    (
        serde_json::Value,
        EngineClient,
        mpsc::UnboundedReceiver<Incoming>,
    ),
    EngineError,
>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut reader = BufReader::new(reader);
    let hello = read_json::<ToExt>(&mut reader).await.map_err(EngineError)?;
    let ToExt::Hello { settings } = hello else {
        return Err(EngineError(
            "first message from the host must be hello".into(),
        ));
    };

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<FromExt>();
    let writer = tokio::sync::Mutex::new(writer);
    tokio::spawn(async move {
        while let Some(message) = out_rx.recv().await {
            if write_json(&writer, &message).await.is_err() {
                return;
            }
        }
    });

    let out = Outbox(out_tx);
    let client = EngineClient {
        out: out.clone(),
        next_id: Arc::new(AtomicU64::new(0)),
        pending: Arc::new(Mutex::new(HashMap::new())),
    };
    // Unbounded: the reader must never wait on the engine. An engine that awaits an answer
    // while pushes pile up would otherwise block the reader on a full channel, and the answer
    // it's waiting for — behind those pushes — would never be read.
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let pending = Arc::clone(&client.pending);
    tokio::spawn(async move {
        loop {
            let message = match read_json::<ToExt>(&mut reader).await {
                Ok(message) => message,
                Err(_) => {
                    let _ = in_tx.send(Incoming::Stop);
                    return;
                }
            };
            let incoming = match message {
                ToExt::Answer { id, value, error } => {
                    let answer = pending
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&id);
                    if let Some(answer) = answer {
                        let _ = answer.send(match error {
                            None => Ok(value.unwrap_or(serde_json::Value::Null)),
                            Some(error) => Err(EngineError(error)),
                        });
                    }
                    continue;
                }
                ToExt::StateChanged {
                    entity_id,
                    old_state,
                    new_state,
                } => Incoming::StateChanged {
                    entity_id,
                    old_state,
                    new_state,
                },
                ToExt::RegistryChanged {} => Incoming::RegistryChanged,
                ToExt::AppRequest { id, method, params } => Incoming::App(AppRequest {
                    method,
                    params,
                    id,
                    out: out.clone(),
                }),
                ToExt::Stop => {
                    let _ = in_tx.send(Incoming::Stop);
                    return;
                }
                // Protocol traffic: an engine has no devices and makes no protocol requests.
                ToExt::Hello { .. }
                | ToExt::Reply { .. }
                | ToExt::Loaded { .. }
                | ToExt::ServiceCall { .. }
                | ToExt::ActionCall { .. }
                | ToExt::UnpairDevice { .. } => continue,
            };
            if in_tx.send(incoming).is_err() {
                return;
            }
        }
    });
    Ok((settings, client, in_rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    /// The client's requests go out as the spec's messages, and answers find their way back to
    /// the request that asked.
    #[tokio::test]
    async fn a_request_is_answered_by_its_own_id() {
        let (core_side, engine_side) = tokio::io::duplex(64 * 1024);
        let (engine_read, engine_write) = tokio::io::split(engine_side);
        let (core_read, mut core_write) = tokio::io::split(core_side);

        core_write
            .write_all(b"{\"type\":\"hello\",\"settings\":{\"a\":1}}\n")
            .await
            .expect("write hello");
        let (settings, client, mut incoming) =
            connect(engine_read, engine_write).await.expect("connects");
        assert_eq!(settings, serde_json::json!({"a": 1}));

        let asking = tokio::spawn({
            let client = client.clone();
            async move { client.get_states().await }
        });
        let mut lines = tokio::io::BufReader::new(core_read).lines();
        let line = lines.next_line().await.expect("read").expect("a line");
        assert_eq!(line, r#"{"type":"get_states","id":0}"#);
        core_write
            .write_all(b"{\"type\":\"app_request\",\"id\":7,\"method\":\"flows.list\"}\n")
            .await
            .expect("write");
        core_write
            .write_all(b"{\"type\":\"answer\",\"id\":0,\"value\":[]}\n")
            .await
            .expect("write");
        assert_eq!(asking.await.expect("joins"), Ok(Vec::new()));

        let Some(Incoming::App(request)) = incoming.recv().await else {
            panic!("the page's request should arrive");
        };
        assert_eq!(request.method, "flows.list");
        request.answer(Ok(serde_json::json!({"flows": []})));
        let line = lines.next_line().await.expect("read").expect("a line");
        assert_eq!(line, r#"{"type":"app_answer","id":7,"value":{"flows":[]}}"#);
    }

    /// A request the core never answers fails after a while, instead of hanging the engine.
    #[tokio::test(start_paused = true)]
    async fn an_unanswered_request_times_out() {
        let (core_side, engine_side) = tokio::io::duplex(64 * 1024);
        let (engine_read, engine_write) = tokio::io::split(engine_side);
        let (_core_read, mut core_write) = tokio::io::split(core_side);
        core_write
            .write_all(b"{\"type\":\"hello\",\"settings\":{}}\n")
            .await
            .expect("write hello");
        let (_, client, _incoming) = connect(engine_read, engine_write).await.expect("connects");
        let error = client.get_states().await.expect_err("no answer");
        assert!(error.0.contains("30 seconds"), "{error}");
    }
}
