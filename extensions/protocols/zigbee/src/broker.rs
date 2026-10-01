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

/// Zigbee2MQTT 2.14 retains `bridge/info` (~48KB, its settings schema included)
/// and `bridge/definitions` (~228KB, the ZCL cluster list). A 20KB cap closed
/// the connection while Home Assistant discovery was still starting.
const MAX_MQTT_PACKET: usize = 1024 * 1024;

/// Starts the embedded broker on its own OS thread (`rumqttd::Broker::start` is a blocking
/// call, not an async one). No return value to join on: when this whole process exits — the
/// normal way a protocol stops — the thread goes with it, the same as any other thread still
/// running when `main` returns.
pub fn start_embedded(port: u16) -> Result<(), String> {
    // Zero means "any free port" to the operating system, and nothing here can find out which
    // one it picked: Zigbee2MQTT and this extension's own client would both go looking for port
    // 0 and neither would arrive. The schema refuses it too; this is the hand-edited file.
    if port == 0 {
        return Err(
            "the broker port can't be 0: it has to be a port both Zigbee2MQTT and \
                    Irori can connect to by number"
                .to_owned(),
        );
    }
    let listen: std::net::SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .map_err(|e| format!("bad broker port {port}: {e}"))?;
    // `rumqttd::Broker::start` binds its listener on a thread of its own and only logs a bind
    // failure from inside it — it neither accepts a socket we already hold nor returns that
    // failure to the caller. Claiming the port here first turns "something is already listening"
    // into an immediate error. The listener is then dropped so rumqttd can bind it. The gap
    // between that drop and rumqttd's own bind is real and unclosable from here.
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
                max_payload_size: MAX_MQTT_PACKET,
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
    /// Delivered because it was retained, on subscribing, rather than published just now: what
    /// was last said, not something happening (`StateReport::replayed`).
    pub retained: bool,
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
        // Queue only. `publish().await` waits until the event loop accepts the request, and that
        // loop is the same task that delivers incoming messages to `run`. Waiting here while `run`
        // is the one supposed to drain those messages stalls both, and permit-join never answers.
        self.0
            .try_publish(topic, QoS::AtLeastOnce, retain, payload)
            .map_err(|e| e.to_string())
    }

    async fn subscribe(&self, topic: &str) -> Result<(), String> {
        self.0
            .try_subscribe(topic, QoS::AtLeastOnce)
            .map_err(|e| e.to_string())
    }
}

/// Hands one event to the run loop. A full queue waits, which stops `poll` and lets the broker's
/// TCP window close. Dropping instead loses retained discovery, and nothing asks for it again.
/// Outbound `try_publish` does not wait on this task, so the wait cannot stall permit-join.
async fn forward(tx: &mpsc::Sender<BrokerEvent>, event: BrokerEvent) -> bool {
    tx.send(event).await.is_ok()
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
    // `bridge/info` is larger than rumqttc's 10KB default, and this client subscribes to it.
    options.set_max_packet_size(MAX_MQTT_PACKET, MAX_MQTT_PACKET);
    let (client, mut event_loop) = AsyncClient::new(options, EVENT_QUEUE);
    let (tx, rx) = mpsc::channel(EVENT_QUEUE);
    let handle = tokio::spawn(async move {
        let mut was_connected = false;
        loop {
            match event_loop.poll().await {
                Ok(Event::Incoming(Packet::Publish(publish))) => {
                    let message = BrokerEvent::Message(Message {
                        retained: publish.retain,
                        topic: publish.topic,
                        payload: publish.payload.to_vec(),
                    });
                    if !forward(&tx, message).await {
                        return;
                    }
                }
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    if !was_connected {
                        was_connected = true;
                        if !forward(&tx, BrokerEvent::Connectivity(Connectivity::Connected)).await {
                            return;
                        }
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "lost the connection to our own embedded broker; reconnecting");
                    if was_connected {
                        was_connected = false;
                        if !forward(&tx, BrokerEvent::Connectivity(Connectivity::Disconnected))
                            .await
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

    /// A message published while a client is subscribed reaches it with retain cleared; only one
    /// delivered because of a (new) subscription is marked retained. Events depend on it: a
    /// retained message is reported as a replay, which isn't a press (`StateReport::replayed`).
    #[tokio::test]
    async fn the_embedded_broker_marks_only_a_subscriptions_backlog_as_retained() {
        use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};

        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("a free port")
            .port();
        start_embedded(port).expect("starts");
        tokio::time::sleep(Duration::from_millis(300)).await;

        async fn next(rx: &mut tokio::sync::mpsc::UnboundedReceiver<bool>) -> bool {
            tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("a message in time")
                .expect("a message")
        }
        async fn client(
            id: &str,
            port: u16,
        ) -> (AsyncClient, tokio::sync::mpsc::UnboundedReceiver<bool>) {
            let (client, mut events) =
                AsyncClient::new(MqttOptions::new(id, "127.0.0.1", port), 10);
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(async move {
                while let Ok(event) = events.poll().await {
                    if let Event::Incoming(Packet::Publish(publish)) = event {
                        let _ = tx.send(publish.retain);
                    }
                }
            });
            (client, rx)
        }

        let (listening, mut heard) = client("listening", port).await;
        listening
            .subscribe("remote/action", QoS::AtLeastOnce)
            .await
            .expect("subscribe");
        tokio::time::sleep(Duration::from_millis(300)).await;
        let (publishing, _) = client("publishing", port).await;
        publishing
            .publish("remote/action", QoS::AtLeastOnce, true, "double")
            .await
            .expect("publish");
        assert!(
            !next(&mut heard).await,
            "a live message isn't marked retained"
        );

        let (late, mut backlog) = client("late", port).await;
        late.subscribe("remote/action", QoS::AtLeastOnce)
            .await
            .expect("subscribe");
        assert!(
            next(&mut backlog).await,
            "what a subscription brings is marked retained"
        );
    }

    #[tokio::test]
    async fn a_full_event_queue_waits_instead_of_dropping_the_publish() {
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(BrokerEvent::Connectivity(Connectivity::Connected))
            .expect("the only slot");
        let waiting = tokio::spawn(async move {
            forward(&tx, BrokerEvent::Connectivity(Connectivity::Disconnected)).await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !waiting.is_finished(),
            "a full queue must wait, not drop the event"
        );
        assert!(rx.recv().await.is_some());
        assert!(waiting.await.expect("forward task"));
    }
}
