//! Home Assistant's WebSocket API: sign in, ask, and hear what it sends back. One request and its
//! answer are matched by `id`; events arrive on the subscription's `id`.
//!
//! **This extension never changes Home Assistant** (rule 5 of milestone 11). Everything it sends
//! is an [`Outgoing`], a closed list: signing in, the reads, subscribing and unsubscribing,
//! asking for a camera's stream to watch (`camera/stream`, which reads and changes nothing), and
//! `call_service`, which [`crate`] sends only in answer to a command someone gave in Irori. There
//! is no way to send a registry, area or automation write, and [`Client::request`] checks
//! every message's type against [`ALLOWED`] once more before it leaves.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use crate::transport::{OpenError, Stream, Transport};

/// Every message type this extension can send Home Assistant. Nothing else ever leaves.
pub const ALLOWED: &[&str] = &[
    "auth",
    "get_config",
    "config/floor_registry/list",
    "config/area_registry/list",
    "config/device_registry/list",
    "config/entity_registry/list",
    "get_states",
    "subscribe_entities",
    "subscribe_events",
    "unsubscribe_events",
    "camera/stream",
    "call_service",
];

/// The registry events a re-read follows.
pub const REGISTRY_EVENTS: [&str; 4] = [
    "floor_registry_updated",
    "area_registry_updated",
    "device_registry_updated",
    "entity_registry_updated",
];

/// How long connecting and signing in may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a read may take. Listing a large home's states is a few megabytes.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a service call may take: inside the 10 seconds the core waits for a command, so a
/// slow answer gets this extension's words, not a timeout.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(8);
/// Quiet this long, and the connection is asked for a WebSocket pong (not Home Assistant's own
/// `ping` message, which isn't in [`ALLOWED`]). Quiet twice as long, and it's taken as gone.
const QUIET: Duration = Duration::from_secs(30);
/// The largest message taken: `get_states` of a home of a few thousand entities, with room.
const MAX_MESSAGE: usize = 64 << 20;

/// A message for Home Assistant. Only these exist; see the module's notes.
#[derive(Debug, Clone, PartialEq)]
pub enum Outgoing {
    GetConfig,
    FloorRegistry,
    AreaRegistry,
    DeviceRegistry,
    EntityRegistry,
    GetStates,
    /// Compact state changes for these entities only.
    SubscribeEntities {
        entity_ids: Vec<String>,
    },
    SubscribeEvents {
        event_type: &'static str,
    },
    Unsubscribe {
        subscription: u64,
    },
    /// Where to watch a camera: an HLS playlist of fragmented MP4, for as long as it's read.
    CameraStream {
        entity_id: String,
    },
    /// Only ever built by [`crate::model::command`], from a command someone gave in Irori.
    CallService {
        domain: String,
        service: String,
        entity_id: String,
        data: serde_json::Map<String, Value>,
    },
}

impl Outgoing {
    /// Its `type` on the wire.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::GetConfig => "get_config",
            Self::FloorRegistry => "config/floor_registry/list",
            Self::AreaRegistry => "config/area_registry/list",
            Self::DeviceRegistry => "config/device_registry/list",
            Self::EntityRegistry => "config/entity_registry/list",
            Self::GetStates => "get_states",
            Self::SubscribeEntities { .. } => "subscribe_entities",
            Self::SubscribeEvents { .. } => "subscribe_events",
            Self::Unsubscribe { .. } => "unsubscribe_events",
            Self::CameraStream { .. } => "camera/stream",
            Self::CallService { .. } => "call_service",
        }
    }

    fn body(&self, id: u64) -> Value {
        let mut body = json!({ "id": id, "type": self.kind() });
        match self {
            Self::SubscribeEntities { entity_ids } => body["entity_ids"] = json!(entity_ids),
            Self::SubscribeEvents { event_type } => body["event_type"] = json!(event_type),
            Self::Unsubscribe { subscription } => body["subscription"] = json!(subscription),
            Self::CameraStream { entity_id } => {
                body["entity_id"] = json!(entity_id);
                body["format"] = json!("hls");
            }
            Self::CallService {
                domain,
                service,
                entity_id,
                data,
            } => {
                body["domain"] = json!(domain);
                body["service"] = json!(service);
                body["service_data"] = Value::Object(data.clone());
                body["target"] = json!({ "entity_id": entity_id });
                // Waits for the integration to carry it out, so the answer says it did.
                body["return_response"] = json!(false);
            }
            _ => {}
        }
        body
    }
}

/// Whether a message of this type may be sent. The last check before anything leaves.
pub fn allowed(kind: &str) -> bool {
    ALLOWED.contains(&kind)
}

/// Home Assistant's answer to a request it didn't carry out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HaError {
    /// Its error code (`unauthorized`, `not_found`…); `None` when no answer came.
    pub code: Option<String>,
    pub message: String,
}

/// [`HaError::code`] while Home Assistant isn't running yet, or any more.
const STARTING: &str = "irori_starting";

impl HaError {
    /// Home Assistant said it's in `state`, not `RUNNING`.
    pub fn starting(state: &str) -> Self {
        Self {
            code: Some(STARTING.to_owned()),
            message: format!(
                "Home Assistant is {}",
                state.to_ascii_lowercase().replace('_', " ")
            ),
        }
    }

    fn local(message: impl Into<String>) -> Self {
        Self {
            code: None,
            message: message.into(),
        }
    }

    /// Home Assistant is starting or stopping: its states aren't all there.
    pub fn is_starting(&self) -> bool {
        self.code.as_deref() == Some(STARTING)
    }

    /// The token's user isn't an administrator: the registries are theirs only.
    pub fn is_unauthorized(&self) -> bool {
        self.code.as_deref() == Some("unauthorized")
    }
}

impl fmt::Display for HaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// Why a connection couldn't be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectError {
    /// Home Assistant wasn't there, or stopped answering.
    Unreachable(String),
    /// It answered, and refused the token.
    Refused(String),
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(why) | Self::Refused(why) => f.write_str(why),
        }
    }
}

/// An event on one of this connection's subscriptions.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub subscription: u64,
    pub event: Value,
}

type Socket = WebSocketStream<Stream>;
type Answer = oneshot::Sender<Result<Value, HaError>>;

/// A signed-in connection. Clones share it.
#[derive(Debug, Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// Held while an id is taken and its message sent: Home Assistant refuses an id that isn't
    /// higher than the last, so two senders mustn't overtake each other.
    sink: tokio::sync::Mutex<(SplitSink<Socket, Message>, u64)>,
    /// Requests sent and not answered yet, by id. `None` once the connection closed, so a
    /// request after that fails at once instead of waiting out its timeout.
    waiting: Mutex<Option<HashMap<u64, Answer>>>,
}

/// A connection, the Home Assistant version it said, its events, and the task reading them.
/// The events end when the connection does.
#[derive(Debug)]
pub struct Connection {
    pub client: Client,
    pub version: Option<String>,
    /// Unbounded: the reader also delivers answers, so it must never wait on the events. Were it
    /// to, a request awaited while events pile up would never hear its answer.
    pub events: mpsc::UnboundedReceiver<Event>,
    pub reader: JoinHandle<()>,
}

/// Connects and signs in with `token`.
pub async fn connect(transport: &Transport, token: &str) -> Result<Connection, ConnectError> {
    tokio::time::timeout(CONNECT_TIMEOUT, connect_now(transport, token))
        .await
        .map_err(|_| {
            ConnectError::Unreachable(format!(
                "Home Assistant at {} didn't answer in time",
                transport.authority()
            ))
        })?
}

async fn connect_now(transport: &Transport, token: &str) -> Result<Connection, ConnectError> {
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE));
    // A certificate that can't be trusted ends it here, before the token is anywhere near it.
    let stream = transport.open().await.map_err(|why| match why {
        OpenError::Untrusted(why) => ConnectError::Refused(why),
        OpenError::Unreachable(why) => ConnectError::Unreachable(why),
    })?;
    let (socket, _) = tokio_tungstenite::client_async_with_config(
        transport.websocket_url(),
        stream,
        Some(config),
    )
    .await
    .map_err(|e| ConnectError::Unreachable(format!("couldn't reach Home Assistant: {e}")))?;
    let (mut sink, mut stream) = socket.split();
    let mut next = async || -> Result<Value, ConnectError> {
        loop {
            let message = stream
                .next()
                .await
                .ok_or_else(|| {
                    ConnectError::Unreachable("Home Assistant closed the connection".into())
                })?
                .map_err(|e| ConnectError::Unreachable(format!("the connection failed: {e}")))?;
            if let Some(value) = json_of(&message) {
                return Ok(value);
            }
        }
    };
    let hello = next().await?;
    if hello["type"] != "auth_required" {
        return Err(ConnectError::Unreachable(
            "that address doesn't answer like Home Assistant".into(),
        ));
    }
    let version = hello["ha_version"].as_str().map(str::to_owned);
    // The one message without an id. Through the same check as every other.
    debug_assert!(allowed("auth"));
    sink.send(Message::text(
        json!({ "type": "auth", "access_token": token }).to_string(),
    ))
    .await
    .map_err(|e| ConnectError::Unreachable(format!("the connection failed: {e}")))?;
    let answer = next().await?;
    match answer["type"].as_str() {
        Some("auth_ok") => {}
        Some("auth_invalid") => {
            return Err(ConnectError::Refused(
                "Home Assistant refused the access token. Make a new long-lived token and enter it in the settings.".into(),
            ));
        }
        _ => {
            return Err(ConnectError::Unreachable(
                "Home Assistant answered signing in with something unexpected".into(),
            ));
        }
    }
    let inner = Arc::new(Inner {
        sink: tokio::sync::Mutex::new((sink, 0)),
        waiting: Mutex::new(Some(HashMap::new())),
    });
    let (events_tx, events) = mpsc::unbounded_channel();
    let reader = tokio::spawn(read(stream, Arc::clone(&inner), events_tx));
    Ok(Connection {
        client: Client { inner },
        version,
        events,
        reader,
    })
}

fn json_of(message: &Message) -> Option<Value> {
    match message {
        Message::Text(text) => serde_json::from_str(text.as_str()).ok(),
        Message::Binary(bytes) => serde_json::from_slice(bytes).ok(),
        _ => None,
    }
}

/// Reads until the connection closes: answers go to whoever asked, events to the channel.
async fn read(
    mut stream: SplitStream<Socket>,
    inner: Arc<Inner>,
    events: mpsc::UnboundedSender<Event>,
) {
    let mut pinged = false;
    loop {
        let message = match tokio::time::timeout(QUIET, stream.next()).await {
            Ok(Some(Ok(message))) => message,
            Ok(Some(Err(e))) => {
                tracing::warn!(error = %e, "Home Assistant's connection failed");
                break;
            }
            Ok(None) => break,
            Err(_) if pinged => {
                tracing::warn!("Home Assistant stopped answering");
                break;
            }
            Err(_) => {
                pinged = true;
                let ping = inner
                    .sink
                    .lock()
                    .await
                    .0
                    .send(Message::Ping(Vec::new().into()))
                    .await;
                if ping.is_err() {
                    break;
                }
                continue;
            }
        };
        pinged = false;
        if matches!(message, Message::Close(_)) {
            break;
        }
        let Some(value) = json_of(&message) else {
            continue;
        };
        // One message, or several in a list.
        let messages = match value {
            Value::Array(list) => list,
            one => vec![one],
        };
        for value in messages {
            let Some(id) = value.get("id").and_then(Value::as_u64) else {
                continue;
            };
            match value["type"].as_str() {
                Some("event") => {
                    let event = Event {
                        subscription: id,
                        event: value.get("event").cloned().unwrap_or(Value::Null),
                    };
                    if events.send(event).is_err() {
                        return;
                    }
                }
                Some("result") => {
                    let answer = if value["success"].as_bool() == Some(true) {
                        Ok(value.get("result").cloned().unwrap_or(Value::Null))
                    } else {
                        Err(HaError {
                            code: value["error"]["code"].as_str().map(str::to_owned),
                            message: value["error"]["message"]
                                .as_str()
                                .unwrap_or("Home Assistant refused it")
                                .to_owned(),
                        })
                    };
                    let asked = inner
                        .waiting
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .as_mut()
                        .and_then(|waiting| waiting.remove(&id));
                    if let Some(asked) = asked {
                        let _ = asked.send(answer);
                    }
                }
                _ => {}
            }
        }
    }
    // Everyone still waiting hears it closed, by their sender dropping.
    inner
        .waiting
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
}

impl Client {
    /// Sends one message and waits up to `timeout` for its answer.
    pub async fn request(&self, message: Outgoing, timeout: Duration) -> Result<Value, HaError> {
        self.request_with_id(message, timeout)
            .await
            .map(|(_, answer)| answer)
    }

    /// Subscribes, and answers with the subscription's id: its events carry it.
    pub async fn subscribe(&self, message: Outgoing) -> Result<u64, HaError> {
        debug_assert!(matches!(
            message,
            Outgoing::SubscribeEntities { .. } | Outgoing::SubscribeEvents { .. }
        ));
        self.request_with_id(message, READ_TIMEOUT)
            .await
            .map(|(id, _)| id)
    }

    async fn request_with_id(
        &self,
        message: Outgoing,
        timeout: Duration,
    ) -> Result<(u64, Value), HaError> {
        let kind = message.kind();
        if !allowed(kind) {
            // Can't happen while `Outgoing` and `ALLOWED` agree; a test checks they do.
            return Err(HaError::local(format!("`{kind}` is never sent")));
        }
        let (tx, rx) = oneshot::channel();
        let id = {
            let mut sink = self.inner.sink.lock().await;
            sink.1 += 1;
            let id = sink.1;
            {
                let mut waiting = self
                    .inner
                    .waiting
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let Some(waiting) = waiting.as_mut() else {
                    return Err(HaError::local("Home Assistant isn't connected"));
                };
                waiting.insert(id, tx);
            }
            let sent = sink
                .0
                .send(Message::text(message.body(id).to_string()))
                .await;
            if let Err(e) = sent {
                drop(sink);
                self.forget(id);
                return Err(HaError::local(format!(
                    "couldn't reach Home Assistant: {e}"
                )));
            }
            id
        };
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(answer)) => answer.map(|answer| (id, answer)),
            Ok(Err(_)) => Err(HaError::local(
                "Home Assistant closed the connection before answering",
            )),
            Err(_) => {
                self.forget(id);
                Err(HaError::local("Home Assistant didn't answer in time"))
            }
        }
    }

    fn forget(&self, id: u64) {
        if let Some(waiting) = self
            .inner
            .waiting
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            waiting.remove(&id);
        }
    }

    /// Fails every request still waiting, at once, and any made after: the connection is being
    /// given up.
    pub fn give_up(&self) {
        self.inner
            .waiting
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }

    /// Closes the connection. The reader then ends, and the events with it.
    pub async fn close(&self) {
        let _ = self.inner.sink.lock().await.0.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_outgoing() -> Vec<Outgoing> {
        vec![
            Outgoing::GetConfig,
            Outgoing::FloorRegistry,
            Outgoing::AreaRegistry,
            Outgoing::DeviceRegistry,
            Outgoing::EntityRegistry,
            Outgoing::GetStates,
            Outgoing::SubscribeEntities {
                entity_ids: vec!["light.a".into()],
            },
            Outgoing::SubscribeEvents {
                event_type: REGISTRY_EVENTS[0],
            },
            Outgoing::Unsubscribe { subscription: 3 },
            Outgoing::CameraStream {
                entity_id: "camera.front".into(),
            },
            Outgoing::CallService {
                domain: "light".into(),
                service: "turn_on".into(),
                entity_id: "light.a".into(),
                data: serde_json::Map::new(),
            },
        ]
    }

    /// Rule 5: what can be sent is exactly the allowlist, and nothing on it writes.
    #[test]
    fn only_reads_subscriptions_and_service_calls_can_be_sent() {
        // Every variant, through an exhaustive match: a new one fails to compile until it's
        // added here, and then fails this test unless it's on the list.
        for message in every_outgoing() {
            match &message {
                Outgoing::GetConfig
                | Outgoing::FloorRegistry
                | Outgoing::AreaRegistry
                | Outgoing::DeviceRegistry
                | Outgoing::EntityRegistry
                | Outgoing::GetStates
                | Outgoing::SubscribeEntities { .. }
                | Outgoing::SubscribeEvents { .. }
                | Outgoing::Unsubscribe { .. }
                | Outgoing::CameraStream { .. }
                | Outgoing::CallService { .. } => {}
            }
            assert!(allowed(message.kind()), "{} isn't allowed", message.kind());
            assert_eq!(message.body(7)["type"], message.kind());
        }
        let kinds: Vec<&str> = every_outgoing().iter().map(Outgoing::kind).collect();
        for kind in ALLOWED {
            assert!(
                *kind == "auth" || kinds.contains(kind),
                "{kind} is allowed but nothing sends it"
            );
        }
        for kind in ALLOWED {
            let reads = kind.ends_with("/list") || kind.starts_with("get_");
            let subscribes = kind.starts_with("subscribe_") || kind.starts_with("unsubscribe_");
            // Where to watch a camera: an address to read, nothing changed.
            let watches = *kind == "camera/stream";
            assert!(
                reads || subscribes || watches || *kind == "auth" || *kind == "call_service",
                "{kind} isn't a read, a subscription or a service call"
            );
        }
        for write in [
            "config/area_registry/create",
            "config/area_registry/update",
            "config/area_registry/delete",
            "config/floor_registry/create",
            "config/device_registry/update",
            "config/device_registry/remove_config_entry",
            "config/entity_registry/update",
            "config/entity_registry/remove",
            "config/automation/config",
            "execute_script",
            "fire_event",
            "ping",
        ] {
            assert!(!allowed(write), "{write} is allowed");
        }
    }

    #[test]
    fn a_service_call_targets_one_entity() {
        let mut data = serde_json::Map::new();
        data.insert("brightness".into(), json!(128));
        let body = Outgoing::CallService {
            domain: "light".into(),
            service: "turn_on".into(),
            entity_id: "light.kitchen".into(),
            data,
        }
        .body(4);
        assert_eq!(
            body,
            json!({
                "id": 4,
                "type": "call_service",
                "domain": "light",
                "service": "turn_on",
                "service_data": { "brightness": 128 },
                "target": { "entity_id": "light.kitchen" },
                "return_response": false,
            })
        );
    }
}
