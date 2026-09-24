//! Zigbee devices, through a Zigbee2MQTT instance this extension installs and manages itself
//! (`docs/specs/protocols.md` §11 — HA discovery, and everything about how Z2M itself is set
//! up, is entirely this protocol's own business).
//!
//! On start: provision Node.js and Zigbee2MQTT if they aren't already there (`provision`), write
//! Zigbee2MQTT's `configuration.yaml` (`config`), start an embedded broker purely for Zigbee2MQTT
//! to publish into (`broker`), and spawn Zigbee2MQTT itself (`supervisor`). From there this
//! extension is its own MQTT client to that broker, consuming HA Discovery exactly like
//! `irori-protocol-mqtt` does against a real one — the describe/apply/route shape below
//! deliberately mirrors that crate's `lib.rs`; the two could share a run loop in
//! `irori-ha-discovery` if a third protocol ever needed the same shape, but weren't factored out
//! for two, to avoid guessing at an abstraction neither has proven yet.
//!
//! If Zigbee2MQTT's own process exits, `run` returns an error — the same as any other crash —
//! so the core's own restart-with-backoff (`docs/specs/protocols.md` §3) is what brings it back,
//! rather than a second retry loop duplicating that logic inside this extension.

mod broker;
mod config;
mod provision;
pub mod settings;
mod supervisor;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use irori_ha_discovery::discovery::{AvailabilityTopic, EntityTopics};
use irori_ha_discovery::{bridge, discovery, map, state, topic};
use irori_protocol::{
    AvailabilityTarget, Health, IncomingAction, IncomingCall, Protocol, ProtocolContext,
    ProtocolError, ServiceError,
};
use irori_types::{Availability, State, StateReport, UniqueId};

use crate::broker::{BrokerEvent, Connectivity, Message, Publisher};
use crate::settings::Settings;

/// Zigbee2MQTT's own base topic — fixed, not a setting: there's no reason for a Zigbee2MQTT this
/// extension installs itself to use anything but its own default.
const BASE_TOPIC: &str = "zigbee2mqtt";
/// HA Discovery's own default prefix, same reasoning.
const DISCOVERY_PREFIX: &str = "homeassistant";
/// Zigbee2MQTT's own state directory — its `configuration.yaml`, and the database holding the
/// network key and every device it has paired.
///
/// Under `IRORI_EXTENSION_DATA` (`docs/specs/protocols.md` §5), never inside this package's own
/// directory: uninstalling deletes the package whole, and putting the network's identity there
/// would mean an uninstall — which is also how an upgrade happens today — silently strands every
/// paired device and makes them all need re-pairing by hand. Falls back to the working directory
/// only for a host too old to set it, where the old behaviour is still better than refusing to
/// start.
fn data_dir() -> std::path::PathBuf {
    match std::env::var_os("IRORI_EXTENSION_DATA") {
        Some(given) => std::path::PathBuf::from(given).join("z2m-data"),
        None => std::path::PathBuf::from("z2m-data"),
    }
}
/// The one action this protocol declares (`irori-extension.toml`).
const PERMIT_JOIN: &str = "permit_join";

/// The Zigbee protocol.
#[derive(Debug)]
pub struct Zigbee;

impl Protocol for Zigbee {
    type Config = Settings;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");

    async fn run(settings: Settings, ctx: ProtocolContext) -> Result<(), ProtocolError> {
        run(settings, ctx).await
    }
}

#[derive(Debug, Clone)]
struct Entity {
    topics: EntityTopics,
    /// The last state successfully decoded for this entity, if any — see the matching field in
    /// `irori-protocol-mqtt`'s own `lib.rs` for why (`irori_ha_discovery::state::decode`'s
    /// `previous` parameter).
    last_state: Option<State>,
}

#[derive(Debug, Default)]
struct Registry {
    entities: BTreeMap<UniqueId, Entity>,
    config_topics: BTreeMap<String, UniqueId>,
    state_topics: BTreeMap<String, Vec<UniqueId>>,
    /// By topic: every entity listening on it, each with its own pair of payload words
    /// (`irori_ha_discovery::discovery::Listener` says why the pair isn't the topic's).
    availability_topics: BTreeMap<String, Vec<discovery::Listener>>,
    /// Whether Zigbee2MQTT's own bridge/info has been seen yet — `permit_join` is only declared
    /// available once it has, since that's the confirmation Zigbee2MQTT itself is up and
    /// connected, not just that our embedded broker is.
    bridge_seen: bool,
}

async fn run(settings: Settings, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
    ctx.set_health(Health::Degraded(
        "installing Node.js and Zigbee2MQTT".to_owned(),
    ))
    .await;
    provision::ensure_node().await.map_err(ProtocolError::new)?;
    provision::ensure_zigbee2mqtt(settings.zigbee2mqtt_version.as_deref())
        .await
        .map_err(ProtocolError::new)?;

    let data_dir = data_dir();
    let config_path = data_dir.join("configuration.yaml");
    // Zigbee2MQTT persists its own generated `network_key`/`pan_id` back into this same file
    // when neither is configured — read whatever's there before overwriting it, so regenerating
    // below doesn't silently erase that generated identity on every restart.
    let existing_advanced = tokio::fs::read_to_string(&config_path)
        .await
        .ok()
        .and_then(|text| config::existing_advanced_block(&text));
    let yaml = config::generate(
        &settings,
        settings.broker_port,
        BASE_TOPIC,
        existing_advanced.as_deref(),
    )
    .map_err(ProtocolError::new)?;
    tokio::fs::create_dir_all(&data_dir)
        .await
        .map_err(|e| ProtocolError::new(format!("couldn't create {}: {e}", data_dir.display())))?;
    write_atomically(&config_path, &yaml)
        .await
        .map_err(|e| ProtocolError::new(format!("couldn't write configuration.yaml: {e}")))?;

    broker::start_embedded(settings.broker_port).map_err(ProtocolError::new)?;
    // Long enough for the embedded broker's own thread to actually be listening before anything
    // tries to connect to it — both Zigbee2MQTT and our own client below would just retry
    // otherwise, but there's no reason to make them.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Zigbee2MQTT is spawned with its own `current_dir`, so every path handed to it has to be
    // absolute — `provision::node_binary` already is (see its own note there).
    let node = provision::node_binary();
    let entry = absolute(provision::zigbee2mqtt_entry());
    let data_dir = absolute(data_dir);
    let mut spawned = supervisor::spawn(&node, &entry, &data_dir).map_err(ProtocolError::new)?;

    let (client, mut events, broker_task) = broker::connect(settings.broker_port);
    client
        .subscribe(&format!("{DISCOVERY_PREFIX}/+/+/config"))
        .await
        .map_err(ProtocolError::new)?;
    client
        .subscribe(&format!("{DISCOVERY_PREFIX}/+/+/+/config"))
        .await
        .map_err(ProtocolError::new)?;
    client
        .subscribe(&bridge::info_topic(BASE_TOPIC))
        .await
        .map_err(ProtocolError::new)?;

    let mut registry = Registry::default();
    ctx.set_health(Health::Degraded("starting Zigbee2MQTT".to_owned()))
        .await;

    let outcome = loop {
        tokio::select! {
            incoming = ctx.next() => {
                match incoming {
                    Some(irori_protocol::Incoming::Call(incoming)) => {
                        route(incoming, &registry, &client).await;
                    }
                    Some(irori_protocol::Incoming::Action(incoming)) => {
                        handle_action(incoming, &client).await;
                    }
                    None => break Ok(()),
                }
            }
            event = events.recv() => {
                match event {
                    Some(BrokerEvent::Message(message)) => {
                        apply(message, &mut registry, &client, &ctx).await;
                    }
                    Some(BrokerEvent::Connectivity(Connectivity::Connected)) => {
                        // Not `Health::Running` yet: this only means our own client reached our
                        // own embedded broker, not that Zigbee2MQTT itself is up and talking to
                        // the radio. `apply` sets `Running` once Zigbee2MQTT's own `bridge/info`
                        // confirms that — the same signal `permit_join` availability waits for.
                    }
                    Some(BrokerEvent::Connectivity(Connectivity::Disconnected)) => {
                        ctx.set_health(Health::Degraded("disconnected from Zigbee2MQTT".to_owned())).await;
                        handle_disconnect(&mut registry, &ctx).await;
                    }
                    None => break Err(ProtocolError::new("stopped listening to the embedded broker")),
                }
            }
            status = spawned.child.wait() => {
                // Its own account of why, not just that it happened: an exit status alone is
                // not something a person can act on (ROADMAP D47), and this extension is the
                // only thing that ever sees Zigbee2MQTT's own error.
                let said = spawned.last_error();
                break Err(ProtocolError::new(match (status, said) {
                    (Ok(status), Some(said)) => format!("Zigbee2MQTT stopped: {said} ({status})"),
                    (Ok(status), None) => format!("Zigbee2MQTT exited: {status}"),
                    (Err(e), _) => format!("Zigbee2MQTT: {e}"),
                }));
            }
        }
    };

    drop(events);
    broker_task.abort();
    let _ = spawned.child.start_kill();
    // After, not before: on the crash path that just broke the loop, Zigbee2MQTT logged its own
    // reason moments before exiting, and only draining now gives the forwarding tasks a chance
    // to have read it before this process's own `main` can exit and take them down mid-read.
    spawned.drain_logs().await;
    outcome
}

/// Writes `contents` to `path` by writing a sibling temp file and renaming it over `path`, rather
/// than truncating `path` in place (`tokio::fs::write`'s own approach). Zigbee2MQTT persists the
/// network's generated identity in this same file (the `existing_advanced` note above) — a crash
/// or full disk mid-write of an in-place truncate could leave a half-written file with no usable
/// `advanced` block, and the next start would read nothing back and generate (and persist) a
/// brand new identity, dropping every paired device. A rename is atomic on the same filesystem,
/// which the temp file always is: it's written next to `path`, inside the same data directory.
async fn write_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name()
            .expect("configuration.yaml always has a file name")
            .to_string_lossy()
    ));
    tokio::fs::write(&tmp, contents).await?;
    tokio::fs::rename(&tmp, path).await
}

fn absolute(path: std::path::PathBuf) -> std::path::PathBuf {
    std::path::absolute(&path).unwrap_or(path)
}

/// Losing the broker connection means Zigbee2MQTT's own `bridge/info` is stale until it
/// reconnects and republishes it — `permit_join` shouldn't stay offered as if the bridge were
/// still confirmed up.
async fn handle_disconnect(registry: &mut Registry, ctx: &ProtocolContext) {
    if registry.bridge_seen {
        registry.bridge_seen = false;
        ctx.set_available_actions(vec![]).await;
    }
}

async fn handle_action(incoming: IncomingAction, client: &impl Publisher) {
    if incoming.action_id != PERMIT_JOIN {
        let action_id = incoming.action_id.clone();
        incoming.reply(Err(format!("no `{action_id}` action")));
        return;
    }
    let publish = bridge::permit_join(BASE_TOPIC, 60);
    let result = client.publish(&publish.topic, publish.payload, false).await;
    incoming.reply(result);
}

/// Applies one incoming broker message: Zigbee2MQTT's own bridge status, a discovery config, a
/// state report, or an availability change — whichever it matches, if any.
async fn apply(
    message: Message,
    registry: &mut Registry,
    client: &impl Publisher,
    ctx: &ProtocolContext,
) {
    if message.topic == bridge::info_topic(BASE_TOPIC) {
        if !registry.bridge_seen && bridge::is_bridge_info(&message.payload) {
            registry.bridge_seen = true;
            // This, not our own broker connecting, is what "ready to use" means: Zigbee2MQTT
            // itself confirmed up, not just our embedded broker having a socket open.
            ctx.set_health(Health::Running).await;
            ctx.set_available_actions(vec![PERMIT_JOIN.to_owned()])
                .await;
        }
        return;
    }
    if let Some(discovered) = topic::parse(&message.topic, DISCOVERY_PREFIX) {
        describe(discovered, &message, registry, client, ctx).await;
        return;
    }
    if let Some(listeners) = registry.availability_topics.get(&message.topic) {
        let text = String::from_utf8_lossy(&message.payload);
        // Two entities can share this topic and read its payloads differently, so each is
        // judged by its own words and the two answers are sent separately.
        let (available, unavailable) = discovery::resolve(listeners, text.trim());
        if !available.is_empty() {
            let _ = ctx
                .set_availability(
                    AvailabilityTarget::Entities(available),
                    Availability::Available,
                )
                .await;
        }
        if !unavailable.is_empty() {
            let _ = ctx
                .set_availability(
                    AvailabilityTarget::Entities(unavailable),
                    Availability::Unavailable,
                )
                .await;
        }
        return;
    }
    if let Some(unique_ids) = registry.state_topics.get(&message.topic).cloned() {
        for unique_id in unique_ids {
            let Some(entity) = registry.entities.get_mut(&unique_id) else {
                continue;
            };
            match state::decode(
                &entity.topics,
                &message.topic,
                &message.payload,
                entity.last_state.as_ref(),
            ) {
                Some(Ok(new_state)) => {
                    entity.last_state = Some(new_state.clone());
                    ctx.report_state(StateReport {
                        unique_id,
                        state: Some(new_state),
                        attributes: BTreeMap::default(),
                        caused_by: None,
                    });
                }
                Some(Err(why)) => {
                    tracing::debug!(%unique_id, %why, "couldn't read a state report");
                }
                None => {}
            }
        }
    }
}

async fn describe(
    discovered: topic::DiscoveryTopic,
    message: &Message,
    registry: &mut Registry,
    client: &impl Publisher,
    ctx: &ProtocolContext,
) {
    if message.payload.is_empty() {
        if let Some(unique_id) = registry.config_topics.remove(&message.topic) {
            if let Some(old) = registry.entities.remove(&unique_id) {
                deindex(&unique_id, &old.topics, registry);
            }
            if let Err(e) = ctx.remove_entity(unique_id.clone()).await {
                tracing::warn!(%unique_id, error = %e, "couldn't remove an entity");
            }
        }
        return;
    }
    let parsed = match discovery::parse(discovered.component, &message.payload) {
        Ok(parsed) => parsed,
        Err(why) => {
            tracing::warn!(topic = %message.topic, %why, "skipping a discovery config Irori can't use");
            return;
        }
    };

    if let Some(device) = map::device(&parsed)
        && let Err(e) = ctx.describe_device(device).await
    {
        tracing::warn!(error = %e, "the core refused a device");
        return;
    }
    let unique_id = parsed.unique_id.clone();
    let entity_description = map::entity(&parsed, &discovered.object_id);
    if let Err(e) = ctx.describe_entity(entity_description).await {
        tracing::warn!(%unique_id, error = %e, "the core refused an entity");
        return;
    }

    // A redescribe (the same entity's discovery config firing again, e.g. Zigbee2MQTT
    // republishing on its own restart) must drop this entity's old topic-index entries first —
    // otherwise a topic it no longer uses keeps reporting for it, and one it still uses ends up
    // listed twice.
    let last_state = if let Some(old) = registry.entities.remove(&unique_id) {
        deindex(&unique_id, &old.topics, registry);
        old.last_state
    } else {
        None
    };

    for (topic, _) in state::topics_of(&unique_id, &parsed.topics) {
        registry
            .state_topics
            .entry(topic.clone())
            .or_default()
            .push(unique_id.clone());
        let _ = client.subscribe(&topic).await;
    }
    for AvailabilityTopic {
        topic,
        payload_available,
        payload_not_available,
    } in parsed.availability
    {
        registry
            .availability_topics
            .entry(topic.clone())
            .or_default()
            .push(discovery::Listener {
                unique_id: unique_id.clone(),
                payload_available,
                payload_not_available,
            });
        let _ = client.subscribe(&topic).await;
    }
    registry
        .config_topics
        .insert(message.topic.clone(), unique_id.clone());
    registry.entities.insert(
        unique_id,
        Entity {
            topics: parsed.topics,
            last_state,
        },
    );
}

/// Drops `unique_id`'s entries from `state_topics` and `availability_topics` — shared by the
/// redescribe and removal paths in `describe`, so an entity that's redescribed or cleared always
/// loses its old topic-index entries. Left in place, a topic it no longer uses would keep
/// reporting for it, and one it still uses would end up listed (and so double-processed) twice.
fn deindex(unique_id: &UniqueId, old_topics: &EntityTopics, registry: &mut Registry) {
    for (topic, _) in state::topics_of(unique_id, old_topics) {
        if let Some(ids) = registry.state_topics.get_mut(&topic) {
            ids.retain(|id| id != unique_id);
            if ids.is_empty() {
                registry.state_topics.remove(&topic);
            }
        }
    }
    for listeners in registry.availability_topics.values_mut() {
        listeners.retain(|listener| &listener.unique_id != unique_id);
    }
    registry
        .availability_topics
        .retain(|_, listeners| !listeners.is_empty());
}

async fn route(incoming: IncomingCall, registry: &Registry, client: &impl Publisher) {
    let Some(entity) = registry.entities.get(&incoming.call.unique_id) else {
        let why = format!(
            "`{}` belongs to a device Irori isn't tracking right now",
            incoming.call.unique_id
        );
        incoming.reply(Err(ServiceError::unavailable(why)));
        return;
    };
    let messages = match state::encode(&entity.topics, &incoming.call.service) {
        Ok(messages) => messages,
        Err(why) => {
            incoming.reply(Err(ServiceError::failed(why)));
            return;
        }
    };
    for message in messages {
        if let Err(why) = client.publish(&message.topic, message.payload, false).await {
            incoming.reply(Err(ServiceError::unavailable(why)));
            return;
        }
    }
    incoming.reply(Ok(()));
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use irori_protocol::host;
    use irori_types::{LightTurnOn, Service, ServiceCall};

    use super::*;

    type Published = (String, Vec<u8>, bool);

    #[derive(Debug, Default, Clone)]
    struct FakePublisher {
        published: Arc<Mutex<Vec<Published>>>,
        subscribed: Arc<Mutex<Vec<String>>>,
    }

    impl Publisher for FakePublisher {
        async fn publish(&self, topic: &str, payload: Vec<u8>, retain: bool) -> Result<(), String> {
            self.published
                .lock()
                .expect("not poisoned")
                .push((topic.to_owned(), payload, retain));
            Ok(())
        }

        async fn subscribe(&self, topic: &str) -> Result<(), String> {
            self.subscribed
                .lock()
                .expect("not poisoned")
                .push(topic.to_owned());
            Ok(())
        }
    }

    fn message(topic: &str, payload: &[u8]) -> Message {
        Message {
            topic: topic.to_owned(),
            payload: payload.to_vec(),
        }
    }

    fn drain_ops(mut ops: tokio::sync::mpsc::Receiver<host::Op>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(op) = ops.recv().await {
                match op {
                    host::Op::DescribeDevice(_, reply)
                    | host::Op::DescribeEntity(_, reply)
                    | host::Op::RemoveDevice(_, reply)
                    | host::Op::RemoveEntity(_, reply)
                    | host::Op::SetAvailability(_, _, reply) => {
                        let _ = reply.send(Ok(()));
                    }
                    host::Op::SetHealth(_)
                    | host::Op::SetWaiting(_)
                    | host::Op::SetAvailableActions(_) => {}
                    host::Op::Load(_, reply) => {
                        let _ = reply.send(Ok(None));
                    }
                    host::Op::Store(_, _, reply) => {
                        let _ = reply.send(Ok(()));
                    }
                }
            }
        })
    }

    const Z2M_LIGHT: &[u8] = br#"{
        "unique_id": "0x0017880104e45520_light",
        "device": { "identifiers": ["0x0017880104e45520"], "name": "Living room lamp" },
        "schema": "json",
        "state_topic": "zigbee2mqtt/Living room lamp",
        "command_topic": "zigbee2mqtt/Living room lamp/set"
    }"#;

    #[tokio::test]
    async fn a_discovery_config_describes_a_device_and_entity() {
        let (ctx, host) = host::connect();
        let _drain = drain_ops(host.ops);
        let publisher = FakePublisher::default();
        let mut registry = Registry::default();

        let config_topic = "homeassistant/light/0x0017880104e45520_light/config";
        let discovered = topic::parse(config_topic, DISCOVERY_PREFIX).expect("a discovery topic");
        describe(
            discovered,
            &message(config_topic, Z2M_LIGHT),
            &mut registry,
            &publisher,
            &ctx,
        )
        .await;

        let unique_id = UniqueId::try_from("0x0017880104e45520_light").expect("valid");
        assert!(registry.entities.contains_key(&unique_id));
    }

    #[tokio::test]
    async fn redescribing_an_entity_doesnt_leave_duplicate_topic_index_entries() {
        let (ctx, host) = host::connect();
        let _drain = drain_ops(host.ops);
        let publisher = FakePublisher::default();
        let mut registry = Registry::default();
        let config_topic = "homeassistant/light/0x0017880104e45520_light/config";
        for _ in 0..2 {
            describe(
                topic::parse(config_topic, DISCOVERY_PREFIX).expect("valid"),
                &message(config_topic, Z2M_LIGHT),
                &mut registry,
                &publisher,
                &ctx,
            )
            .await;
        }

        assert_eq!(
            registry
                .state_topics
                .get("zigbee2mqtt/Living room lamp")
                .map(Vec::len),
            Some(1),
            "the same entity shouldn't be indexed twice under the same topic"
        );
    }

    #[tokio::test]
    async fn an_empty_payload_on_a_known_config_topic_removes_the_entity_and_its_topic_index() {
        let (ctx, host) = host::connect();
        let _drain = drain_ops(host.ops);
        let publisher = FakePublisher::default();
        let mut registry = Registry::default();
        let config_topic = "homeassistant/light/0x0017880104e45520_light/config";
        let discovered = topic::parse(config_topic, DISCOVERY_PREFIX).expect("a discovery topic");
        describe(
            discovered.clone(),
            &message(config_topic, Z2M_LIGHT),
            &mut registry,
            &publisher,
            &ctx,
        )
        .await;
        assert_eq!(registry.entities.len(), 1);

        describe(
            discovered,
            &message(config_topic, b""),
            &mut registry,
            &publisher,
            &ctx,
        )
        .await;

        assert!(
            registry.entities.is_empty(),
            "removed once its config goes empty"
        );
        assert!(
            registry.state_topics.is_empty(),
            "a removed entity's state topics shouldn't linger and double-report if reused: {:?}",
            registry.state_topics
        );
    }

    #[tokio::test]
    async fn a_brightness_only_report_keeps_the_previous_on_state() {
        let (ctx, host) = host::connect();
        let _drain = drain_ops(host.ops);
        let publisher = FakePublisher::default();
        let mut registry = Registry::default();
        let unique_id = UniqueId::try_from("some_dimmer").expect("valid");
        registry.entities.insert(
            unique_id.clone(),
            Entity {
                topics: EntityTopics::LightDefault {
                    state_topic: Some("t/POWER".to_owned()),
                    command_topic: "t/cmnd/POWER".to_owned(),
                    payload_on: "ON".to_owned(),
                    payload_off: "OFF".to_owned(),
                    brightness_state_topic: Some("t/RESULT".to_owned()),
                    brightness_command_topic: Some("t/cmnd/Dimmer".to_owned()),
                    brightness_scale: 100,
                },
                last_state: None,
            },
        );
        registry
            .state_topics
            .insert("t/POWER".to_owned(), vec![unique_id.clone()]);
        registry
            .state_topics
            .insert("t/RESULT".to_owned(), vec![unique_id.clone()]);

        apply(message("t/POWER", b"ON"), &mut registry, &publisher, &ctx).await;
        host.reports.ready().await;
        let _ = host.reports.drain();

        // A brightness-only report shouldn't force the light on/off — it keeps the entity's own
        // last-known `on` from the earlier report on the separate on/off topic.
        apply(message("t/RESULT", b"50"), &mut registry, &publisher, &ctx).await;

        host.reports.ready().await;
        let reports = host.reports.drain();
        assert_eq!(reports.len(), 1);
        let State::Light(second) = reports[0].state.clone().expect("a light state") else {
            panic!("expected a light state");
        };
        assert!(second.on);
        assert_eq!(second.brightness, Some(128)); // 50/100 * 255, rounded
    }

    #[tokio::test]
    async fn losing_the_broker_disconnects_the_bridge_and_clears_actions() {
        let (ctx, host) = host::connect();
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&seen);
        let mut ops = host.ops;
        let _drain = tokio::spawn(async move {
            while let Some(op) = ops.recv().await {
                if let host::Op::SetAvailableActions(actions) = op
                    && actions.is_empty()
                {
                    counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        });
        let mut registry = Registry {
            bridge_seen: true,
            ..Registry::default()
        };

        handle_disconnect(&mut registry, &ctx).await;

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!registry.bridge_seen);
        assert_eq!(seen.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn seeing_bridge_info_makes_permit_join_available() {
        let (ctx, host) = host::connect();
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&seen);
        let became_running = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted_running = Arc::clone(&became_running);
        let mut ops = host.ops;
        let _drain = tokio::spawn(async move {
            while let Some(op) = ops.recv().await {
                match op {
                    host::Op::SetAvailableActions(actions)
                        if actions == vec![PERMIT_JOIN.to_owned()] =>
                    {
                        counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    host::Op::SetHealth(Health::Running) => {
                        counted_running.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    _ => {}
                }
            }
        });
        let mut registry = Registry::default();

        apply(
            message(
                "zigbee2mqtt/bridge/info",
                br#"{"version": "2.1.0", "commit": "abc"}"#,
            ),
            &mut registry,
            &FakePublisher::default(),
            &ctx,
        )
        .await;

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(seen.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(
            became_running.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the bridge confirming itself up is what should report the extension as running, \
             not merely our own client reaching our own embedded broker"
        );
        assert!(registry.bridge_seen);
    }

    #[tokio::test]
    async fn write_atomically_replaces_the_files_contents_and_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("configuration.yaml");
        write_atomically(&path, "first\n").await.expect("writes");
        write_atomically(&path, "second\n").await.expect("writes");

        let contents = tokio::fs::read_to_string(&path).await.expect("readable");
        assert_eq!(contents, "second\n");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("readable")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(
            leftovers,
            vec![std::ffi::OsString::from("configuration.yaml")],
            "no .tmp file should be left behind: {leftovers:?}"
        );
    }

    #[tokio::test]
    async fn permit_join_publishes_to_the_bridge_request_topic() {
        let (incoming, answer) = host::incoming_action(PERMIT_JOIN.to_owned());
        let publisher = FakePublisher::default();

        handle_action(incoming, &publisher).await;

        let answered = answer.await.expect("answered rather than dropped");
        assert_eq!(answered, Ok(()));
        let published = publisher.published.lock().expect("not poisoned");
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].0, "zigbee2mqtt/bridge/request/permit_join");
        assert_eq!(published[0].1, br#"{"time":60,"value":true}"#);
    }

    #[tokio::test]
    async fn an_unknown_action_is_refused() {
        let (incoming, answer) = host::incoming_action("not_a_real_action".to_owned());
        let publisher = FakePublisher::default();

        handle_action(incoming, &publisher).await;

        let answered = answer.await.expect("answered rather than dropped");
        assert!(answered.is_err_and(|e| e.contains("not_a_real_action")));
        assert!(publisher.published.lock().expect("not poisoned").is_empty());
    }

    #[tokio::test]
    async fn a_service_call_publishes_the_right_json_schema_command() {
        let mut registry = Registry::default();
        registry.entities.insert(
            UniqueId::try_from("0x0017880104e45520_light").expect("valid"),
            Entity {
                topics: EntityTopics::LightJson {
                    state_topic: "zigbee2mqtt/Living room lamp".to_owned(),
                    command_topic: "zigbee2mqtt/Living room lamp/set".to_owned(),
                },
                last_state: None,
            },
        );
        let publisher = FakePublisher::default();
        let (incoming, answer) = host::incoming_call(ServiceCall {
            unique_id: UniqueId::try_from("0x0017880104e45520_light").expect("valid"),
            service: Service::LightTurnOn(LightTurnOn {
                brightness: Some(128),
                color_temp_kelvin: None,
                rgb: None,
            }),
            context: irori_types::Context {
                id: irori_types::ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9").expect("valid"),
                parent_id: None,
                origin: irori_types::Origin::System,
            },
        });

        route(incoming, &registry, &publisher).await;

        let answered = answer.await.expect("answered rather than dropped");
        assert_eq!(answered, Ok(()));
        let published = publisher.published.lock().expect("not poisoned");
        assert_eq!(published[0].0, "zigbee2mqtt/Living room lamp/set");
    }
}
