//! The Matter server's WebSocket API, on loopback: one request and its answer, matched by
//! `message_id` (answers can come back in any order), and the events it sends once listening.
//!
//! This module, [`Event`] and the commands on [`Client`] are the whole of what the rest of the
//! extension asks of a Matter controller. A controller built on `rs-matter` instead
//! would provide the same and leave `map` and `lib` as they are (#82).

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::model::{Command, Node, NodeId, Path};

/// How many events can wait for the extension before the server's own socket waits too.
const EVENTS_CAPACITY: usize = 1024;
/// How long an ordinary command may take.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the server looks for the device on the network before giving up. Newer servers
/// take it; 1.4.0 looks until [`COMMISSION_TIMEOUT`] ends the wait.
const DISCOVERY_SECONDS: u64 = 45;
/// How long adding a device on the network may take in all: finding it, then commissioning
/// and reading it. Inside the time the manifest says the action takes, so the answer is this
/// extension's words, not the core's timeout.
pub const COMMISSION_TIMEOUT: Duration = Duration::from_secs(110);
/// The same over Bluetooth, which also hands the device the network and waits for it to join.
pub const BLUETOOTH_COMMISSION_TIMEOUT: Duration = Duration::from_secs(280);
/// How long a device has to answer a command. Inside the ten seconds the core waits for a
/// service call (`SERVICE_CALL_TIMEOUT`), so a device that doesn't answer gets this extension's
/// words.
const DEVICE_TIMEOUT: Duration = Duration::from_secs(8);
/// The window Door Lock's timed commands are given, as Home Assistant gives it.
const TIMED_REQUEST_MS: u64 = 10_000;

/// The [`CommandError::reason`] of a command whose answer didn't come in time.
pub const TIMED_OUT: &str = "timed-out";

/// Newer firmware the server found for a node, from the DCL or its own store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoftwareUpdate {
    /// The number the device compares (`SoftwareVersion`), what installing it asks for.
    pub version: u64,
    /// What people read (`SoftwareVersionString`).
    pub version_string: String,
    pub release_url: Option<String>,
}

impl SoftwareUpdate {
    fn from_json(value: &Value) -> Option<Self> {
        let version = value.get("software_version")?.as_u64()?;
        Some(Self {
            version,
            version_string: value
                .get("software_version_string")
                .and_then(Value::as_str)
                .map_or_else(|| version.to_string(), str::to_owned),
            release_url: value
                .get("release_notes_url")
                .and_then(Value::as_str)
                .filter(|url| url.starts_with("https://"))
                .map(str::to_owned),
        })
    }
}

/// What the server says about itself on connecting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    /// Identifies Irori's fabric. Device ids start with it, so a device commissioned onto a new
    /// fabric (after the server's storage is lost) is a new device, as it is on the network.
    ///
    /// Read straight from the server's JSON, which writes it whole: past JavaScript's safe
    /// integers, it only looks rounded once a JavaScript client parses it.
    pub compressed_fabric_id: u64,
    pub sdk_version: String,
}

impl ServerInfo {
    fn from_json(value: &Value) -> Result<Self, String> {
        Ok(Self {
            compressed_fabric_id: value
                .get("compressed_fabric_id")
                .and_then(Value::as_u64)
                .ok_or("the Matter server didn't say which fabric it's on")?,
            sdk_version: value
                .get("sdk_version")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
        })
    }
}

/// A command the server refused or couldn't carry out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError {
    /// The server's own error code; `None` when the answer never came.
    pub code: Option<i64>,
    /// The server's short reason, e.g. `not-discovered`, when it gives one.
    pub reason: Option<String>,
    pub details: String,
}

impl CommandError {
    fn local(details: impl Into<String>) -> Self {
        Self {
            code: None,
            reason: None,
            details: details.into(),
        }
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.details)
    }
}

/// Something the server tells every listener.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A node joined (`node_added`) or changed as a whole (`node_updated`): reachability,
    /// a fresh interview, an endpoint added or gone.
    Node(Node),
    NodeRemoved(NodeId),
    AttributeUpdated {
        node: NodeId,
        path: Path,
        value: Value,
    },
    /// The server is going away. The connection closes next.
    ServerShutdown,
}

impl Event {
    /// An event message, or `None` for one this extension doesn't use (`node_event`,
    /// `endpoint_added`: the server follows those with a `node_updated` when it matters).
    fn from_json(name: &str, data: &Value) -> Result<Option<Self>, String> {
        Ok(Some(match name {
            "node_added" | "node_updated" => Self::Node(Node::from_json(data)?),
            "node_removed" => Self::NodeRemoved(
                data.as_u64()
                    .or_else(|| data.get("node_id").and_then(Value::as_u64))
                    .ok_or("node_removed without a node id")?,
            ),
            "attribute_updated" => {
                let (Some(node), Some(path), Some(value)) = (
                    data.get(0).and_then(Value::as_u64),
                    data.get(1).and_then(Value::as_str),
                    data.get(2),
                ) else {
                    return Err("attribute_updated isn't [node, path, value]".to_owned());
                };
                Self::AttributeUpdated {
                    node,
                    path: path.parse()?,
                    value: value.clone(),
                }
            }
            "server_shutdown" => Self::ServerShutdown,
            _ => return Ok(None),
        }))
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Waiting = oneshot::Sender<Result<Value, CommandError>>;

/// A connection to the server. Clones share it.
#[derive(Debug, Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    sink: tokio::sync::Mutex<SplitSink<Socket, Message>>,
    /// Commands sent and not answered yet, by `message_id`. `None` once the connection closed,
    /// so a command sent after that fails at once rather than waiting out its timeout.
    waiting: Mutex<Option<HashMap<String, Waiting>>>,
    sent: AtomicU64,
}

/// The connection, what the server said about itself, its events, and the task reading them.
/// The events end when the connection does.
pub struct Connection {
    pub client: Client,
    pub info: ServerInfo,
    pub events: mpsc::Receiver<Event>,
    pub reader: JoinHandle<()>,
}

/// Connects once. The server sends its `server_info` first, before anything is asked.
pub async fn connect(url: &str) -> Result<Connection, String> {
    let (socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|e| format!("couldn't reach the Matter server at {url}: {e}"))?;
    let (sink, mut stream) = socket.split();
    let info = loop {
        let message = tokio::time::timeout(COMMAND_TIMEOUT, stream.next())
            .await
            .map_err(|_| "the Matter server didn't say hello".to_owned())?
            .ok_or("the Matter server closed the connection straight away")?
            .map_err(|e| format!("the Matter server's connection failed: {e}"))?;
        if let Some(value) = json_of(&message) {
            break ServerInfo::from_json(&value)?;
        }
    };
    let inner = Arc::new(Inner {
        sink: tokio::sync::Mutex::new(sink),
        waiting: Mutex::new(Some(HashMap::new())),
        sent: AtomicU64::new(0),
    });
    let (events_tx, events) = mpsc::channel(EVENTS_CAPACITY);
    let reader = tokio::spawn(read(stream, Arc::clone(&inner), events_tx));
    Ok(Connection {
        client: Client { inner },
        info,
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
    mut stream: futures_util::stream::SplitStream<Socket>,
    inner: Arc<Inner>,
    events: mpsc::Sender<Event>,
) {
    while let Some(message) = stream.next().await {
        let message = match message {
            Ok(message) => message,
            Err(e) => {
                tracing::warn!(error = %e, "the Matter server's connection failed");
                break;
            }
        };
        if matches!(message, Message::Close(_)) {
            break;
        }
        let Some(value) = json_of(&message) else {
            continue;
        };
        if let Some(name) = value.get("event").and_then(Value::as_str) {
            let data = value.get("data").unwrap_or(&Value::Null);
            match Event::from_json(name, data) {
                Ok(Some(event)) => {
                    if events.send(event).await.is_err() {
                        break;
                    }
                }
                Ok(None) => {}
                Err(why) => tracing::debug!(event = name, %why, "skipping an event"),
            }
            continue;
        }
        let Some(id) = value.get("message_id").map(id_text) else {
            continue;
        };
        let answer = if let Some(error_code) = value.get("error_code") {
            Err(CommandError {
                code: error_code.as_i64(),
                reason: value
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                details: value
                    .get("details")
                    .and_then(Value::as_str)
                    .unwrap_or("the Matter server refused it")
                    .to_owned(),
            })
        } else {
            Ok(value.get("result").cloned().unwrap_or(Value::Null))
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
    // Everyone still waiting hears it closed, by their sender dropping.
    inner
        .waiting
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
}

/// A `message_id` as text, whether it came back as a string or a number.
fn id_text(id: &Value) -> String {
    match id {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

impl Client {
    /// Sends one command and waits up to `timeout` for its answer.
    pub async fn command(
        &self,
        command: &str,
        args: Value,
        timeout: Duration,
    ) -> Result<Value, CommandError> {
        let id = (self.inner.sent.fetch_add(1, Ordering::Relaxed) + 1).to_string();
        let (tx, rx) = oneshot::channel();
        {
            let mut waiting = self
                .inner
                .waiting
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let Some(waiting) = waiting.as_mut() else {
                return Err(CommandError::local("the Matter server isn't connected"));
            };
            waiting.insert(id.clone(), tx);
        }
        let text = json!({"message_id": id, "command": command, "args": args}).to_string();
        let sent = self.inner.sink.lock().await.send(Message::text(text)).await;
        if let Err(e) = sent {
            self.forget(&id);
            return Err(CommandError::local(format!(
                "couldn't reach the Matter server: {e}"
            )));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(CommandError::local(
                "the Matter server closed the connection before answering",
            )),
            Err(_) => {
                self.forget(&id);
                Err(CommandError {
                    reason: Some(TIMED_OUT.to_owned()),
                    ..CommandError::local("the Matter server didn't answer in time")
                })
            }
        }
    }

    fn forget(&self, id: &str) {
        if let Some(waiting) = self
            .inner
            .waiting
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            waiting.remove(id);
        }
    }

    /// Starts the events, and answers with every node the server knows.
    pub async fn start_listening(&self) -> Result<Vec<Node>, CommandError> {
        let answer = self
            .command("start_listening", json!({}), COMMAND_TIMEOUT)
            .await?;
        let nodes = answer
            .as_array()
            .ok_or_else(|| CommandError::local("the Matter server's node list isn't a list"))?;
        Ok(nodes
            .iter()
            .filter_map(|node| match Node::from_json(node) {
                Ok(node) => Some(node),
                Err(why) => {
                    tracing::warn!(%why, "skipping a node the Matter server listed");
                    None
                }
            })
            .collect())
    }

    /// Finds the device the code is for and commissions it onto Irori's fabric. On the network
    /// only, or, with `bluetooth`, also over Bluetooth: a new device that isn't on a network yet
    /// is handed the Wi-Fi or Thread network the server was given
    /// ([`Client::set_wifi_credentials`], [`Client::set_thread_dataset`]).
    pub async fn commission(&self, code: &str, bluetooth: bool) -> Result<Node, CommandError> {
        let answer = self
            .command(
                "commission_with_code",
                json!({
                    "code": code,
                    "network_only": !bluetooth,
                    "discovery_timeout_s": DISCOVERY_SECONDS,
                }),
                if bluetooth {
                    BLUETOOTH_COMMISSION_TIMEOUT
                } else {
                    COMMISSION_TIMEOUT
                },
            )
            .await?;
        Node::from_json(&answer).map_err(CommandError::local)
    }

    /// The Wi-Fi network a device commissioned over Bluetooth joins. The server keeps it in its
    /// storage.
    pub async fn set_wifi_credentials(
        &self,
        ssid: &str,
        password: &str,
    ) -> Result<(), CommandError> {
        self.command(
            "set_wifi_credentials",
            json!({"ssid": ssid, "credentials": password}),
            COMMAND_TIMEOUT,
        )
        .await
        .map(|_| ())
    }

    /// The Thread network a Thread device commissioned over Bluetooth joins: the operational
    /// dataset, as hex. The server keeps it in its storage.
    pub async fn set_thread_dataset(&self, dataset: &str) -> Result<(), CommandError> {
        self.command(
            "set_thread_dataset",
            json!({"dataset": dataset}),
            COMMAND_TIMEOUT,
        )
        .await
        .map(|_| ())
    }

    /// Sends one command or attribute write to a node, and waits for the device to accept it.
    pub async fn send(&self, node: NodeId, command: &Command) -> Result<(), CommandError> {
        match command {
            Command::Invoke {
                endpoint,
                cluster,
                name,
                payload,
                timed,
            } => {
                let mut args = json!({
                    "node_id": node,
                    "endpoint_id": endpoint,
                    "cluster_id": cluster,
                    "command_name": name,
                    "payload": payload,
                });
                if *timed {
                    args["timed_request_timeout_ms"] = json!(TIMED_REQUEST_MS);
                }
                self.command("device_command", args, DEVICE_TIMEOUT)
                    .await
                    .map(|_| ())
            }
            Command::Write { path, value } => {
                let answer = self
                    .command(
                        "write_attribute",
                        json!({"node_id": node, "attribute_path": path.to_string(), "value": value}),
                        DEVICE_TIMEOUT,
                    )
                    .await?;
                // One status per attribute written; anything but 0 is the device refusing it.
                let refused = answer
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|status| status.get("Status").and_then(Value::as_u64))
                    .find(|&status| status != 0);
                match refused {
                    None => Ok(()),
                    Some(status) => Err(CommandError::local(format!(
                        "the device refused to change {path} (status {status:#04x})"
                    ))),
                }
            }
        }
    }

    /// Newer firmware for the node, if the server knows of any. The server checks by itself
    /// now and then; this answers from what it found, or asks the DCL.
    pub async fn check_update(&self, node: NodeId) -> Result<Option<SoftwareUpdate>, CommandError> {
        let answer = self
            .command(
                "check_node_update",
                json!({"node_id": node}),
                COMMAND_TIMEOUT,
            )
            .await?;
        Ok(SoftwareUpdate::from_json(&answer))
    }

    /// Tells the node to install `version`. The server downloads it and hands it over; the
    /// node's OTA Software Update Requestor says how far it has got.
    pub async fn install_update(&self, node: NodeId, version: u64) -> Result<(), CommandError> {
        self.command(
            "update_node",
            json!({"node_id": node, "software_version": version}),
            COMMAND_TIMEOUT,
        )
        .await
        .map(|_| ())
    }

    /// Takes Irori's fabric off the node, and forgets it. Other controllers it was shared with
    /// keep it: the server removes only its own fabric from the device. A node that can't be
    /// reached is only forgotten here.
    pub async fn remove(&self, node: NodeId) -> Result<(), CommandError> {
        self.command("remove_node", json!({"node_id": node}), COMMAND_TIMEOUT)
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_read_in_matter_terms() {
        assert_eq!(
            Event::from_json("attribute_updated", &json!([4, "1/1030/0", 1])),
            Ok(Some(Event::AttributeUpdated {
                node: 4,
                path: Path::new(1, 1030, 0),
                value: json!(1),
            }))
        );
        assert_eq!(
            Event::from_json("node_removed", &json!(7)),
            Ok(Some(Event::NodeRemoved(7)))
        );
        assert_eq!(
            Event::from_json("node_event", &json!({})),
            Ok(None),
            "not one this extension uses"
        );
        assert!(Event::from_json("attribute_updated", &json!([4, "1/6"])).is_err());
        assert!(matches!(
            Event::from_json(
                "node_updated",
                &json!({"node_id": 3, "available": false, "attributes": {}})
            ),
            Ok(Some(Event::Node(Node {
                id: 3,
                available: false,
                ..
            })))
        ));
    }

    #[test]
    fn the_fabric_id_is_read_exactly() {
        // As matter-server 1.4.0 sends it: past JavaScript's safe integers, but written whole.
        let info: Value = serde_json::from_str(
            r#"{"fabric_id":1,"compressed_fabric_id":10256683378676567378,
                "sdk_version":"matter-server/1.4.0 (matter.js/0.17.9)"}"#,
        )
        .expect("JSON");
        assert_eq!(
            ServerInfo::from_json(&info),
            Ok(ServerInfo {
                compressed_fabric_id: 10_256_683_378_676_567_378,
                sdk_version: "matter-server/1.4.0 (matter.js/0.17.9)".to_owned(),
            })
        );
        assert!(ServerInfo::from_json(&json!({"fabric_id": 1})).is_err());
    }

    #[test]
    fn a_message_id_matches_as_text_or_number() {
        assert_eq!(id_text(&json!("12")), "12");
        assert_eq!(id_text(&json!(12)), "12");
    }
}
