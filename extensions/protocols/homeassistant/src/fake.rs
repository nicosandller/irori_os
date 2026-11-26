//! A fake Home Assistant: its WebSocket API on a local port, enough of it for this extension.
//! For the tests, and for the load gate (`dev/ha-load`, through `examples/fake_ha.rs`).
//!
//! It serves a [`World`] of floors, areas, devices, entities and states; sends changes to whoever
//! subscribed; carries out `call_service` on lights and switches; and records every message it
//! receives, so a test can check what this extension sent.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;

/// What the fake Home Assistant holds.
#[derive(Debug, Clone, Default)]
pub struct World {
    pub token: String,
    /// Whether the token's user is an administrator: only they can read the registries.
    pub admin: bool,
    /// What `get_config` says Home Assistant is doing: `RUNNING` when `None`.
    pub running: Option<String>,
    pub floors: Vec<Value>,
    pub areas: Vec<Value>,
    pub devices: Vec<Value>,
    pub entities: Vec<Value>,
    /// State and attributes, by `entity_id`.
    pub states: BTreeMap<String, (String, Map<String, Value>)>,
    /// Automations' setups, by their `id`, as `GET /api/config/automation/config/<id>` gives
    /// them.
    pub automation_setups: BTreeMap<String, Value>,
    /// Service calls refused with this message, as Home Assistant refuses one it can't carry
    /// out: none when `None`.
    pub refuse_calls: Option<String>,
}

impl World {
    /// An empty home that takes `token` from an administrator.
    pub fn new(token: &str) -> Self {
        Self {
            token: token.to_owned(),
            admin: true,
            ..Self::default()
        }
    }

    pub fn floor(&mut self, id: &str, name: &str, level: i64) -> &mut Self {
        self.floors
            .push(json!({ "floor_id": id, "name": name, "level": level }));
        self
    }

    pub fn area(&mut self, id: &str, name: &str, floor: Option<&str>) -> &mut Self {
        self.areas
            .push(json!({ "area_id": id, "name": name, "floor_id": floor }));
        self
    }

    pub fn device(&mut self, device: Value) -> &mut Self {
        self.devices.push(device);
        self
    }

    /// An entity on `device`, with its registry entry id, state and attributes.
    pub fn entity(
        &mut self,
        id: &str,
        entity_id: &str,
        device: Option<&str>,
        state: &str,
        attributes: Value,
    ) -> &mut Self {
        self.entities.push(json!({
            "id": id,
            "entity_id": entity_id,
            "platform": "fake",
            "device_id": device,
            "area_id": null,
            "disabled_by": null,
            "hidden_by": null,
            "entity_category": null,
            "has_entity_name": true,
            "name": null,
            "original_name": if entity_id.starts_with("light.") || entity_id.starts_with("switch.") { Value::Null } else { json!(entity_id) },
        }));
        self.states.insert(
            entity_id.to_owned(),
            (
                state.to_owned(),
                attributes.as_object().cloned().unwrap_or_default(),
            ),
        );
        self
    }

    /// An automation: its state (`on` or `off`), and its setup under `id` when it has one (one
    /// written in YAML without an `id` has none to read).
    pub fn automation(
        &mut self,
        entity_id: &str,
        name: &str,
        on: bool,
        id: Option<&str>,
        setup: Value,
    ) -> &mut Self {
        let mut attributes = json!({ "friendly_name": name, "mode": "single" });
        if let Some(id) = id {
            attributes["id"] = json!(id);
            self.automation_setups.insert(id.to_owned(), setup);
        }
        self.states.insert(
            entity_id.to_owned(),
            (
                if on { "on" } else { "off" }.to_owned(),
                attributes.as_object().cloned().unwrap_or_default(),
            ),
        );
        self
    }

    /// A home of `devices` devices with `per_device` entities each, spread over three areas on
    /// two floors: on each device a light, a motion sensor, then temperature and power sensors.
    /// For the load gate.
    pub fn home(token: &str, devices: usize, per_device: usize) -> Self {
        let mut world = Self::new(token);
        world
            .floor("ground", "Ground floor", 0)
            .floor("first", "First floor", 1);
        world
            .area("living", "Living room", Some("ground"))
            .area("kitchen", "Kitchen", Some("ground"))
            .area("bedroom", "Bedroom", Some("first"));
        let areas = ["living", "kitchen", "bedroom"];
        let mut n = 0;
        for d in 0..devices {
            let device = format!("device{d:05}");
            world.device(json!({
                "id": device,
                "name": format!("Fake device {d}"),
                "manufacturer": "Fake",
                "model": "Load",
                "area_id": areas[d % areas.len()],
                "connections": [["mac", format!("02:00:00:{:02x}:{:02x}:{:02x}", (d >> 16) & 0xff, (d >> 8) & 0xff, d & 0xff)]],
            }));
            for k in 0..per_device {
                let id = format!("entry{n:06}");
                n += 1;
                match k {
                    0 => world.entity(&id, &format!("light.d{d}"), Some(&device), "off", json!({ "supported_color_modes": ["brightness"], "brightness": null })),
                    1 => world.entity(&id, &format!("binary_sensor.d{d}_motion"), Some(&device), "off", json!({ "device_class": "motion" })),
                    k if k % 2 == 0 => world.entity(&id, &format!("sensor.d{d}_temperature_{k}"), Some(&device), "20.0", json!({ "device_class": "temperature", "unit_of_measurement": "°C", "state_class": "measurement" })),
                    k => world.entity(&id, &format!("sensor.d{d}_power_{k}"), Some(&device), "10", json!({ "device_class": "power", "unit_of_measurement": "W", "state_class": "measurement" })),
                };
            }
        }
        world
    }
}

#[derive(Debug)]
enum Subscription {
    Entities(BTreeSet<String>),
    Events(String),
}

/// One open connection: its outbox, and its subscriptions by id.
type Open = (mpsc::UnboundedSender<String>, BTreeMap<u64, Subscription>);

#[derive(Debug, Default)]
struct Shared {
    world: Mutex<World>,
    received: Mutex<Vec<Value>>,
    /// Each plain HTTP request's first line (`GET /api/… HTTP/1.1`), in order.
    http: Mutex<Vec<String>>,
    /// Each open connection's outbox and subscriptions, by connection number.
    connections: Mutex<BTreeMap<u64, Open>>,
    contexts: AtomicU64,
    opened: AtomicU64,
}

impl Shared {
    fn lock_world(&self) -> std::sync::MutexGuard<'_, World> {
        self.world.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn context(&self) -> String {
        format!("ctx{:08}", self.contexts.fetch_add(1, Ordering::Relaxed))
    }

    /// Sends `entity_id`'s change to everyone following it.
    fn push_change(&self, entity_id: &str, plus: Value) {
        let connections = self
            .connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for (outbox, subscriptions) in connections.values() {
            for (id, subscription) in subscriptions {
                if let Subscription::Entities(ids) = subscription
                    && ids.contains(entity_id)
                {
                    let _ = outbox.send(
                        json!({ "id": id, "type": "event", "event": { "c": { entity_id: { "+": plus } } } })
                            .to_string(),
                    );
                }
            }
        }
    }
}

/// A running fake Home Assistant.
#[derive(Debug)]
pub struct FakeHa {
    /// `http://127.0.0.1:<port>`, as the settings take it.
    pub url: String,
    shared: Arc<Shared>,
    /// Bumped to close every open connection.
    close: watch::Sender<u64>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeHa {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeHa {
    /// Serves `world` on a free local port.
    pub async fn start(world: World) -> std::io::Result<Self> {
        Self::start_on(world, 0).await
    }

    /// Serves `world` on `port` (`0`: any free one).
    pub async fn start_on(world: World, port: u16) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        let url = format!("http://{}", listener.local_addr()?);
        let shared = Arc::new(Shared {
            world: Mutex::new(world),
            ..Shared::default()
        });
        let (close, closing) = watch::channel(0);
        let task = tokio::spawn({
            let shared = Arc::clone(&shared);
            async move {
                while let Ok((stream, _)) = listener.accept().await {
                    tokio::spawn(serve(stream, Arc::clone(&shared), closing.clone()));
                }
            }
        });
        Ok(Self {
            url,
            shared,
            close,
            task,
        })
    }

    /// Serves `world` over TLS on a free local port, with this certificate and key.
    pub async fn start_tls(
        world: World,
        certificate: rustls::pki_types::CertificateDer<'static>,
        key: rustls::pki_types::PrivateKeyDer<'static>,
    ) -> std::io::Result<Self> {
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(std::io::Error::other)?
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .map_err(std::io::Error::other)?;
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let url = format!("https://{}", listener.local_addr()?);
        let shared = Arc::new(Shared {
            world: Mutex::new(world),
            ..Shared::default()
        });
        let (close, closing) = watch::channel(0);
        let task = tokio::spawn({
            let shared = Arc::clone(&shared);
            async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let acceptor = acceptor.clone();
                    let shared = Arc::clone(&shared);
                    let closing = closing.clone();
                    tokio::spawn(async move {
                        if let Ok(stream) = acceptor.accept(stream).await {
                            serve(stream, shared, closing).await;
                        }
                    });
                }
            }
        });
        Ok(Self {
            url,
            shared,
            close,
            task,
        })
    }

    /// Changes the world; nothing is sent.
    pub fn with_world<T>(&self, change: impl FnOnce(&mut World) -> T) -> T {
        change(&mut self.shared.lock_world())
    }

    /// Sets an entity's state and some of its attributes, and sends the change. Answers with
    /// the change's context id.
    pub fn set_state(&self, entity_id: &str, state: &str, attributes: Value) -> String {
        let context = self.shared.context();
        self.change(entity_id, Some(state), attributes, &context);
        context
    }

    fn change(&self, entity_id: &str, state: Option<&str>, attributes: Value, context: &str) {
        let mut plus = json!({ "c": context, "lu": 1.0 });
        {
            let mut world = self.shared.lock_world();
            let Some((current, attrs)) = world.states.get_mut(entity_id) else {
                return;
            };
            if let Some(state) = state
                && state != current
            {
                state.clone_into(current);
                plus["s"] = json!(state);
            }
            if let Some(changed) = attributes.as_object().filter(|a| !a.is_empty()) {
                for (key, value) in changed {
                    attrs.insert(key.clone(), value.clone());
                }
                plus["a"] = Value::Object(changed.clone());
            }
        }
        self.shared.push_change(entity_id, plus);
    }

    /// Tells everyone following `event_type` it happened (`area_registry_updated`…).
    pub fn registry_changed(&self, event_type: &str) {
        let connections = self
            .shared
            .connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for (outbox, subscriptions) in connections.values() {
            for (id, subscription) in subscriptions {
                if matches!(subscription, Subscription::Events(kind) if kind == event_type) {
                    let _ = outbox.send(
                        json!({ "id": id, "type": "event", "event": { "event_type": event_type, "data": { "action": "update" } } })
                            .to_string(),
                    );
                }
            }
        }
    }

    /// Closes every open connection, as a restart of Home Assistant does.
    pub fn drop_connections(&self) {
        self.close.send_modify(|n| *n += 1);
    }

    /// Every message received, in order (the token is blanked out).
    pub fn received(&self) -> Vec<Value> {
        self.shared
            .received
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The `type` of every message received.
    pub fn received_types(&self) -> Vec<String> {
        self.received()
            .iter()
            .map(|message| message["type"].as_str().unwrap_or("").to_owned())
            .collect()
    }

    /// Each plain HTTP request's first line, in order: `GET /api/… HTTP/1.1`.
    pub fn http_requests(&self) -> Vec<String> {
        self.shared
            .http
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The `call_service` messages received.
    pub fn calls(&self) -> Vec<Value> {
        self.received()
            .into_iter()
            .filter(|message| message["type"] == "call_service")
            .collect()
    }

    /// How many connections have signed in.
    pub fn connections(&self) -> u64 {
        self.shared.opened.load(Ordering::Relaxed)
    }
}

/// A stream whose first bytes were already read: they're read again first.
struct Rewound<S> {
    head: Vec<u8>,
    at: usize,
    inner: S,
}

impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for Rewound<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.at < self.head.len() {
            let n = buf.remaining().min(self.head.len() - self.at);
            let at = self.at;
            buf.put_slice(&self.head[at..at + n]);
            self.at += n;
            return std::task::Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for Rewound<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Reads a request's head: a WebSocket upgrade goes on to the WebSocket API, anything else is
/// answered as a plain HTTP request.
async fn serve<S>(mut stream: S, shared: Arc<Shared>, closing: watch::Receiver<u64>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncReadExt as _;
    let mut head = Vec::new();
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let mut buffer = [0u8; 4096];
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buffer[..n]),
        }
        if head.len() > 64 * 1024 {
            return;
        }
    }
    let text = String::from_utf8_lossy(&head).to_ascii_lowercase();
    if text.contains("upgrade: websocket") {
        let stream = Rewound {
            head,
            at: 0,
            inner: stream,
        };
        serve_websocket(stream, shared, closing).await;
    } else {
        serve_http(stream, &head, &shared).await;
    }
}

/// One plain HTTP request: an automation's setup, with the token. Nothing else is served.
async fn serve_http<S>(mut stream: S, head: &[u8], shared: &Shared)
where
    S: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt as _;
    let head = String::from_utf8_lossy(head).into_owned();
    let first = head.lines().next().unwrap_or("").to_owned();
    shared
        .http
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(first.clone());
    let mut words = first.split_whitespace();
    let (method, path) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
    let (token, setups) = {
        let world = shared.lock_world();
        (world.token.clone(), world.automation_setups.clone())
    };
    let authorized = head
        .lines()
        .any(|line| line.eq_ignore_ascii_case(&format!("authorization: bearer {token}")));
    let (status, body) = if !authorized {
        ("401 Unauthorized", json!({ "message": "Unauthorized" }))
    } else if method != "GET" {
        ("405 Method Not Allowed", json!({ "message": "not here" }))
    } else if let Some(setup) = path
        .strip_prefix("/api/config/automation/config/")
        .and_then(|id| setups.get(id))
    {
        ("200 OK", setup.clone())
    } else {
        ("404 Not Found", json!({ "message": "Resource not found" }))
    };
    let body = body.to_string();
    let answer = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(answer.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn serve_websocket<S>(stream: S, shared: Arc<Shared>, mut closing: watch::Receiver<u64>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    closing.mark_unchanged();
    let Ok(socket) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    let (mut sink, mut source) = socket.split();
    let hello = json!({ "type": "auth_required", "ha_version": "2026.11.0" }).to_string();
    if sink.send(Message::text(hello)).await.is_err() {
        return;
    }
    let Some(Ok(Message::Text(auth))) = source.next().await else {
        return;
    };
    let auth: Value = serde_json::from_str(auth.as_str()).unwrap_or(Value::Null);
    let token_ok = auth["type"] == "auth" && auth["access_token"] == shared.lock_world().token;
    let mut recorded = auth.clone();
    if recorded.get("access_token").is_some() {
        recorded["access_token"] = json!("…");
    }
    shared
        .received
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(recorded);
    if !token_ok {
        let _ = sink
            .send(Message::text(
                json!({ "type": "auth_invalid", "message": "Invalid access token or password" })
                    .to_string(),
            ))
            .await;
        let _ = sink.close().await;
        return;
    }
    let answer = json!({ "type": "auth_ok", "ha_version": "2026.11.0" }).to_string();
    if sink.send(Message::text(answer)).await.is_err() {
        return;
    }
    let number = shared.opened.fetch_add(1, Ordering::Relaxed);
    let (outbox, mut out) = mpsc::unbounded_channel::<String>();
    shared
        .connections
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(number, (outbox.clone(), BTreeMap::new()));
    loop {
        tokio::select! {
            message = source.next() => {
                let Some(Ok(message)) = message else { break };
                let Message::Text(text) = message else { continue };
                let Ok(asked) = serde_json::from_str::<Value>(text.as_str()) else { continue };
                shared.received.lock().unwrap_or_else(PoisonError::into_inner).push(asked.clone());
                for reply in answer_to(&shared, number, &asked) {
                    let _ = outbox.send(reply.to_string());
                }
            }
            Some(text) = out.recv() => {
                if sink.send(Message::text(text)).await.is_err() {
                    break;
                }
            }
            _ = closing.changed() => {
                let _ = sink.close().await;
                break;
            }
        }
    }
    shared
        .connections
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&number);
}

fn result(id: &Value, result: Value) -> Value {
    json!({ "id": id, "type": "result", "success": true, "result": result })
}

fn error(id: &Value, code: &str, message: &str) -> Value {
    json!({ "id": id, "type": "result", "success": false, "error": { "code": code, "message": message } })
}

fn compact(state: &str, attributes: &Map<String, Value>) -> Value {
    json!({ "s": state, "a": attributes, "c": "ctx-initial", "lc": 1.0 })
}

/// What the fake answers `asked` with, in order.
fn answer_to(shared: &Shared, connection: u64, asked: &Value) -> Vec<Value> {
    let id = &asked["id"];
    let kind = asked["type"].as_str().unwrap_or("");
    let subscribe = |subscription: Subscription| {
        if let Some((_, subscriptions)) = shared
            .connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(&connection)
            && let Some(id) = id.as_u64()
        {
            subscriptions.insert(id, subscription);
        }
    };
    let world = shared.lock_world().clone();
    let registry = |list: &Vec<Value>| {
        if world.admin {
            result(id, json!(list))
        } else {
            error(id, "unauthorized", "Unauthorized")
        }
    };
    match kind {
        "get_config" => vec![result(
            id,
            json!({
                "location_name": "Fake home", "version": "2026.11.0",
                "unit_system": { "temperature": "°C" },
                "state": world.running.as_deref().unwrap_or("RUNNING"),
            }),
        )],
        "config/floor_registry/list" => vec![registry(&world.floors)],
        "config/area_registry/list" => vec![registry(&world.areas)],
        "config/device_registry/list" => vec![registry(&world.devices)],
        "config/entity_registry/list" => vec![registry(&world.entities)],
        "get_states" => vec![result(
            id,
            Value::Array(
                world
                    .states
                    .iter()
                    .map(|(entity_id, (state, attributes))| {
                        json!({ "entity_id": entity_id, "state": state, "attributes": attributes, "context": { "id": "ctx-initial" } })
                    })
                    .collect(),
            ),
        )],
        "subscribe_events" => {
            subscribe(Subscription::Events(
                asked["event_type"].as_str().unwrap_or("").to_owned(),
            ));
            vec![result(id, Value::Null)]
        }
        "subscribe_entities" => {
            let wanted: BTreeSet<String> = asked["entity_ids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            let added: Map<String, Value> = world
                .states
                .iter()
                .filter(|(entity_id, _)| wanted.contains(*entity_id))
                .map(|(entity_id, (state, attributes))| {
                    (entity_id.clone(), compact(state, attributes))
                })
                .collect();
            subscribe(Subscription::Entities(wanted));
            vec![
                result(id, Value::Null),
                json!({ "id": id, "type": "event", "event": { "a": added } }),
            ]
        }
        "unsubscribe_events" => {
            if let Some((_, subscriptions)) = shared
                .connections
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_mut(&connection)
                && let Some(subscription) = asked["subscription"].as_u64()
            {
                subscriptions.remove(&subscription);
            }
            vec![result(id, Value::Null)]
        }
        "call_service" => {
            let refused = shared.lock_world().refuse_calls.clone();
            if let Some(why) = refused {
                return vec![error(id, "home_assistant_error", &why)];
            }
            let context = shared.context();
            let domain = asked["domain"].as_str().unwrap_or("");
            let service = asked["service"].as_str().unwrap_or("");
            let entity_id = asked["target"]["entity_id"].as_str().unwrap_or("");
            let state = match (domain, service) {
                ("light" | "switch" | "automation", "turn_on") => Some("on"),
                ("light" | "switch" | "automation", "turn_off") => Some("off"),
                _ => None,
            };
            let mut plus = json!({ "c": context });
            {
                let mut world = shared.lock_world();
                if let Some((current, attributes)) = world.states.get_mut(entity_id) {
                    if let Some(state) = state {
                        state.clone_into(current);
                        plus["s"] = json!(state);
                    }
                    if let Some(brightness) = asked["service_data"].get("brightness") {
                        attributes.insert("brightness".into(), brightness.clone());
                        plus["a"] = json!({ "brightness": brightness });
                    }
                }
            }
            // As Home Assistant does: the change goes out before the answer.
            shared.push_change(entity_id, plus);
            vec![result(id, json!({ "context": { "id": context, "parent_id": null, "user_id": "fake" } }))]
        }
        _ => vec![error(id, "unknown_command", "Unknown command.")],
    }
}
