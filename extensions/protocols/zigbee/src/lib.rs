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
use irori_types::{Availability, StateReport, UniqueId};

use crate::broker::{BrokerEvent, Connectivity, Message, Publisher};
use crate::settings::Settings;

/// Zigbee2MQTT's own base topic — fixed, not a setting: there's no reason for a Zigbee2MQTT this
/// extension installs itself to use anything but its own default.
const BASE_TOPIC: &str = "zigbee2mqtt";
/// HA Discovery's own default prefix, same reasoning.
const DISCOVERY_PREFIX: &str = "homeassistant";
const DATA_DIR: &str = "z2m-data";
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
}

#[derive(Debug, Default)]
struct Registry {
    entities: BTreeMap<UniqueId, Entity>,
    config_topics: BTreeMap<String, UniqueId>,
    state_topics: BTreeMap<String, Vec<UniqueId>>,
    availability_topics: BTreeMap<String, (String, String, Vec<UniqueId>)>,
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

    let yaml = config::generate(&settings, settings.broker_port, BASE_TOPIC)
        .map_err(ProtocolError::new)?;
    tokio::fs::create_dir_all(DATA_DIR)
        .await
        .map_err(|e| ProtocolError::new(format!("couldn't create {DATA_DIR}: {e}")))?;
    tokio::fs::write(Path::new(DATA_DIR).join("configuration.yaml"), yaml)
        .await
        .map_err(|e| ProtocolError::new(format!("couldn't write configuration.yaml: {e}")))?;

    broker::start_embedded(settings.broker_port).map_err(ProtocolError::new)?;
    // Long enough for the embedded broker's own thread to actually be listening before anything
    // tries to connect to it — both Zigbee2MQTT and our own client below would just retry
    // otherwise, but there's no reason to make them.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let node = absolute(provision::node_binary());
    let entry = absolute(provision::zigbee2mqtt_entry());
    let data_dir = absolute(Path::new(DATA_DIR).to_path_buf());
    let mut child = supervisor::spawn(&node, &entry, &data_dir).map_err(ProtocolError::new)?;

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
                        ctx.set_health(Health::Running).await;
                    }
                    Some(BrokerEvent::Connectivity(Connectivity::Disconnected)) => {
                        ctx.set_health(Health::Degraded("disconnected from Zigbee2MQTT".to_owned())).await;
                    }
                    None => break Err(ProtocolError::new("stopped listening to the embedded broker")),
                }
            }
            status = child.wait() => {
                break Err(ProtocolError::new(match status {
                    Ok(status) => format!("Zigbee2MQTT exited: {status}"),
                    Err(e) => format!("Zigbee2MQTT: {e}"),
                }));
            }
        }
    };

    drop(events);
    broker_task.abort();
    let _ = child.start_kill();
    outcome
}

fn absolute(path: std::path::PathBuf) -> std::path::PathBuf {
    std::path::absolute(&path).unwrap_or(path)
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
            ctx.set_available_actions(vec![PERMIT_JOIN.to_owned()])
                .await;
        }
        return;
    }
    if let Some(discovered) = topic::parse(&message.topic, DISCOVERY_PREFIX) {
        describe(discovered, &message, registry, client, ctx).await;
        return;
    }
    if let Some((payload_available, payload_not_available, entities)) =
        registry.availability_topics.get(&message.topic)
    {
        let text = String::from_utf8_lossy(&message.payload);
        let text = text.trim();
        let availability = if text == payload_available {
            Some(Availability::Available)
        } else if text == payload_not_available {
            Some(Availability::Unavailable)
        } else {
            None
        };
        if let Some(availability) = availability {
            let _ = ctx
                .set_availability(AvailabilityTarget::Entities(entities.clone()), availability)
                .await;
        }
        return;
    }
    if let Some(unique_ids) = registry.state_topics.get(&message.topic).cloned() {
        for unique_id in unique_ids {
            let Some(entity) = registry.entities.get(&unique_id) else {
                continue;
            };
            match state::decode(&entity.topics, &message.topic, &message.payload) {
                Some(Ok(new_state)) => {
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
            registry.entities.remove(&unique_id);
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
            .or_insert_with(|| (payload_available, payload_not_available, Vec::new()))
            .2
            .push(unique_id.clone());
        let _ = client.subscribe(&topic).await;
    }
    registry
        .config_topics
        .insert(message.topic.clone(), unique_id.clone());
    registry.entities.insert(
        unique_id,
        Entity {
            topics: parsed.topics,
        },
    );
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
    async fn seeing_bridge_info_makes_permit_join_available() {
        let (ctx, host) = host::connect();
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&seen);
        let mut ops = host.ops;
        let _drain = tokio::spawn(async move {
            while let Some(op) = ops.recv().await {
                if let host::Op::SetAvailableActions(actions) = op
                    && actions == vec![PERMIT_JOIN.to_owned()]
                {
                    counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        assert!(registry.bridge_seen);
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
