//! A fake Home Assistant: its WebSocket API on a local port, enough of it for this extension.
//! For the tests, and for the load gate (`dev/ha-load`, through `examples/fake_ha.rs`).
//!
//! It serves a [`World`] of floors, areas, devices, entities and states; sends changes to whoever
//! subscribed; carries out `call_service` on lights and switches; answers the recorder's reads
//! with a made-up past ([`past_value`]); and records every message it receives, so a test can
//! check what this extension sent.

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

async fn serve<S>(stream: S, shared: Arc<Shared>, mut closing: watch::Receiver<u64>)
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
/// What an entity was at `at` (seconds since the epoch), made up from what it is now: a number
/// swings a fifth either way over each day, anything with `on` and `off` takes turns every three
/// hours, and anything else stays as it is. The same moment always gives the same answer.
pub fn past_value(now: &str, at: f64) -> String {
    if let Ok(number) = now.parse::<f64>() {
        let swing = (number.abs() * 0.2).max(1.0);
        let value = number + swing * (at / 86_400.0 * std::f64::consts::TAU).sin();
        return format!("{:.1}", value);
    }
    match now {
        "on" | "off" => {
            if ((at / 10_800.0).floor() as i64) % 2 == 0 {
                "on".to_owned()
            } else {
                "off".to_owned()
            }
        }
        other => other.to_owned(),
    }
}

/// A read's `start_time` and `end_time`, in seconds since the epoch. No end is now.
fn period(asked: &Value) -> Option<(f64, f64)> {
    let seconds = |text: &Value| {
        let parsed: jiff::Timestamp = text.as_str()?.parse().ok()?;
        Some(parsed.as_millisecond() as f64 / 1000.0)
    };
    let start = seconds(&asked["start_time"])?;
    let end = seconds(&asked["end_time"])
        .unwrap_or_else(|| jiff::Timestamp::now().as_millisecond() as f64 / 1000.0);
    Some((start, end))
}

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
        "history/history_during_period" => {
            let Some((start, end)) = period(asked) else {
                return vec![error(id, "invalid_start_time", "Invalid start_time")];
            };
            let mut answer = Map::new();
            for entity_id in asked["entity_ids"].as_array().into_iter().flatten() {
                let Some(entity_id) = entity_id.as_str() else {
                    continue;
                };
                let Some((state, attributes)) = world.states.get(entity_id) else {
                    continue;
                };
                let numeric = state.parse::<f64>().ok();
                // A reading every quarter hour; a switch's change every three hours.
                let step = if numeric.is_some() { 900.0 } else { 10_800.0 };
                let mut rows = Vec::new();
                let mut at = start;
                while at < end {
                    let s = past_value(state, at);
                    let row = if rows.is_empty() {
                        json!({ "s": s, "a": attributes, "lu": at })
                    } else {
                        json!({ "s": s, "lu": at })
                    };
                    rows.push(row);
                    at = (at / step).floor() * step + step;
                }
                answer.insert(entity_id.to_owned(), Value::Array(rows));
            }
            vec![result(id, Value::Object(answer))]
        }
        "recorder/statistics_during_period" => {
            let Some((start, end)) = period(asked) else {
                return vec![error(id, "invalid_start_time", "Invalid start_time")];
            };
            let mut answer = Map::new();
            for entity_id in asked["statistic_ids"].as_array().into_iter().flatten() {
                let Some(entity_id) = entity_id.as_str() else {
                    continue;
                };
                let Some((state, attributes)) = world.states.get(entity_id) else {
                    continue;
                };
                if attributes.get("state_class").is_none() || state.parse::<f64>().is_err() {
                    continue;
                }
                let mut rows = Vec::new();
                let mut hour = (start / 3600.0).ceil() * 3600.0;
                while hour + 3600.0 <= end {
                    let mean: f64 = past_value(state, hour + 1800.0).parse().unwrap_or(0.0);
                    let spread = (mean.abs() * 0.05).max(0.5);
                    rows.push(json!({
                        "start": (hour * 1000.0) as i64, "end": ((hour + 3600.0) * 1000.0) as i64,
                        "mean": mean, "min": mean - spread, "max": mean + spread,
                        "change": spread,
                    }));
                    hour += 3600.0;
                }
                answer.insert(entity_id.to_owned(), Value::Array(rows));
            }
            vec![result(id, Value::Object(answer))]
        }
        "call_service" => {
            let context = shared.context();
            let domain = asked["domain"].as_str().unwrap_or("");
            let service = asked["service"].as_str().unwrap_or("");
            let entity_id = asked["target"]["entity_id"].as_str().unwrap_or("");
            let state = match (domain, service) {
                ("light" | "switch", "turn_on") => Some("on"),
                ("light" | "switch", "turn_off") => Some("off"),
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
