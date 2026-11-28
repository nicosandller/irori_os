//! The protocol against a fake printer on loopback. Nothing here reaches the real network.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use irori_protocol::host::{self, Reports};
use irori_protocol::{Health, IncomingCall, ProtocolError, ServiceErrorCode};
use irori_types::{
    Availability, Context, ContextId, DeviceDescription, EntityDescription, Origin, SensorValue,
    Service, ServiceCall, State, UniqueId,
};
use serde_json::json;
use tokio::sync::{mpsc, watch};

use crate::drive;
use crate::fake::Fake;
use crate::link::{MOST_MESSAGE, Timing};
use crate::settings::Settings;

/// Made up: no printer has it.
const SERIAL: &str = "00TEST0000000001";
const CODE: &str = "24681357";

fn settings(fake: &Fake, code: &str, certificate: Option<&str>) -> Settings {
    serde_json::from_value(json!({
        "host": "127.0.0.1", "port": fake.port, "serial": SERIAL,
        "access_code": code, "certificate": certificate,
    }))
    .expect("valid settings")
}

/// A print under way, with one filament unit: two filaments and an empty slot.
fn printing() -> serde_json::Value {
    json!({
        "gcode_state": "RUNNING", "mc_percent": 40, "mc_remaining_time": 75,
        "layer_num": 12, "total_layer_num": 120, "nozzle_temper": 219.5, "bed_temper": 60.0,
        "ams": { "ams": [{ "id": "0", "tray": [
            { "id": "0", "tray_type": "PLA", "tray_color": "00AE42FF", "remain": 80 },
            { "id": "1", "tray_type": "PETG", "tray_color": "FFFFFFFF", "remain": -1 },
            { "id": "2" },
        ]}]},
    })
}

#[derive(Debug, Default)]
struct Log {
    health: Vec<Health>,
    availability: Vec<Availability>,
    devices: Vec<DeviceDescription>,
    entities: Vec<EntityDescription>,
    stored: Vec<serde_json::Value>,
}

struct World {
    calls: mpsc::Sender<IncomingCall>,
    reports: Reports,
    log: Arc<Mutex<Log>>,
    /// Kept so `next_call` doesn't see the stop channel close and end the run.
    _stop: watch::Sender<bool>,
    drive: tokio::task::JoinHandle<Result<(), ProtocolError>>,
}

/// Starts the protocol. `remembered` is the certificate its store already holds, if any.
async fn world(settings: Settings, remembered: Option<&str>) -> World {
    let (ctx, host) = irori_protocol::host::connect();
    let log = Arc::new(Mutex::new(Log::default()));
    let recorded = Arc::clone(&log);
    let kept = remembered.map(|print| json!({ "serial": SERIAL, "fingerprint": print }));
    let mut ops = host.ops;
    tokio::spawn(async move {
        while let Some(op) = ops.recv().await {
            let mut log = recorded.lock().expect("log");
            match op {
                host::Op::DescribeDevice(device, reply) => {
                    log.devices.push(device);
                    let _ = reply.send(Ok(()));
                }
                host::Op::DescribeEntity(entity, reply) => {
                    log.entities.push(entity);
                    let _ = reply.send(Ok(()));
                }
                host::Op::SetAvailability(_, availability, reply) => {
                    log.availability.push(availability);
                    let _ = reply.send(Ok(()));
                }
                host::Op::SetHealth(health) => log.health.push(health),
                host::Op::Load(_, reply) => {
                    let _ = reply.send(Ok(kept.clone()));
                }
                host::Op::Store(_, value, reply) => {
                    log.stored.extend(value);
                    let _ = reply.send(Ok(()));
                }
                host::Op::RemoveEntity(_, reply) | host::Op::RemoveDevice(_, reply) => {
                    let _ = reply.send(Ok(()));
                }
                host::Op::SetWaiting(_)
                | host::Op::SetUnmodeled(_)
                | host::Op::SuggestLayout(_)
                | host::Op::SetActionOpen(_, _)
                | host::Op::SetAvailableActions(_)
                | host::Op::SetNetworks(_) => {}
            }
        }
    });
    let timing = Timing {
        first: Duration::from_millis(20),
        longest: Duration::from_millis(100),
        steady: Duration::from_secs(30),
    };
    World {
        calls: host.calls,
        reports: host.reports,
        log,
        _stop: host.stop,
        drive: tokio::spawn(drive(settings, ctx, timing)),
    }
}

/// Waits until `check` holds, or fails after five seconds.
async fn until(what: &str, check: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

impl World {
    async fn until(&self, what: &str, check: impl Fn(&Log) -> bool) {
        until(what, || check(&self.log.lock().expect("log"))).await;
    }

    /// The last health said, once one holding `text` has been.
    async fn health(&self, text: &str) -> String {
        let said = |log: &Log| match log.health.last() {
            Some(Health::Degraded(why)) if why.contains(text) => Some(why.clone()),
            _ => None,
        };
        self.until(text, |log| said(log).is_some()).await;
        said(&self.log.lock().expect("log")).expect("just seen")
    }

    /// Collects reports until `id` has one, and returns every state reported on the way.
    async fn states(&self, id: &str) -> BTreeMap<String, Option<State>> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut seen = BTreeMap::new();
        loop {
            for report in self.reports.drain() {
                seen.insert(report.unique_id.as_str().to_owned(), report.state);
            }
            if seen.contains_key(&format!("{SERIAL}-{id}")) {
                return seen;
            }
            tokio::select! {
                _ = self.reports.ready() => {}
                _ = tokio::time::sleep_until(deadline) => panic!("no report for {id}; got {seen:?}"),
            }
        }
    }
}

fn number(states: &BTreeMap<String, Option<State>>, id: &str) -> Option<f64> {
    match states.get(&format!("{SERIAL}-{id}"))? {
        Some(State::Sensor(sensor)) => match sensor.value {
            SensorValue::Number(number) => Some(number),
            SensorValue::Text(_) => None,
        },
        _ => None,
    }
}

fn text(states: &BTreeMap<String, Option<State>>, id: &str) -> Option<String> {
    match states.get(&format!("{SERIAL}-{id}"))? {
        Some(State::Sensor(sensor)) => match &sensor.value {
            SensorValue::Text(text) => Some(text.clone()),
            SensorValue::Number(_) => None,
        },
        _ => None,
    }
}

#[tokio::test]
async fn a_printer_is_described_and_a_partial_report_changes_only_what_it_names() {
    let fake = Fake::start(SERIAL, CODE).await;
    let world = world(settings(&fake, CODE, None), None).await;
    // It signs in, listens to the printer's reports and asks for the whole status, once.
    until("the status request", || fake.requests() == 1).await;
    {
        let seen = fake.seen.lock().expect("seen");
        assert_eq!(seen.codes, [CODE]);
        assert_eq!(seen.subscribed, [format!("device/{SERIAL}/report")]);
        assert_eq!(seen.requests[0].0, format!("device/{SERIAL}/request"));
        assert!(seen.requests[0].1.contains(r#""command":"pushall""#));
    }

    fake.report(printing());
    let states = world.states("unit0-slot2").await;
    assert_eq!(text(&states, "state").as_deref(), Some("printing"));
    assert_eq!(number(&states, "progress"), Some(40.0));
    assert_eq!(number(&states, "remaining"), Some(75.0));
    assert_eq!(number(&states, "layer"), Some(12.0));
    assert_eq!(number(&states, "layers"), Some(120.0));
    assert_eq!(number(&states, "nozzle"), Some(219.5));
    assert_eq!(number(&states, "bed"), Some(60.0));
    assert_eq!(text(&states, "unit0-slot0").as_deref(), Some("PLA"));
    assert_eq!(text(&states, "unit0-slot1").as_deref(), Some("PETG"));
    assert_eq!(text(&states, "unit0-slot2").as_deref(), Some("empty"));
    {
        let log = world.log.lock().expect("log");
        assert_eq!(log.devices.len(), 1);
        assert_eq!(log.devices[0].unique_id.as_str(), SERIAL);
        assert_eq!(log.entities.len(), 10);
        assert_eq!(log.health, [Health::Running]);
        // The certificate it signed in with is remembered for next time.
        assert_eq!(
            log.stored,
            [json!({ "serial": SERIAL, "fingerprint": fake.fingerprint })]
        );
    }

    // The printer then says only what changed. The rest keeps what it had: nothing else is
    // reported, and nothing is reported as unknown.
    fake.report(json!({ "mc_percent": 41 }));
    fake.report(json!({ "layer_num": 13 }));
    let states = world.states("layer").await;
    assert_eq!(number(&states, "progress"), Some(41.0));
    assert_eq!(number(&states, "layer"), Some(13.0));
    assert_eq!(states.len(), 2, "{states:?}");
    assert_eq!(world.log.lock().expect("log").entities.len(), 10);
    world.drive.abort();
}

#[tokio::test]
async fn a_different_certificate_gets_no_access_code() {
    let fake = Fake::start(SERIAL, CODE).await;
    let other = "00".repeat(32);
    // Another certificate was remembered for this printer.
    let first = world(settings(&fake, CODE, None), Some(&other)).await;
    let why = first.health("different certificate").await;
    assert!(why.contains(&fake.fingerprint), "{why}");
    assert!(!why.contains(CODE), "{why}");
    // Typing a fingerprint that isn't the printer's is refused the same way.
    let typed = world(settings(&fake, CODE, Some(&other)), None).await;
    typed.health("different certificate").await;
    assert!(fake.seen.lock().expect("seen").codes.is_empty());
    assert!(first.log.lock().expect("log").devices.is_empty());
    first.drive.abort();
    typed.drive.abort();

    // Typing the fingerprint it was shown takes the new certificate, and remembers it.
    let world = world(settings(&fake, CODE, Some(&fake.fingerprint)), Some(&other)).await;
    until("the status request", || fake.requests() == 1).await;
    world
        .until("the certificate kept", |log| !log.stored.is_empty())
        .await;
    let stored = world.log.lock().expect("log").stored.clone();
    assert_eq!(
        stored,
        [json!({ "serial": SERIAL, "fingerprint": fake.fingerprint })]
    );
    world.drive.abort();
}

#[tokio::test]
async fn a_wrong_access_code_says_so_without_repeating_it() {
    let fake = Fake::start(SERIAL, CODE).await;
    let world = world(settings(&fake, "13572468", None), None).await;
    let why = world.health("refused the access code").await;
    assert!(!why.contains("13572468"), "{why}");
    let log = world.log.lock().expect("log");
    assert!(log.devices.is_empty());
    // A certificate is only remembered once its printer took the access code.
    assert!(log.stored.is_empty());
    world.drive.abort();
}

#[tokio::test]
async fn a_dropped_connection_is_unavailable_until_the_printer_is_back() {
    let fake = Fake::start(SERIAL, CODE).await;
    let world = world(settings(&fake, CODE, None), None).await;
    until("the status request", || fake.requests() == 1).await;
    fake.report(printing());
    world.states("unit0-slot2").await;

    fake.hang_up();
    world
        .until("unavailable", |log| {
            log.availability == [Availability::Unavailable]
        })
        .await;
    world.health("can't be reached").await;

    // It connects again by itself, and asks for the whole status again.
    until("the second status request", || fake.requests() == 2).await;
    fake.report(json!({ "gcode_state": "PAUSE" }));
    let states = world.states("unit0-slot2").await;
    assert_eq!(text(&states, "state").as_deref(), Some("paused"));
    // What the printer said before the drop is kept, and said again.
    assert_eq!(number(&states, "progress"), Some(40.0));
    let log = world.log.lock().expect("log");
    assert_eq!(
        log.availability,
        [Availability::Unavailable, Availability::Available]
    );
    assert_eq!(log.health.last(), Some(&Health::Running));
    assert_eq!(log.devices.len(), 1);
    world.drive.abort();
}

#[tokio::test]
async fn a_message_over_the_size_limit_is_said_and_the_connection_recovers() {
    let fake = Fake::start(SERIAL, CODE).await;
    let world = world(settings(&fake, CODE, None), None).await;
    until("the status request", || fake.requests() == 1).await;
    fake.raw(vec![b' '; MOST_MESSAGE + 1]);
    world.health("over the").await;
    until("the second status request", || fake.requests() == 2).await;
    world.drive.abort();
}

#[tokio::test]
async fn commands_are_refused_while_read_only() {
    let fake = Fake::start(SERIAL, CODE).await;
    let world = world(settings(&fake, CODE, None), None).await;
    let (incoming, answer) = irori_protocol::host::incoming_call(ServiceCall {
        unique_id: UniqueId::try_from(format!("{SERIAL}-state")).expect("id"),
        service: Service::SwitchTurnOff,
        context: Context {
            id: ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9").expect("context"),
            parent_id: None,
            origin: Origin::System,
        },
    });
    world.calls.send(incoming).await.expect("running");
    let reply = tokio::time::timeout(Duration::from_secs(5), answer).await;
    let error = reply.expect("in time").expect("sent").expect_err("refused");
    assert_eq!(error.code, ServiceErrorCode::Failed);
    assert!(error.message.contains("only reads"), "{}", error.message);
    // Nothing but the status request ever goes to the printer.
    until("the status request", || fake.requests() == 1).await;
    assert!(
        fake.seen.lock().expect("seen").requests[0]
            .1
            .contains("pushall")
    );
    world.drive.abort();
}
