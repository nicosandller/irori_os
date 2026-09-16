//! One task per ESPHome device: connect, learn what it has, stream its state, carry out
//! commands, and reconnect when it drops.
//!
//! The task never touches the core directly. It sends [`Event`]s to the integration's run loop,
//! which owns the [`IntegrationContext`], because that context can't be shared (it holds the
//! receiving end of the call queue).

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::time::Duration;

use esphome_client::EspHomeClient;
use esphome_client::types::{
    EspHomeMessage, LightCommandRequest, ListEntitiesRequest, PingResponse, SubscribeStatesRequest,
    SwitchCommandRequest,
};
use irori_integration::types::{
    Capabilities, ContextId, DeviceDescription, EntityDescription, LightCapabilities, Service,
    StateReport, UniqueId,
};
use irori_integration::{IncomingCall, ServiceError};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::map;

/// Which connection something came from. Addresses are reused — by the same device after a
/// reconnect, by a different one after DHCP hands it on — so an address can't tell two
/// connections apart. This can: it's handed out once per task and never repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Connection {
    pub id: u64,
    pub address: SocketAddr,
}

impl std::fmt::Display for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.address)
    }
}

/// What a device task tells the run loop.
#[derive(Debug)]
pub enum Event {
    /// Connected, and this is everything it has. Sent again after every reconnect, because the
    /// device may have been reflashed with different entities while it was away.
    Arrived {
        /// Which connection this is, so the run loop can send it commands — and so anything
        /// from a connection it has already replaced can be recognised and ignored.
        connection: Connection,
        device: Box<DeviceDescription>,
        entities: Vec<EntityDescription>,
    },
    /// A new value for one of the device's entities. Carries the connection that heard it, so
    /// a report queued by a connection that has since been replaced can't overwrite the state
    /// the current one is reporting.
    Reported {
        connection: Connection,
        device: UniqueId,
        report: Box<StateReport>,
    },
    /// Lost: the entities stay in the registry, marked unavailable, until it comes back. The
    /// address says which connection this is, so a goodbye from one that has already been
    /// replaced can be told apart from the real thing.
    Left {
        connection: Connection,
        device: UniqueId,
        why: String,
    },
    /// Couldn't connect at all, and this device was never introduced. Logged, not registered:
    /// there's nothing to show yet.
    Unreachable { connection: Connection, why: String },
}

/// How long to wait before trying a device again, doubling up to [`MAX_RETRY`]. A device that's
/// simply switched off shouldn't be hammered.
const FIRST_RETRY: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(60);

/// How long the whole opening exchange may take: connect, say hello, ask what the device is and
/// what it has, and subscribe. A host that accepts the connection and then says nothing must not
/// hold a task for ever — it has to reach the reconnect path like any other failure.
const SETUP_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a connection may be silent before Irori checks it's still there, and how long the
/// answer may take. A device with nothing to report is normal; one that has stopped answering
/// looks exactly the same from here until asked.
const PING_AFTER: Duration = Duration::from_secs(30);

/// How long a command's context stays attached to what the device reports next. ESPHome answers
/// a command with a state message rather than an acknowledgement, so the next report for that
/// entity is the result; after this long it's a change that happened on its own.
const CAUSED_BY_WINDOW: Duration = Duration::from_secs(10);

/// Runs one device until the integration stops (its command channel closes).
pub async fn run(
    connection: Connection,
    events: mpsc::Sender<Event>,
    mut calls: mpsc::Receiver<IncomingCall>,
) {
    let address = connection.address;
    let mut retry = FIRST_RETRY;
    // Who this address turned out to be. Kept once learned, across every reconnect: a device
    // that has introduced itself and then gone quiet is a device the home knows about, not an
    // address that never answered, and saying otherwise would count it as two kinds of trouble
    // at once.
    let mut known: Option<UniqueId> = None;
    // The trust warning is about this connection, not this attempt; once is enough.
    let mut warned = false;
    loop {
        let mut connected = false;
        let ended = session(
            connection,
            &events,
            &mut calls,
            &mut known,
            &mut warned,
            &mut connected,
        )
        .await;
        match ended {
            // The run loop dropped our command channel: the integration is stopping.
            Ok(Ended::Stopping) => break,
            Ok(Ended::Disconnected(why)) | Err(why) => {
                let event = match known.clone() {
                    Some(device) => Event::Left {
                        connection,
                        device,
                        why,
                    },
                    None => Event::Unreachable { connection, why },
                };
                if events.send(event).await.is_err() {
                    break;
                }
            }
        }
        // A device that worked and then dropped gets the short wait again; only repeated
        // failures back off.
        if connected {
            retry = FIRST_RETRY;
        }
        // Wait to retry. Commands that arrive meanwhile are refused rather than dropped:
        // dropping one reaches the caller as "the integration dropped the call" instead of the
        // plain truth, which is that the device isn't there.
        let until = Instant::now() + retry;
        loop {
            tokio::select! {
                () = tokio::time::sleep_until(until) => break,
                call = calls.recv() => match call {
                    // The integration is stopping, or this connection has been replaced.
                    None => return refuse_pending(&mut calls, address).await,
                    Some(incoming) => {
                        let why = format!("{address} isn't connected right now");
                        incoming.reply(Err(ServiceError::unavailable(why)));
                    }
                },
            }
        }
        retry = (retry * 2).min(MAX_RETRY);
    }
    refuse_pending(&mut calls, address).await;
}

/// Answers whatever is still queued on the way out. A command that is simply dropped reaches
/// its caller as "the integration dropped the call", which says nothing; this says what
/// happened. Called wherever this task stops: shutdown, or another connection taking over.
async fn refuse_pending(calls: &mut mpsc::Receiver<IncomingCall>, address: SocketAddr) {
    calls.close();
    while let Some(incoming) = calls.recv().await {
        let why = format!("the connection to {address} ended before this could be sent");
        incoming.reply(Err(ServiceError::unavailable(why)));
    }
}

/// Why a session ended.
enum Ended {
    /// The integration is stopping; don't reconnect.
    Stopping,
    /// The device went away, with the reason to show.
    Disconnected(String),
}

/// One connection, from TCP to disconnection.
async fn session(
    connection: Connection,
    events: &mpsc::Sender<Event>,
    calls: &mut mpsc::Receiver<IncomingCall>,
    known: &mut Option<UniqueId>,
    warned: &mut bool,
    connected: &mut bool,
) -> Result<Ended, String> {
    let address = connection.address;
    // Bounded from the first packet: everything up to "connected and listening" has to finish
    // or be given up on. An address that silently swallows packets — a device that has moved
    // on, a firewall — would otherwise hold this task for the operating system's own TCP
    // timeout, which is minutes, saying nothing the whole time.
    let opening = async {
        let mut client = EspHomeClient::builder()
            .address(&address.to_string())
            .connect()
            .await
            .map_err(|e| format!("can't connect to {address}: {e}"))?;
        let device = handshake(&mut client).await?;
        let device_unique_id = map::device_id(&device).map_err(|e| e.to_string())?;
        let description = map::device(&device).map_err(|e| e.to_string())?;
        let entities = list_entities(&mut client, &device_unique_id).await?;
        Ok::<_, String>((client, device, device_unique_id, description, entities))
    };
    let (mut client, device, device_unique_id, description, entities) =
        match tokio::time::timeout(SETUP_TIMEOUT, opening).await {
            Ok(opened) => opened?,
            Err(_) => {
                return Err(format!(
                    "{address} didn't answer as an ESPHome device within {}s",
                    SETUP_TIMEOUT.as_secs()
                ));
            }
        };
    let lights: HashMap<u32, LightCapabilities> = entities
        .iter()
        .filter_map(|(key, entity)| match &entity.capabilities {
            Capabilities::Light(light) => Some((*key, light.clone())),
            _ => None,
        })
        .collect();
    let by_key: HashMap<u32, UniqueId> = entities
        .iter()
        .map(|(key, entity)| (*key, entity.unique_id.clone()))
        .collect();
    let by_unique_id: HashMap<UniqueId, u32> =
        by_key.iter().map(|(key, id)| (id.clone(), *key)).collect();

    if !*warned {
        *warned = true;
        // Said once per connection, not once per attempt: plain ESPHome has no authentication
        // of its own, so this is a trust decision, not a detail.
        tracing::warn!(
            device = %device_unique_id,
            name = %description.name,
            %address,
            "connected without authentication; anything on this network that answers to this \
             address is believed"
        );
    }
    tracing::info!(
        device = %device_unique_id,
        name = %description.name,
        entities = entities.len(),
        esphome = %device.esphome_version,
        "connected"
    );
    *known = Some(device_unique_id.clone());
    *connected = true;
    let sent = events
        .send(Event::Arrived {
            connection,
            device: Box::new(description),
            entities: entities.into_iter().map(|(_, entity)| entity).collect(),
        })
        .await;
    if sent.is_err() {
        return Ok(Ended::Stopping);
    }

    client
        .try_write(SubscribeStatesRequest {})
        .await
        .map_err(|e| format!("can't subscribe to state from {address}: {e}"))?;

    // Which entity a command is still waiting on, so the state it produces can be traced back
    // to whoever asked for it.
    let mut commanded: HashMap<u32, VecDeque<(ContextId, Instant)>> = HashMap::new();

    // Silence is ambiguous: a device with nothing to say and one that has gone away look the
    // same down a TCP connection that nobody has closed. So after a quiet spell, ask.
    let mut quiet = tokio::time::interval_at(Instant::now() + PING_AFTER, PING_AFTER);
    let mut asked = false;

    loop {
        tokio::select! {
            message = client.try_read() => {
                // Anything at all is proof it's still there.
                asked = false;
                let message = match message {
                    Ok(message) => message,
                    Err(e) => return Ok(Ended::Disconnected(format!("{address} stopped answering: {e}"))),
                };
                match message {
                    EspHomeMessage::PingRequest(_) => {
                        client.try_write(PingResponse {}).await
                            .map_err(|e| format!("can't answer {address}'s ping: {e}"))?;
                    }
                    EspHomeMessage::DisconnectRequest(_) => {
                        // A polite goodbye, e.g. the device is rebooting after an update.
                        return Ok(Ended::Disconnected(format!("{address} said goodbye")));
                    }
                    message => {
                        if let Some(report) = report(&message, &by_key, &lights, &mut commanded) {
                            let event = Event::Reported {
                                connection,
                                device: device_unique_id.clone(),
                                report: Box::new(report),
                            };
                            if events.send(event).await.is_err() {
                                return Ok(Ended::Stopping);
                            }
                        }
                    }
                }
            }
            call = calls.recv() => {
                let Some(incoming) = call else { return Ok(Ended::Stopping) };
                let key = by_unique_id.get(&incoming.call.unique_id).copied();
                command(&mut client, incoming, key, &lights, &mut commanded).await;
            }
            _ = quiet.tick() => {
                if asked {
                    return Ok(Ended::Disconnected(format!(
                        "{address} stopped answering (no reply to a ping in {}s)",
                        PING_AFTER.as_secs()
                    )));
                }
                client.try_write(esphome_client::types::PingRequest {}).await
                    .map_err(|e| format!("can't ask {address} whether it's still there: {e}"))?;
                asked = true;
            }
        }
    }
}

/// Hello, then the device's own description. The client library sends `HelloRequest` and
/// `ConnectRequest` while connecting; this asks who answered.
async fn handshake(
    client: &mut EspHomeClient,
) -> Result<esphome_client::types::DeviceInfoResponse, String> {
    client
        .try_write(esphome_client::types::DeviceInfoRequest {})
        .await
        .map_err(|e| format!("can't ask the device who it is: {e}"))?;
    loop {
        let message = client
            .try_read()
            .await
            .map_err(|e| format!("no answer to who-are-you: {e}"))?;
        match message {
            EspHomeMessage::DeviceInfoResponse(info) => return Ok(info),
            EspHomeMessage::PingRequest(_) => {
                client
                    .try_write(PingResponse {})
                    .await
                    .map_err(|e| format!("can't answer a ping: {e}"))?;
            }
            EspHomeMessage::DisconnectRequest(_) => {
                return Err("the device hung up during the handshake".to_owned());
            }
            _ => {}
        }
    }
}

/// Everything the device has, of the kinds Irori models. Kinds it doesn't (fan, cover, climate,
/// text sensors, …) are counted and mentioned once, not dropped silently.
async fn list_entities(
    client: &mut EspHomeClient,
    device: &UniqueId,
) -> Result<Vec<(u32, EntityDescription)>, String> {
    client
        .try_write(ListEntitiesRequest {})
        .await
        .map_err(|e| format!("can't ask the device what it has: {e}"))?;

    let mut entities = Vec::new();
    let mut skipped = 0_usize;
    loop {
        let message = client
            .try_read()
            .await
            .map_err(|e| format!("the device stopped listing what it has: {e}"))?;
        let described = match &message {
            EspHomeMessage::ListEntitiesDoneResponse(_) => {
                if skipped > 0 {
                    tracing::info!(
                        device = %device,
                        skipped,
                        "left out entities of kinds Irori doesn't model yet \
                         (fan, cover, climate, text sensors, and the rest)"
                    );
                }
                return Ok(entities);
            }
            EspHomeMessage::ListEntitiesLightResponse(e) => (e.key, map::light(device, e)),
            EspHomeMessage::ListEntitiesSwitchResponse(e) => (e.key, map::switch(device, e)),
            EspHomeMessage::ListEntitiesSensorResponse(e) => (e.key, map::sensor(device, e)),
            EspHomeMessage::ListEntitiesBinarySensorResponse(e) => {
                (e.key, map::binary_sensor(device, e))
            }
            EspHomeMessage::PingRequest(_) => {
                client
                    .try_write(PingResponse {})
                    .await
                    .map_err(|e| format!("can't answer a ping: {e}"))?;
                continue;
            }
            // Between the request and `Done` a device sends nothing but entity listings, so
            // anything else here is a kind this build doesn't model.
            _ => {
                skipped += 1;
                continue;
            }
        };
        match described {
            (key, Ok(entity)) => entities.push((key, entity)),
            // One unusable entity (an empty name, say) shouldn't cost us the whole device.
            (_, Err(e)) => tracing::warn!(device = %device, error = %e, "skipping an entity"),
        }
    }
}

/// A state message turned into a report, if it's for an entity we know.
fn report(
    message: &EspHomeMessage,
    by_key: &HashMap<u32, UniqueId>,
    lights: &HashMap<u32, LightCapabilities>,
    commanded: &mut HashMap<u32, VecDeque<(ContextId, Instant)>>,
) -> Option<StateReport> {
    let (key, state) = match message {
        EspHomeMessage::LightStateResponse(s) => {
            let known = lights.get(&s.key)?;
            (s.key, Some(map::light_state(s, known)))
        }
        EspHomeMessage::SwitchStateResponse(s) => (s.key, Some(map::switch_state(s))),
        EspHomeMessage::BinarySensorStateResponse(s) => (s.key, Some(map::binary_sensor_state(s))),
        EspHomeMessage::SensorStateResponse(s) => (s.key, map::sensor_state(s)),
        _ => return None,
    };
    let unique_id = by_key.get(&key)?.clone();
    Some(StateReport {
        unique_id,
        state,
        attributes: std::collections::BTreeMap::new(),
        caused_by: caused_by(key, commanded),
    })
}

/// The context of the command this report answers, if it's recent enough to be its result.
///
/// A queue rather than a single slot: the core lets a second call go out as soon as the first
/// has been sent, so two can be in flight on one entity, and each report answers the oldest one
/// still waiting. Anything older than the window was answered by a report Irori never saw.
fn caused_by(
    key: u32,
    commanded: &mut HashMap<u32, VecDeque<(ContextId, Instant)>>,
) -> Option<ContextId> {
    let waiting = commanded.get_mut(&key)?;
    while let Some((context, at)) = waiting.pop_front() {
        if at.elapsed() < CAUSED_BY_WINDOW {
            return Some(context);
        }
    }
    commanded.remove(&key);
    None
}

/// Carries out a service call, and remembers who asked so the state it produces can say.
async fn command(
    client: &mut EspHomeClient,
    incoming: IncomingCall,
    key: Option<u32>,
    lights: &HashMap<u32, LightCapabilities>,
    commanded: &mut HashMap<u32, VecDeque<(ContextId, Instant)>>,
) {
    let Some(key) = key else {
        // The core only sends calls for entities this device described, so this means the
        // device was reflashed between the two.
        let why = format!(
            "`{}` isn't on this device any more",
            incoming.call.unique_id
        );
        incoming.reply(Err(ServiceError::unavailable(why)));
        return;
    };
    let context = incoming.call.context.id.clone();
    let written = match &incoming.call.service {
        Service::LightTurnOn(data) => {
            let known = lights.get(&key);
            let mut request = LightCommandRequest {
                key,
                has_state: true,
                state: true,
                ..Default::default()
            };
            if let Some(brightness) = data.brightness {
                request.has_brightness = true;
                request.brightness = map::to_fraction(brightness);
            }
            if let Some(kelvin) = data.color_temp_kelvin {
                request.has_color_temperature = true;
                request.color_temperature = map::mireds(kelvin);
            }
            if let Some([red, green, blue]) = data.rgb {
                request.has_rgb = true;
                request.red = map::to_fraction(red);
                request.green = map::to_fraction(green);
                request.blue = map::to_fraction(blue);
            }
            // The core checks capabilities before it gets here; this is the last guard.
            if known.is_none() {
                incoming.reply(Err(ServiceError::failed(
                    "this entity isn't a light on the device".to_owned(),
                )));
                return;
            }
            client.try_write(request).await.map_err(|e| e.to_string())
        }
        Service::LightTurnOff => client
            .try_write(LightCommandRequest {
                key,
                has_state: true,
                state: false,
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string()),
        Service::SwitchTurnOn | Service::SwitchTurnOff => client
            .try_write(SwitchCommandRequest {
                key,
                state: matches!(incoming.call.service, Service::SwitchTurnOn),
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string()),
    };
    match written {
        Ok(()) => {
            // ESPHome doesn't acknowledge a command; it reports the new state. The device has
            // the command, which is what a reply means here (spec §7.3).
            commanded
                .entry(key)
                .or_default()
                .push_back((context, Instant::now()));
            incoming.reply(Ok(()));
        }
        Err(why) => incoming.reply(Err(ServiceError::unavailable(format!(
            "couldn't send the command: {why}"
        )))),
    }
}

#[cfg(test)]
mod tests {
    use irori_integration::host::incoming_call;
    use irori_integration::types::{
        Capabilities, ColorTempRange, Context, ContextId, LightTurnOn, Origin, ServiceCall, State,
        UniqueId, UserId,
    };

    use super::*;
    use crate::fake_device;

    type Answer = tokio::sync::oneshot::Receiver<Result<(), ServiceError>>;

    fn call(unique_id: &str, service: Service) -> (IncomingCall, ContextId, Answer) {
        let context = Context {
            id: ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9").expect("valid"),
            parent_id: None,
            origin: Origin::User {
                user_id: UserId::try_from("nico").expect("valid"),
            },
        };
        let id = context.id.clone();
        let (incoming, answer) = incoming_call(ServiceCall {
            unique_id: UniqueId::try_from(unique_id).expect("valid"),
            service,
            context,
        });
        (incoming, id, answer)
    }

    /// The whole conversation with a device, against a stand-in that speaks the real protocol:
    /// what it has, what it's doing, and doing what it's told.
    #[tokio::test]
    async fn a_device_introduces_itself_reports_and_obeys() {
        let (address, commanded) = fake_device::start().await;
        let (events_tx, mut events) = mpsc::channel(64);
        let (calls_tx, calls_rx) = mpsc::channel(8);
        let connection = Connection { id: 1, address };
        let task = tokio::spawn(run(connection, events_tx, calls_rx));

        // What it has. The fake device also offers a fan, which Irori doesn't model yet.
        let Some(Event::Arrived {
            device, entities, ..
        }) = events.recv().await
        else {
            panic!("the device never introduced itself");
        };
        assert_eq!(device.unique_id.as_str(), fake_device::MAC);
        assert_eq!(device.name.as_str(), "Fake device");
        assert_eq!(device.sw_version.as_deref(), Some("2026.8.2"));
        assert_eq!(
            entities.len(),
            4,
            "the fan should be left out: {entities:?}"
        );

        let lamp = entities
            .iter()
            .find(|e| e.name.as_ref().is_some_and(|n| n.as_str() == "Desk lamp"))
            .expect("the lamp is described");
        assert_eq!(
            lamp.unique_id,
            map::entity_id(
                &device.unique_id,
                irori_integration::types::EntityKind::Light,
                fake_device::LIGHT_KEY,
            )
            .expect("valid")
        );
        // 153-500 mireds is 2000-6536 K, and a colour-temperature light dims.
        assert_eq!(
            lamp.capabilities,
            Capabilities::Light(irori_integration::types::LightCapabilities {
                brightness: true,
                color_temp_kelvin: Some(ColorTempRange {
                    min: 2000,
                    max: 6536
                }),
                rgb: false,
            })
        );

        // What it's doing.
        let mut reports = Vec::new();
        while reports.len() < 4 {
            match events.recv().await {
                Some(Event::Reported { report, .. }) => reports.push(*report),
                other => panic!("expected a state report, got {other:?}"),
            }
        }
        let lamp_state = reports
            .iter()
            .find(|r| r.unique_id == lamp.unique_id)
            .expect("the lamp reported");
        let Some(State::Light(light)) = &lamp_state.state else {
            panic!("the lamp reports a light state");
        };
        assert!(!light.on);
        assert_eq!(light.brightness, Some(128), "0.5 of 255");
        assert_eq!(light.color_temp_kelvin, Some(2703), "370 mireds");
        assert_eq!(
            lamp_state.caused_by, None,
            "nobody asked for it; the device just said so"
        );

        // Doing what it's told, and saying who asked.
        let (incoming, context, _answer) = call(
            lamp.unique_id.as_str(),
            Service::LightTurnOn(LightTurnOn {
                brightness: Some(255),
                ..Default::default()
            }),
        );
        calls_tx.send(incoming).await.expect("the task is running");

        let mut answered = None;
        for _ in 0..10 {
            match events.recv().await {
                Some(Event::Reported { report, .. }) if report.unique_id == lamp.unique_id => {
                    answered = Some(*report);
                    break;
                }
                Some(_) => continue,
                None => break,
            }
        }
        let answered = answered.expect("the lamp reported what the command did");
        assert_eq!(
            answered.caused_by,
            Some(context),
            "the change traces back to the command"
        );
        let Some(State::Light(light)) = &answered.state else {
            panic!("the lamp reports a light state");
        };
        assert!(light.on, "it was told to turn on");

        {
            let sent = commanded.lock().expect("not poisoned");
            assert_eq!(sent.lights.len(), 1);
            assert!(sent.lights[0].state, "the device was told to turn on");
            assert!(sent.lights[0].has_brightness);
            assert!(
                (sent.lights[0].brightness - 1.0).abs() < f32::EPSILON,
                "255 of 255"
            );
        }

        // Stopping: dropping the command channel ends the task.
        drop(calls_tx);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the task stops when the integration does")
            .expect("it stops without panicking");
    }

    /// A command for a device that isn't answering is refused with a reason. Dropping it would
    /// reach the caller as "the integration dropped the call", which says nothing useful.
    #[tokio::test]
    async fn a_command_for_a_device_that_is_away_is_refused_not_dropped() {
        // Nothing listens on port 1, so the task stays in its retry loop.
        let address: SocketAddr = "127.0.0.1:1".parse().expect("a valid address");
        let (events_tx, mut events) = mpsc::channel(8);
        let (calls_tx, calls_rx) = mpsc::channel(8);
        let connection = Connection { id: 1, address };
        let task = tokio::spawn(run(connection, events_tx, calls_rx));

        match events.recv().await {
            Some(Event::Unreachable { connection: at, .. }) => assert_eq!(at.address, address),
            other => panic!("expected an unreachable device, got {other:?}"),
        }

        let (incoming, _, answer) = call("light.somewhere", Service::LightTurnOff);
        calls_tx.send(incoming).await.expect("the task is running");
        let answered = tokio::time::timeout(Duration::from_secs(5), answer)
            .await
            .expect("the call is answered rather than left hanging")
            .expect("the call is answered rather than dropped");
        let Err(error) = answered else {
            panic!("a device that isn't there can't have done it");
        };
        assert_eq!(error.code, irori_integration::ServiceErrorCode::Unavailable);

        drop(calls_tx);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the task stops when the integration does")
            .expect("it stops without panicking");
    }
}
