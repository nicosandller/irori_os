//! The embedded broker this extension runs purely so its own Zigbee2MQTT has somewhere to
//! publish to — never reachable beyond loopback, never a general-purpose broker (that's what
//! the `mqtt` extension is for, against a real one). Once it's up, consuming HA discovery from
//! it is identical to `irori-protocol-mqtt`'s own `broker.rs`, so the client half mirrors that
//! one closely.

use std::collections::HashMap;
use std::time::Duration;

use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use rumqttd::{Broker as EmbeddedBroker, Config, ConnectionSettings, RouterConfig, ServerSettings};
use tokio::sync::mpsc;

/// Starts the embedded broker on its own OS thread (`rumqttd::Broker::start` is a blocking
/// call, not an async one). No return value to join on: when this whole process exits — the
/// normal way a protocol stops — the thread goes with it, the same as any other thread still
/// running when `main` returns.
pub fn start_embedded(port: u16) -> Result<(), String> {
    let listen: std::net::SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .map_err(|e| format!("bad broker port {port}: {e}"))?;
    // `rumqttd::Broker::start` binds its listener on a thread of its own and only logs a bind
    // failure from inside it — the caller who just spawned that thread has no way to know it
    // failed, and would carry on connecting to `port` regardless. If something unrelated is
    // already listening there, this instance's Zigbee2MQTT and our own client would silently
    // talk to that instead of to each other. Claiming the port here first, synchronously, turns
    // that into an immediate, reportable error; dropping the listener immediately after hands it
    // back for `rumqttd` to bind in turn.
    std::net::TcpListener::bind(listen)
        .map_err(|e| format!("port {port} is already in use: {e}"))?;
    let mut v4 = HashMap::new();
    v4.insert(
        "zigbee".to_owned(),
        ServerSettings {
            name: "zigbee".to_owned(),
            listen,
            tls: None,
            next_connection_delay_ms: 1,
            connections: ConnectionSettings {
                connection_timeout_ms: 60_000,
                max_payload_size: 20_480,
                max_inflight_count: 100,
                auth: None,
                external_auth: None,
                dynamic_filters: true,
            },
        },
    );
    let config = Config {
        id: 0,
        router: RouterConfig {
            max_connections: 100,
            max_outgoing_packet_count: 200,
            max_segment_size: 104_857_600,
            max_segment_count: 10,
            custom_segment: None,
            initialized_filters: None,
            shared_subscriptions_strategy: Default::default(),
        },
        v4: Some(v4),
        v5: None,
        ws: None,
        cluster: None,
        console: None,
        bridge: None,
        prometheus: None,
        metrics: None,
    };
    let mut broker = EmbeddedBroker::new(config);
    std::thread::Builder::new()
        .name("zigbee-broker".into())
        .spawn(move || {
            if let Err(error) = broker.start() {
                tracing::error!(%error, "the embedded broker stopped");
            }
        })
        .map_err(|e| format!("couldn't start the embedded broker's thread: {e}"))?;
    Ok(())
}

/// One publish arriving from the broker.
#[derive(Debug, Clone)]
pub struct Message {
    pub topic: String,
    pub payload: Vec<u8>,
}

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

/// What the run loop needs to talk back to the broker — real or, in tests, a fake that records
/// what it was asked to do.
pub trait Publisher: Send + Sync {
    fn publish(
        &self,
        topic: &str,
        payload: Vec<u8>,
        retain: bool,
    ) -> impl Future<Output = Result<(), String>> + Send;

    fn subscribe(&self, topic: &str) -> impl Future<Output = Result<(), String>> + Send;
}

/// This extension's own client connection to its own embedded broker.
#[derive(Debug, Clone)]
pub struct Client(AsyncClient);

impl Publisher for Client {
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

const EVENT_QUEUE: usize = 1024;

/// Connects to the embedded broker (which must already be listening — `start_embedded` first)
/// and starts polling in the background.
pub fn connect(
    port: u16,
) -> (
    Client,
    mpsc::Receiver<BrokerEvent>,
    tokio::task::JoinHandle<()>,
) {
    let mut options = MqttOptions::new("irori-zigbee", "127.0.0.1", port);
    options.set_keep_alive(Duration::from_secs(30));
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
                        return;
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
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "lost the connection to our own embedded broker; reconnecting");
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
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    });
    (Client(client), rx, handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starting_on_a_port_already_taken_is_a_named_error_not_a_silent_wrong_broker() {
        // Claims the port ourselves first, standing in for "something unrelated already has it".
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("binds");
        let port = listener.local_addr().expect("has an address").port();

        let error = start_embedded(port).expect_err("the port is taken");
        assert!(
            error.contains("already in use"),
            "should name the problem, not just fail generically: {error}"
        );

        drop(listener);
    }
}
