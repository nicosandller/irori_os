//! MQTT protocol with Home Assistant MQTT Discovery.
//!
//! Speaks to one external broker (`settings`), subscribing to `<discovery_prefix>/+/+/config`
//! and `<discovery_prefix>/+/+/+/config` and turning what it hears into devices and entities —
//! the same discovery convention Zigbee2MQTT, Tasmota, and ESPHome-over-MQTT already publish.
//! The parsing and translation logic itself lives in `irori-ha-discovery`, shared with the
//! `zigbee` protocol, which uses it against its own embedded broker.

mod broker;
pub mod settings;

use std::collections::BTreeMap;

use irori_ha_discovery::discovery::{AvailabilityTopic, EntityTopics};
use irori_ha_discovery::{discovery, map, state, topic};
use irori_protocol::{
    AvailabilityTarget, Health, IncomingCall, Protocol, ProtocolContext, ProtocolError,
    ServiceError,
};
use irori_types::{Availability, State, StateReport, UniqueId};

use crate::broker::{BrokerEvent, Connectivity, Message, Publisher};
use crate::settings::Settings;

/// The MQTT protocol.
#[derive(Debug)]
pub struct Mqtt;

impl Protocol for Mqtt {
    type Config = Settings;
    const MANIFEST: &'static str = include_str!("../irori-extension.toml");

    async fn run(settings: Settings, ctx: ProtocolContext) -> Result<(), ProtocolError> {
        run(settings, ctx).await
    }
}

/// What's tracked about one entity Irori knows about: enough to decode its state and encode a
/// service call to it.
#[derive(Debug, Clone)]
struct Entity {
    topics: EntityTopics,
    /// The last state successfully decoded for this entity, if any — the default light schema
    /// splits on/off and brightness across two topics, so decoding one needs what the other last
    /// reported (`irori_ha_discovery::state::decode`'s own `previous` parameter).
    last_state: Option<State>,
}

#[derive(Debug, Default)]
struct Registry {
    entities: BTreeMap<UniqueId, Entity>,
    /// Which entity a discovery config topic described, so a later empty (retained-removed)
    /// payload on the same topic knows what to remove.
    config_topics: BTreeMap<String, UniqueId>,
    /// A state or brightness-state topic -> the entities it might be reporting for. Almost
    /// always one, but nothing stops two entities from sharing a topic.
    state_topics: BTreeMap<String, Vec<UniqueId>>,
    /// An availability topic -> its payload strings and the entities it speaks for.
    availability_topics: BTreeMap<String, (String, String, Vec<UniqueId>)>,
}

async fn run(settings: Settings, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
    let (broker, mut events, broker_task) = broker::connect(&settings);
    broker
        .subscribe(&format!("{}/+/+/config", settings.discovery_prefix))
        .await
        .map_err(ProtocolError::new)?;
    broker
        .subscribe(&format!("{}/+/+/+/config", settings.discovery_prefix))
        .await
        .map_err(ProtocolError::new)?;

    let mut registry = Registry::default();
    ctx.set_health(Health::Degraded("connecting to the broker".to_owned()))
        .await;

    let outcome = loop {
        tokio::select! {
            call = ctx.next_call() => {
                let Some(incoming) = call else { break Ok(()) };
                route(incoming, &registry, &broker).await;
            }
            event = events.recv() => {
                match event {
                    Some(BrokerEvent::Message(message)) => {
                        apply(message, &settings, &mut registry, &broker, &ctx).await;
                    }
                    Some(BrokerEvent::Connectivity(Connectivity::Connected)) => {
                        ctx.set_health(Health::Running).await;
                    }
                    Some(BrokerEvent::Connectivity(Connectivity::Disconnected)) => {
                        ctx.set_health(Health::Degraded("disconnected from the broker".to_owned())).await;
                    }
                    None => break Err(ProtocolError::new("stopped listening to the broker")),
                }
            }
        }
    };

    drop(events);
    broker_task.abort();
    outcome
}

/// Applies one incoming broker message: a discovery config, a state report, an availability
/// change, or (harmlessly ignored here) Z2M's own bridge status — whichever it matches, if any.
async fn apply(
    message: Message,
    settings: &Settings,
    registry: &mut Registry,
    broker: &impl Publisher,
    ctx: &ProtocolContext,
) {
    if let Some(discovered) = topic::parse(&message.topic, &settings.discovery_prefix) {
        describe(discovered, &message, registry, broker, ctx).await;
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

/// A discovery config topic fired: parse it, describe the device/entity, and (re)index its
/// topics so future messages on them are recognised. An empty (retained-removed) payload means
/// the entity is gone instead.
async fn describe(
    discovered: topic::DiscoveryTopic,
    message: &Message,
    registry: &mut Registry,
    broker: &impl Publisher,
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

    // A redescribe (the same entity's discovery config firing again, e.g. Z2M republishing on
    // its own restart) must drop this entity's old topic-index entries first — otherwise a topic
    // it no longer uses keeps reporting for it, and one it still uses ends up listed twice.
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
        let _ = broker.subscribe(&topic).await;
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
        let _ = broker.subscribe(&topic).await;
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
    for (_, _, ids) in registry.availability_topics.values_mut() {
        ids.retain(|id| id != unique_id);
    }
    registry
        .availability_topics
        .retain(|_, (_, _, ids)| !ids.is_empty());
}

/// Sends a service call to the broker for the entity it belongs to.
async fn route(incoming: IncomingCall, registry: &Registry, broker: &impl Publisher) {
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
        if let Err(why) = broker.publish(&message.topic, message.payload, false).await {
            incoming.reply(Err(ServiceError::unavailable(why)));
            return;
        }
    }
    incoming.reply(Ok(()));
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use irori_ha_discovery::topic::Component;
    use irori_protocol::host;
    use irori_types::{LightTurnOn, Service, ServiceCall};
    use tokio::sync::mpsc;

    use super::*;

    /// One call to [`FakePublisher::publish`]: the topic, payload, and retain flag it was given.
    type Published = (String, Vec<u8>, bool);

    /// Records what it was asked to publish/subscribe, instead of touching a real broker.
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

    fn settings() -> Settings {
        serde_json::from_value(serde_json::json!({ "host": "test-broker" })).expect("valid")
    }

    fn message(topic: &str, payload: &[u8]) -> Message {
        Message {
            topic: topic.to_owned(),
            payload: payload.to_vec(),
        }
    }

    /// Answers every op the tests below produce, so `describe`/`apply`/`route` never block
    /// waiting for a core that isn't there.
    fn drain_ops(mut ops: mpsc::Receiver<host::Op>) -> tokio::task::JoinHandle<()> {
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
        "command_topic": "zigbee2mqtt/Living room lamp/set",
        "availability_topic": "zigbee2mqtt/bridge/state"
    }"#;

    #[tokio::test]
    async fn a_discovery_config_describes_a_device_and_entity_and_subscribes_its_topics() {
        let (ctx, host) = host::connect();
        let _drain = drain_ops(host.ops);
        let publisher = FakePublisher::default();
        let mut registry = Registry::default();

        let topic = "homeassistant/light/0x0017880104e45520_light/config";
        let discovered = topic::parse(topic, "homeassistant").expect("a discovery topic");
        assert_eq!(discovered.component, Component::Light);
        describe(
            discovered,
            &message(topic, Z2M_LIGHT),
            &mut registry,
            &publisher,
            &ctx,
        )
        .await;

        let unique_id = UniqueId::try_from("0x0017880104e45520_light").expect("valid");
        assert!(registry.entities.contains_key(&unique_id));
        assert!(
            registry
                .state_topics
                .get("zigbee2mqtt/Living room lamp")
                .is_some_and(|ids| ids.contains(&unique_id))
        );
        assert!(
            publisher
                .subscribed
                .lock()
                .expect("not poisoned")
                .iter()
                .any(|t| t == "zigbee2mqtt/Living room lamp")
        );
    }

    #[tokio::test]
    async fn an_empty_payload_on_a_known_config_topic_removes_the_entity() {
        let (ctx, host) = host::connect();
        let _drain = drain_ops(host.ops);
        let publisher = FakePublisher::default();
        let mut registry = Registry::default();
        let topic = "homeassistant/light/0x0017880104e45520_light/config";
        let discovered = topic::parse(topic, "homeassistant").expect("a discovery topic");
        describe(
            discovered.clone(),
            &message(topic, Z2M_LIGHT),
            &mut registry,
            &publisher,
            &ctx,
        )
        .await;
        assert_eq!(registry.entities.len(), 1);

        describe(
            discovered,
            &message(topic, b""),
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
        assert!(
            registry.availability_topics.is_empty(),
            "a removed entity's availability topics shouldn't linger either: {:?}",
            registry.availability_topics
        );
    }

    #[tokio::test]
    async fn a_state_message_reports_to_the_core() {
        let (ctx, host) = host::connect();
        let _drain = drain_ops(host.ops);
        let publisher = FakePublisher::default();
        let mut registry = Registry::default();
        let config_topic = "homeassistant/light/0x0017880104e45520_light/config";
        describe(
            topic::parse(config_topic, "homeassistant").expect("valid"),
            &message(config_topic, Z2M_LIGHT),
            &mut registry,
            &publisher,
            &ctx,
        )
        .await;

        apply(
            message(
                "zigbee2mqtt/Living room lamp",
                br#"{"state":"ON","brightness":200}"#,
            ),
            &settings(),
            &mut registry,
            &publisher,
            &ctx,
        )
        .await;

        host.reports.ready().await;
        let reports = host.reports.drain();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].unique_id.as_str(), "0x0017880104e45520_light");
    }

    #[tokio::test]
    async fn a_brightness_only_report_keeps_the_previous_on_state() {
        let (ctx, host) = host::connect();
        let _drain = drain_ops(host.ops);
        let publisher = FakePublisher::default();
        let mut registry = Registry::default();
        let unique_id = UniqueId::try_from("tasmota_light").expect("valid");
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

        apply(
            message("t/POWER", b"ON"),
            &settings(),
            &mut registry,
            &publisher,
            &ctx,
        )
        .await;
        host.reports.ready().await;
        let _ = host.reports.drain();

        // A brightness-only report shouldn't force the light on/off — it keeps the entity's own
        // last-known `on` from the earlier report on the separate on/off topic.
        apply(
            message("t/RESULT", b"50"),
            &settings(),
            &mut registry,
            &publisher,
            &ctx,
        )
        .await;

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
    async fn redescribing_an_entity_doesnt_leave_duplicate_topic_index_entries() {
        let (ctx, host) = host::connect();
        let _drain = drain_ops(host.ops);
        let publisher = FakePublisher::default();
        let mut registry = Registry::default();
        let config_topic = "homeassistant/light/0x0017880104e45520_light/config";
        for _ in 0..2 {
            describe(
                topic::parse(config_topic, "homeassistant").expect("valid"),
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
        assert_eq!(
            registry
                .availability_topics
                .get("zigbee2mqtt/bridge/state")
                .map(|(_, _, ids)| ids.len()),
            Some(1)
        );
    }

    #[tokio::test]
    async fn an_availability_message_marks_the_entity_unavailable() {
        let (ctx, host) = host::connect();
        let described = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&described);
        let mut ops = host.ops;
        let _drain = tokio::spawn(async move {
            while let Some(op) = ops.recv().await {
                match op {
                    host::Op::SetAvailability(_, availability, reply) => {
                        if availability == Availability::Unavailable {
                            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        let _ = reply.send(Ok(()));
                    }
                    host::Op::DescribeDevice(_, reply)
                    | host::Op::DescribeEntity(_, reply)
                    | host::Op::RemoveDevice(_, reply)
                    | host::Op::RemoveEntity(_, reply) => {
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
        });
        let publisher = FakePublisher::default();
        let mut registry = Registry::default();
        let config_topic = "homeassistant/light/0x0017880104e45520_light/config";
        describe(
            topic::parse(config_topic, "homeassistant").expect("valid"),
            &message(config_topic, Z2M_LIGHT),
            &mut registry,
            &publisher,
            &ctx,
        )
        .await;

        apply(
            message("zigbee2mqtt/bridge/state", b"offline"),
            &settings(),
            &mut registry,
            &publisher,
            &ctx,
        )
        .await;

        // Give the drain task a turn; it's driven by the same channel `apply` just sent on.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(described.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn a_service_call_publishes_the_right_json_schema_command() {
        let registry = {
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
            registry
        };
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
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].0, "zigbee2mqtt/Living room lamp/set");
        assert_eq!(published[0].1, br#"{"brightness":128,"state":"ON"}"#);
    }

    #[tokio::test]
    async fn a_service_call_for_an_unknown_entity_is_unavailable() {
        let registry = Registry::default();
        let publisher = FakePublisher::default();
        let (incoming, answer) = host::incoming_call(ServiceCall {
            unique_id: UniqueId::try_from("nobody_home").expect("valid"),
            service: Service::SwitchTurnOn,
            context: irori_types::Context {
                id: irori_types::ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9").expect("valid"),
                parent_id: None,
                origin: irori_types::Origin::System,
            },
        });

        route(incoming, &registry, &publisher).await;

        let answered = answer.await.expect("answered rather than dropped");
        assert!(
            matches!(answered, Err(ref e) if e.code == irori_protocol::ServiceErrorCode::Unavailable),
            "got {answered:?}"
        );
    }
}
