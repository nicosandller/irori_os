//! Connecting to the broker: a thin wrapper around `rumqttc`, translating its events into what
//! `lib.rs`'s run loop needs and nothing else. Real network I/O lives only here — the run loop
//! itself is tested without a broker, by feeding it [`Message`]s directly.

use std::time::Duration;

use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, Transport};
use tokio::sync::mpsc;

use crate::settings::Settings;

/// One publish arriving from the broker.
#[derive(Debug, Clone)]
pub struct Message {
    pub topic: String,
    pub payload: Vec<u8>,
}

/// What changed about the connection itself, not about any one topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connectivity {
    Connected,
    Disconnected,
}

#[derive(Debug, Clone)]
pub enum BrokerEvent {
    Message(Message),
    Connectivity(Connectivity),
}

/// What the run loop needs to talk back to a broker — real or, in tests, not. Kept to exactly
/// these two operations so `lib.rs`'s `describe`/`route`/`apply` can be tested by handing them a
/// fake that records what it was asked to do, never a real socket.
pub trait Publisher: Send + Sync {
    fn publish(
        &self,
        topic: &str,
        payload: Vec<u8>,
        retain: bool,
    ) -> impl Future<Output = Result<(), String>> + Send;

    fn subscribe(&self, topic: &str) -> impl Future<Output = Result<(), String>> + Send;
}

/// A handle to publish and subscribe. Cheap to clone (an `rumqttc::AsyncClient` is a channel
/// handle internally).
#[derive(Debug, Clone)]
pub struct Broker(AsyncClient);

impl Publisher for Broker {
    async fn publish(&self, topic: &str, payload: Vec<u8>, retain: bool) -> Result<(), String> {
        self.0
            .publish(topic, QoS::AtLeastOnce, retain, payload)
            .await
            .map_err(|e| e.to_string())
    }

    async fn subscribe(&self, topic: &str) -> Result<(), String> {
        self.0
            .subscribe(topic, QoS::AtLeastOnce)
            .await
            .map_err(|e| e.to_string())
    }
}

/// How many events can be waiting before the run loop falls behind. Generous: a fresh connection
/// to a busy broker can replay a lot of retained discovery configs at once.
const EVENT_QUEUE: usize = 1024;

/// Connects and starts polling in the background, returning a handle to publish/subscribe with
/// and the stream of what comes back. The task keeps running (and rumqttc keeps reconnecting on
/// its own — `docs.rs`'s own note: "continuing to poll will reconnect to the broker") until the
/// receiver is dropped.
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
    if settings.username.is_some() || settings.password.is_some() {
        let username = settings.username.as_ref().map_or("", |s| s.expose());
        let password = settings.password.as_ref().map_or("", |s| s.expose());
        options.set_credentials(username, password);
    }
    if settings.tls {
        options.set_transport(Transport::tls_with_default_config());
    }

    let (client, mut event_loop) = AsyncClient::new(options, EVENT_QUEUE);
    let (tx, rx) = mpsc::channel(EVENT_QUEUE);
    let handle = tokio::spawn(async move {
        let mut was_connected = false;
        loop {
            match event_loop.poll().await {
                Ok(Event::Incoming(Packet::Publish(publish))) => {
                    let message = BrokerEvent::Message(Message {
                        topic: publish.topic,
                        payload: publish.payload.to_vec(),
                    });
                    if tx.send(message).await.is_err() {
                        return; // the protocol stopped
                    }
                }
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    if !was_connected {
                        was_connected = true;
                        if tx
                            .send(BrokerEvent::Connectivity(Connectivity::Connected))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                Ok(_) => {} // acks and the like: nothing the run loop needs
                Err(error) => {
                    tracing::warn!(%error, "mqtt broker connection lost; reconnecting");
                    if was_connected {
                        was_connected = false;
                        if tx
                            .send(BrokerEvent::Connectivity(Connectivity::Disconnected))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    // rumqttc reconnects on the next poll; a brief pause avoids a hot loop
                    // against a broker that keeps refusing the same bad settings instantly.
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });
    (Broker(client), rx, handle)
}
