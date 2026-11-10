//! Connecting to the broker: a thin wrapper around `rumqttc`, translating its events into what
//! `lib.rs`'s run loop needs and nothing else. Real network I/O lives only here — the run loop
//! itself is tested without a broker, by feeding it [`Message`]s directly.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rumqttc::{
    AsyncClient, ConnectionError, Event, MqttOptions, Packet, QoS, StateError, SubscribeFilter,
    Transport,
};
use tokio::sync::mpsc;

use crate::settings::Settings;

/// One publish arriving from the broker.
#[derive(Debug, Clone)]
pub struct Message {
    pub topic: String,
    pub payload: Vec<u8>,
    /// Delivered because it was retained, on subscribing, rather than published just now: what
    /// was last said, not something happening (`StateReport::replayed`).
    pub retained: bool,
}

/// What changed about the connection itself, not about any one topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connectivity {
    Connected,
    /// The connection dropped, or couldn't be made, and why, in words for the extension's card.
    Disconnected(String),
}

#[derive(Debug, Clone)]
pub enum BrokerEvent {
    Message(Message),
    Connectivity(Connectivity),
}

/// What the run loop needs to talk back to a broker — real or, in tests, not. Kept to exactly
/// these operations so `lib.rs`'s `describe`/`route`/`apply` can be tested by handing them a
/// fake that records what it was asked to do, never a real socket.
pub trait Publisher: Send + Sync {
    fn publish(
        &self,
        topic: &str,
        payload: Vec<u8>,
        retain: bool,
    ) -> impl Future<Output = Result<(), String>> + Send;

    /// Subscribes to `topic`. Nothing is sent when it's already subscribed: asking again would
    /// only have the broker replay its retained message.
    fn subscribe(&self, topic: &str) -> impl Future<Output = Result<(), String>> + Send;

    /// Stops listening to `topic`, which no entity uses any more.
    fn unsubscribe(&self, topic: &str) -> impl Future<Output = Result<(), String>> + Send;
}

/// A handle to publish and subscribe. Cheap to clone (an `rumqttc::AsyncClient` is a channel
/// handle internally).
#[derive(Debug, Clone)]
pub struct Broker {
    client: AsyncClient,
    /// What's subscribed to, so it can all be subscribed again after a reconnect.
    topics: Topics,
}

/// The topics subscribed to, shared between [`Broker`] and the task polling the connection.
#[derive(Debug, Clone, Default)]
struct Topics(Arc<Mutex<BTreeSet<String>>>);

impl Topics {
    /// Whether `topic` is new, and so has to be sent to the broker.
    fn add(&self, topic: &str) -> bool {
        self.0
            .lock()
            .expect("not poisoned")
            .insert(topic.to_owned())
    }

    /// Whether `topic` was subscribed, and so the broker has to be told to stop.
    fn remove(&self, topic: &str) -> bool {
        self.0.lock().expect("not poisoned").remove(topic)
    }

    /// Every topic, in SUBSCRIBE-sized batches.
    fn batches(&self) -> Vec<Vec<SubscribeFilter>> {
        batches(&self.0.lock().expect("not poisoned"))
    }
}

impl Publisher for Broker {
    async fn publish(&self, topic: &str, payload: Vec<u8>, retain: bool) -> Result<(), String> {
        self.client
            .publish(topic, QoS::AtLeastOnce, retain, payload)
            .await
            .map_err(|e| e.to_string())
    }

    async fn subscribe(&self, topic: &str) -> Result<(), String> {
        // Recorded before it's sent: a subscribe still waiting to go out when the connection
        // drops is thrown away by rumqttc, and only this list brings it back.
        if !self.topics.add(topic) {
            return Ok(());
        }
        self.client
            .subscribe(topic, QoS::AtLeastOnce)
            .await
            .map_err(|e| e.to_string())
    }

    async fn unsubscribe(&self, topic: &str) -> Result<(), String> {
        if !self.topics.remove(topic) {
            return Ok(());
        }
        self.client
            .unsubscribe(topic)
            .await
            .map_err(|e| e.to_string())
    }
}

/// How many events can be waiting before the run loop falls behind. Generous: a fresh connection
/// to a busy broker can replay a lot of retained discovery configs at once.
const EVENT_QUEUE: usize = 1024;

/// How long the connection has to stay up with no error before it counts as working again: the
/// reconnect pause goes back to its shortest, and the extension's card goes back to Running.
/// Resetting on every successful reconnect instead would undo both each time a message that's
/// too big drops the connection again a moment later.
pub const STEADY_AFTER: Duration = Duration::from_secs(30);

/// The first pause before reconnecting, doubled after each failure that came too soon.
const FIRST_RETRY: Duration = Duration::from_secs(1);

/// The longest pause before reconnecting.
const LONGEST_RETRY: Duration = Duration::from_secs(60);

/// How many topics go in one SUBSCRIBE when subscribing again after a reconnect. Small enough
/// that the packet stays well under even the smallest message size limit (10 KiB), which applies
/// to what's sent as well as to what's received.
const RESUBSCRIBE_BATCH: usize = 50;

/// Connects and starts polling in the background, returning a handle to publish/subscribe with
/// and the stream of what comes back. The task keeps running until the receiver is dropped.
///
/// rumqttc reconnects on the next poll after an error. Each new connection is a clean session,
/// so the broker has forgotten every subscription: they're all sent again on each reconnect.
pub fn connect(
    settings: &Settings,
) -> (
    Broker,
    mpsc::Receiver<BrokerEvent>,
    tokio::task::JoinHandle<()>,
) {
    let client_id = settings
        .client_id
        .clone()
        .unwrap_or_else(|| format!("irori-{}", std::process::id()));
    let mut options = MqttOptions::new(client_id, settings.host.clone(), settings.port);
    options.set_keep_alive(Duration::from_secs(30));
    let max_message = settings.max_message_bytes();
    options.set_max_packet_size(max_message, max_message);
    if settings.username.is_some() || settings.password.is_some() {
        let username = settings.username.as_ref().map_or("", |s| s.expose());
        let password = settings.password.as_ref().map_or("", |s| s.expose());
        options.set_credentials(username, password);
    }
    if settings.tls {
        options.set_transport(Transport::tls_with_default_config());
    }

    let (client, mut event_loop) = AsyncClient::new(options, EVENT_QUEUE);
    let topics = Topics::default();
    let broker = Broker {
        client: client.clone(),
        topics: topics.clone(),
    };
    let (tx, rx) = mpsc::channel(EVENT_QUEUE);
    let handle = tokio::spawn(async move {
        let mut connected_at: Option<Instant> = None;
        let mut session = Session::default();
        let mut retry: Option<Duration> = None;
        loop {
            match event_loop.poll().await {
                Ok(Event::Incoming(Packet::Publish(publish))) => {
                    let message = BrokerEvent::Message(Message {
                        retained: publish.retain,
                        topic: publish.topic,
                        payload: publish.payload.to_vec(),
                    });
                    if tx.send(message).await.is_err() {
                        return; // the protocol stopped
                    }
                }
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    connected_at = Some(Instant::now());
                    if session.connected() {
                        resubscribe(&client, &topics);
                    }
                    if tx
                        .send(BrokerEvent::Connectivity(Connectivity::Connected))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(_) => {} // acks and the like: nothing the run loop needs
                Err(error) => {
                    let steady = held_steady(connected_at.take(), Instant::now());
                    let pause = next_retry(retry, steady);
                    retry = Some(pause);
                    let reason = match oversized(&error) {
                        Some(size) => {
                            let reason = too_big(size, max_message);
                            tracing::warn!(
                                size,
                                limit = max_message,
                                retry_in = ?pause,
                                "{reason}; reconnecting",
                            );
                            reason
                        }
                        None => {
                            tracing::warn!(
                                %error,
                                retry_in = ?pause,
                                "mqtt broker connection lost; reconnecting",
                            );
                            format!(
                                "the broker connection dropped or failed ({error}); waiting for \
                                 it to hold"
                            )
                        }
                    };
                    if tx
                        .send(BrokerEvent::Connectivity(Connectivity::Disconnected(
                            reason,
                        )))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    tokio::time::sleep(pause).await;
                }
            }
        }
    });
    (broker, rx, handle)
}

/// Whether a new connection has to subscribe to everything again.
#[derive(Debug, Default)]
struct Session {
    connected_before: bool,
}

impl Session {
    /// Called on each connection: true from the second on. Each one is a clean session, so the
    /// broker has forgotten what the last one subscribed to. The first connection's subscribes
    /// are still queued, and sending them twice would have the broker replay every retained
    /// message twice.
    fn connected(&mut self) -> bool {
        std::mem::replace(&mut self.connected_before, true)
    }
}

/// Subscribes to every known topic again, in the background: the request queue is bounded and
/// only the polling task empties it, so that task can't wait on it itself.
fn resubscribe(client: &AsyncClient, topics: &Topics) {
    let batches = topics.batches();
    let client = client.clone();
    tokio::spawn(async move {
        for batch in batches {
            if let Err(error) = client.subscribe_many(batch).await {
                tracing::warn!(%error, "couldn't subscribe again after reconnecting");
            }
        }
    });
}

/// `topics` split into SUBSCRIBE-sized batches. None when there are no topics: an empty
/// SUBSCRIBE is refused.
fn batches(topics: &BTreeSet<String>) -> Vec<Vec<SubscribeFilter>> {
    let topics: Vec<&String> = topics.iter().collect();
    topics
        .chunks(RESUBSCRIBE_BATCH)
        .map(|batch| {
            batch
                .iter()
                .map(|topic| SubscribeFilter::new((*topic).clone(), QoS::AtLeastOnce))
                .collect()
        })
        .collect()
}

/// Whether a connection made at `connected_at` (`None`: it never got that far) had held for
/// [`STEADY_AFTER`] by `now`, when it failed.
fn held_steady(connected_at: Option<Instant>, now: Instant) -> bool {
    connected_at.is_some_and(|since| now.duration_since(since) >= STEADY_AFTER)
}

/// The pause before the next reconnect: the shortest after the first failure or after a
/// connection that held for [`STEADY_AFTER`], otherwise twice the last, up to [`LONGEST_RETRY`].
fn next_retry(last: Option<Duration>, steady: bool) -> Duration {
    match last {
        Some(last) if !steady => (last * 2).min(LONGEST_RETRY),
        _ => FIRST_RETRY,
    }
}

/// The size of the message, in bytes, when `error` is rumqttc refusing one over the size limit.
/// It can't skip such a message: it always drops the connection.
fn oversized(error: &ConnectionError) -> Option<usize> {
    match error {
        ConnectionError::MqttState(StateError::Deserialization(
            rumqttc::mqttbytes::Error::PayloadSizeLimitExceeded(size),
        )) => Some(*size),
        _ => None,
    }
}

/// What the extension's card says when a message is over the limit.
fn too_big(size: usize, limit: usize) -> String {
    format!(
        "a message of {} is over the {} limit (max_message_kib), so the broker connection \
         drops each time it's sent",
        human_size(size),
        human_size(limit),
    )
}

/// A size in KB or MB (of 1024), rounded up, so a message just over the limit never reads as
/// the same size as the limit.
fn human_size(bytes: usize) -> String {
    const KB: usize = 1024;
    const MB: usize = 1024 * 1024;
    if bytes >= MB {
        let tenths = bytes.saturating_mul(10).div_ceil(MB);
        if tenths.is_multiple_of(10) {
            format!("{} MB", tenths / 10)
        } else {
            format!("{}.{} MB", tenths / 10, tenths % 10)
        }
    } else {
        format!("{} KB", bytes.div_ceil(KB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_over_the_size_limit_is_recognised() {
        let error = ConnectionError::MqttState(StateError::Deserialization(
            rumqttc::mqttbytes::Error::PayloadSizeLimitExceeded(51_233),
        ));
        assert_eq!(oversized(&error), Some(51_233));
        assert_eq!(
            too_big(51_233, 10 * 1024),
            "a message of 51 KB is over the 10 KB limit (max_message_kib), so the broker \
             connection drops each time it's sent"
        );

        let other = ConnectionError::MqttState(StateError::Deserialization(
            rumqttc::mqttbytes::Error::MalformedPacket,
        ));
        assert_eq!(oversized(&other), None);
        assert_eq!(oversized(&ConnectionError::NetworkTimeout), None);
        assert_eq!(
            oversized(&ConnectionError::MqttState(StateError::ConnectionAborted)),
            None
        );
    }

    #[test]
    fn sizes_round_up() {
        assert_eq!(human_size(10 * 1024), "10 KB");
        assert_eq!(human_size(10 * 1024 + 1), "11 KB");
        assert_eq!(human_size(1024 * 1024), "1 MB");
        assert_eq!(human_size(1024 * 1024 + 1), "1.1 MB");
        assert_eq!(human_size(2_400_000), "2.3 MB");
    }

    #[test]
    fn reconnect_pauses_double_up_to_a_minute() {
        let mut last = None;
        let mut pauses = Vec::new();
        for _ in 0..8 {
            let pause = next_retry(last, false);
            pauses.push(pause.as_secs());
            last = Some(pause);
        }
        assert_eq!(pauses, [1, 2, 4, 8, 16, 32, 60, 60]);
    }

    #[test]
    fn a_connection_that_drops_again_within_30_seconds_keeps_the_longer_pause() {
        let connected = Instant::now();
        let pause_after = |failed_at: Duration| {
            let steady = held_steady(Some(connected), connected + failed_at);
            next_retry(Some(Duration::from_secs(8)), steady)
        };
        // Reconnected, then dropped again before STEADY_AFTER: the pause keeps growing.
        assert_eq!(pause_after(Duration::from_secs(10)).as_secs(), 16);
        assert_eq!(pause_after(Duration::from_secs(29)).as_secs(), 16);
        // Held for STEADY_AFTER first: back to the shortest.
        assert_eq!(pause_after(STEADY_AFTER), FIRST_RETRY);
        // Never connected at all: not steady.
        assert!(!held_steady(None, connected + STEADY_AFTER));
        assert_eq!(next_retry(Some(LONGEST_RETRY), false), LONGEST_RETRY);
    }

    #[test]
    fn every_connection_after_the_first_subscribes_again() {
        let mut session = Session::default();
        assert!(!session.connected());
        assert!(session.connected());
        assert!(session.connected());
    }

    #[test]
    fn a_topic_is_only_sent_when_new_or_still_subscribed() {
        let topics = Topics::default();
        assert!(topics.add("zigbee2mqtt/lamp"));
        assert!(!topics.add("zigbee2mqtt/lamp"));
        assert!(topics.remove("zigbee2mqtt/lamp"));
        assert!(!topics.remove("zigbee2mqtt/lamp"));
        assert!(topics.batches().is_empty());
        topics.add("zigbee2mqtt/lamp");
        assert_eq!(topics.batches()[0][0].path, "zigbee2mqtt/lamp");
    }

    #[test]
    fn topics_are_subscribed_again_in_batches() {
        assert!(batches(&BTreeSet::new()).is_empty());
        let topics: BTreeSet<String> = (0..120).map(|i| format!("topic/{i}")).collect();
        let sizes: Vec<usize> = batches(&topics).iter().map(Vec::len).collect();
        assert_eq!(sizes, [50, 50, 20]);
    }
}
