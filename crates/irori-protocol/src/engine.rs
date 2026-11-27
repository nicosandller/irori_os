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
//!         Incoming::RegistryChanged | Incoming::Missed { .. } | Incoming::Stop => {}
//!     }
//! }
//! ```

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use irori_types::{ContextId, Entity, EntityId, EntityState, Timestamp, UserId};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncWrite, BufReader};
use tokio::sync::{mpsc, oneshot};

/// How long a request waits for the core's answer. Longer than any service call (10 s) or
/// history read; past it, the core is taken to have lost the request.
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

use crate::process::{FromExt, MAX_LINE, ToExt, WireCommand, read_json, write_json};

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

/// Who is asking on the engine's page, as the core saw it (`docs/specs/access.md` §4.1). The
/// core puts this on every question; the page cannot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Caller {
    /// The person signed in. Absent while Irori is open and nobody has been set up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<UserId>,
    /// Whether they may change how the home is set up. A regular user's flows are theirs alone.
    pub owner: bool,
}

impl Caller {
    /// The home's own: an owner, or anybody in a home that has no sign-in.
    pub fn owner() -> Self {
        Self {
            user: None,
            owner: true,
        }
    }
}

/// What `get_reach` answers: where a person stands now, so a flow of theirs can be checked
/// against it (`docs/specs/access.md` §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonReach {
    /// Whether they are still in the home. A person who was removed leaves their flows behind.
    pub here: bool,
    /// What they are called now.
    #[serde(default)]
    pub name: String,
    /// Whether they may change how the home is set up.
    #[serde(default)]
    pub owner: bool,
    /// The entities they can see; absent means the whole home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entities: Option<Vec<EntityId>>,
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
    /// Who asked. From a core that says nothing about it, the home's own: such a core only ever
    /// lets an owner ask.
    pub caller: Caller,
    id: u64,
    out: Outbox,
}

impl AppRequest {
    /// Answers the page. `Err` is shown to the person, so say what went wrong in their terms.
    ///
    /// An answer longer than the core reads ([`MAX_LINE`]) would end the engine, and every flow
    /// with it; it goes back as an error saying it was too large instead.
    pub fn answer(self, result: Result<serde_json::Value, String>) {
        let (value, error) = match result {
            Ok(value) => (Some(value), None),
            Err(error) => (None, Some(error)),
        };
        let mut message = FromExt::AppAnswer {
            id: self.id,
            value,
            error,
        };
        let size = encoded_len(&message);
        if size > MAX_LINE {
            const MIB: f64 = 1024.0 * 1024.0;
            message = FromExt::AppAnswer {
                id: self.id,
                value: None,
                error: Some(format!(
                    "the answer to `{}` is too large to send ({:.1} MiB; an answer can be at most \
                     {:.0} MiB), so it wasn't sent. Try asking for less at a time.",
                    self.method,
                    size as f64 / MIB,
                    MAX_LINE as f64 / MIB,
                )),
            };
        }
        self.out.send(message);
    }
}

/// How many bytes `value` takes as JSON, without holding them.
fn encoded_len(value: &impl Serialize) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    // Counting can't fail; a value that can't be written fails again, the same way, when sent.
    let _ = serde_json::to_writer(&mut count, value);
    count.0
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
    /// The engine fell behind and wasn't sent this many of the home's events. A
    /// [`Incoming::RegistryChanged`] follows, to read the home again. What changed in between
    /// is gone: a trigger that needed one of those changes didn't fire.
    Missed {
        events: u64,
    },
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

    /// Where a person stands in the home now. Only an extension that limits what people can do
    /// is answered (`docs/specs/access.md` §4.1).
    pub async fn get_reach(&self, user: UserId) -> Result<PersonReach, EngineError> {
        self.ask_as(|id| FromExt::GetReach { id, user }).await
    }

    /// Tells the core something that happened to the home by the engine's doing, for the
    /// activity log: a flow turned off because the home changed under it. The core only takes
    /// the actions it names (`docs/specs/access.md` §3).
    pub fn note(&self, action: &str, target: &str, detail: &str) {
        self.out.send(FromExt::Note {
            action: action.to_owned(),
            target: target.to_owned(),
            detail: detail.to_owned(),
        });
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
                ToExt::RegistryChanged { missed } => {
                    if let Some(events) = missed
                        && in_tx.send(Incoming::Missed { events }).is_err()
                    {
                        return;
                    }
                    Incoming::RegistryChanged
                }
                ToExt::AppRequest {
                    id,
                    method,
                    params,
                    caller,
                } => Incoming::App(AppRequest {
                    method,
                    params,
                    caller: caller.unwrap_or_else(Caller::owner),
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
                | ToExt::LiveStart { .. }
                | ToExt::LiveStop { .. }
                | ToExt::TalkStart { .. }
                | ToExt::TalkAudio { .. }
                | ToExt::TalkStop { .. }
                | ToExt::ServiceCall { .. }
                | ToExt::ActionCall { .. }
                | ToExt::UnpairDevice { .. }
                | ToExt::CameraPicture { .. } => continue,
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
    /// A nudge after falling behind says how much was missed, before the nudge itself; an
    /// ordinary one doesn't (issue #97).
    #[tokio::test]
    async fn a_nudge_after_falling_behind_says_how_much_was_missed() {
        let (core_side, engine_side) = tokio::io::duplex(64 * 1024);
        let (engine_read, engine_write) = tokio::io::split(engine_side);
        let (_core_read, mut core_write) = tokio::io::split(core_side);

        core_write
            .write_all(b"{\"type\":\"hello\",\"settings\":{}}\n")
            .await
            .expect("write hello");
        let (_settings, _client, mut incoming) =
            connect(engine_read, engine_write).await.expect("connects");
        core_write
            .write_all(
                b"{\"type\":\"registry_changed\",\"missed\":12}\n{\"type\":\"registry_changed\"}\n",
            )
            .await
            .expect("write");
        assert!(matches!(
            incoming.recv().await,
            Some(Incoming::Missed { events: 12 })
        ));
        assert!(matches!(
            incoming.recv().await,
            Some(Incoming::RegistryChanged)
        ));
        assert!(matches!(
            incoming.recv().await,
            Some(Incoming::RegistryChanged)
        ));
    }

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

    /// An answer too long for the pipe goes back as an error to the same request, saying so,
    /// rather than as a line the core refuses by ending the engine; one at the bound goes as is.
    #[test]
    fn an_answer_longer_than_the_pipe_takes_goes_back_as_an_error() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let request = |id| AppRequest {
            method: "backtest".into(),
            params: serde_json::Value::Null,
            caller: Caller::owner(),
            id,
            out: Outbox(tx.clone()),
        };
        // A string answer whose message, newline not counted, is `len` bytes.
        let answer_of = |id, len: usize| {
            let empty = FromExt::AppAnswer {
                id,
                value: Some(serde_json::json!("")),
                error: None,
            };
            let shell = serde_json::to_string(&empty).expect("ser").len();
            serde_json::json!("x".repeat(len - shell))
        };

        request(1).answer(Ok(answer_of(1, MAX_LINE)));
        let Some(FromExt::AppAnswer { id, value, error }) = rx.try_recv().ok() else {
            panic!("an answer should go out");
        };
        assert_eq!((id, error), (1, None));
        assert!(value.is_some());

        request(2).answer(Ok(answer_of(2, MAX_LINE + 1)));
        let Some(FromExt::AppAnswer { id, value, error }) = rx.try_recv().ok() else {
            panic!("an answer should go out");
        };
        assert_eq!(id, 2);
        assert!(value.is_none(), "the answer itself shouldn't be sent");
        let error = error.expect("an error instead");
        assert!(
            error.contains("`backtest`") && error.contains("too large"),
            "{error}"
        );
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
