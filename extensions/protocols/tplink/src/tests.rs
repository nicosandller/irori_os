//! The protocol against a fake Tapo device on loopback. Nothing here probes the real network.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use irori_protocol::host::{self, Reports};
use irori_protocol::{IncomingCall, ProtocolError, ServiceError, ServiceErrorCode};
use irori_types::{
    Availability, Context, ContextId, DeviceDescription, EntityDescription, Origin, SensorValue,
    Service, ServiceCall, State, UniqueId, Waiting,
};
use tokio::sync::{mpsc, watch};

use crate::discover::{Discovery, Found};
use crate::drive;
use crate::fake::{self, Fake, Kind};
use crate::klap::{AuthHash, KlapError, Session};
use crate::settings::Settings;

const USER: &str = "me@example.com";
const PASSWORD: &str = "correct horse";

fn auth() -> AuthHash {
    AuthHash::new(USER, PASSWORD)
}

fn settings(account: bool) -> Settings {
    Settings {
        username: account.then(|| USER.to_owned()),
        password: account.then(|| PASSWORD.to_owned()),
        known_hosts: Vec::new(),
        poll_seconds: Some(1),
    }
}

#[derive(Debug, Default)]
struct Log {
    health: Vec<irori_protocol::Health>,
    availability: Vec<Availability>,
    devices: Vec<DeviceDescription>,
    entities: Vec<EntityDescription>,
    removed: Vec<UniqueId>,
    waiting: Vec<Vec<Waiting>>,
}

struct World {
    discovery: mpsc::Sender<Discovery>,
    calls: mpsc::Sender<IncomingCall>,
    reports: Reports,
    log: Arc<Mutex<Log>>,
    /// Kept so `next_call` doesn't see the stop channel close and end the run.
    _stop: watch::Sender<bool>,
    drive: tokio::task::JoinHandle<Result<(), ProtocolError>>,
}

async fn world(settings: Settings) -> World {
    let (ctx, host) = irori_protocol::host::connect();
    let log = Arc::new(Mutex::new(Log::default()));
    let recorded = Arc::clone(&log);
    let mut ops = host.ops;
    tokio::spawn(async move {
        while let Some(op) = ops.recv().await {
            match op {
                host::Op::DescribeDevice(device, reply) => {
                    recorded.lock().expect("log").devices.push(device);
                    let _ = reply.send(Ok(()));
                }
                host::Op::DescribeEntity(entity, reply) => {
                    recorded.lock().expect("log").entities.push(entity);
                    let _ = reply.send(Ok(()));
                }
                host::Op::SetAvailability(_, availability, reply) => {
                    recorded
                        .lock()
                        .expect("log")
                        .availability
                        .push(availability);
                    let _ = reply.send(Ok(()));
                }
                host::Op::SetHealth(health) => {
                    recorded.lock().expect("log").health.push(health);
                }
                host::Op::SetWaiting(waiting) => {
                    recorded.lock().expect("log").waiting.push(waiting);
                }
                host::Op::RemoveEntity(unique_id, reply) => {
                    recorded.lock().expect("log").removed.push(unique_id);
                    let _ = reply.send(Ok(()));
                }
                host::Op::RemoveDevice(_, reply) => {
                    let _ = reply.send(Ok(()));
                }
                host::Op::SetUnmodeled(_)
                | host::Op::SetActionOpen(_, _)
                | host::Op::SetAvailableActions(_)
                | host::Op::SetNetworks(_) => {}
                host::Op::Load(_, reply) => {
                    let _ = reply.send(Ok(None));
                }
                host::Op::Store(_, _, reply) => {
                    let _ = reply.send(Ok(()));
                }
            }
        }
    });
    let (discovery, incoming) = mpsc::channel(8);
    let drive = tokio::spawn(drive(settings, ctx, incoming));
    World {
        discovery,
        calls: host.calls,
        reports: host.reports,
        log,
        _stop: host.stop,
        drive,
    }
}

impl World {
    async fn see(&self, fake: &Fake, mac: &str, model: &str) {
        let found = Found {
            mac: mac.to_owned(),
            address: fake.address,
            model: model.to_owned(),
            device_type: "SMART.TAPOPLUG".to_owned(),
            encryption: "KLAP".to_owned(),
            known_host: None,
        };
        self.discovery
            .send(Discovery::Seen(found))
            .await
            .expect("discovery is open");
    }

    /// Waits until `check` holds for the log, or fails after five seconds.
    async fn until(&self, what: &str, check: impl Fn(&Log) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !check(&self.log.lock().expect("log")) {
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {what}: {:?}",
                    self.log.lock().expect("log")
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Collects reports until every id in `want` has one.
    async fn states(&self, want: &[&str]) -> BTreeMap<String, Option<State>> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut seen = BTreeMap::new();
        loop {
            for report in self.reports.drain() {
                seen.insert(report.unique_id.as_str().to_owned(), report.state);
            }
            if want.iter().all(|id| seen.contains_key(*id)) {
                return seen;
            }
            tokio::select! {
                _ = self.reports.ready() => {}
                _ = tokio::time::sleep_until(deadline) => panic!("missing reports; got {seen:?}"),
            }
        }
    }

    /// Collects reports until `id` is reported as `check` wants.
    async fn until_state(&self, id: &str, check: impl Fn(&Option<State>) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if self
                .reports
                .drain()
                .into_iter()
                .any(|report| report.unique_id.as_str() == id && check(&report.state))
            {
                return;
            }
            tokio::select! {
                _ = self.reports.ready() => {}
                _ = tokio::time::sleep_until(deadline) => panic!("{id} never reported as wanted"),
            }
        }
    }

    async fn shutdown(self) {
        drop(self.discovery);
        let _ = tokio::time::timeout(Duration::from_secs(2), self.drive).await;
    }
}

fn number(state: &Option<State>) -> f64 {
    match state {
        Some(State::Sensor(sensor)) => match sensor.value {
            SensorValue::Number(value) => value,
            SensorValue::Text(_) => panic!("text, not a number"),
        },
        other => panic!("not a sensor: {other:?}"),
    }
}

fn on(state: &Option<State>) -> bool {
    match state {
        Some(State::Switch(switch)) => switch.on,
        other => panic!("not a switch: {other:?}"),
    }
}

#[tokio::test]
async fn a_session_reads_the_device_over_klap() {
    let fake = fake::start(Kind::Plug, auth()).await;
    let mut session = Session::connect(fake.address, &auth())
        .await
        .expect("logged in");
    let request = crate::smart::multiple(&[(crate::smart::DEVICE_INFO, serde_json::Value::Null)]);
    for _ in 0..3 {
        let response = session.request(&request).await.expect("answered");
        let answers = crate::smart::answers(&response).expect("ok");
        assert!(answers[crate::smart::DEVICE_INFO].is_ok());
    }
}

#[tokio::test]
async fn a_device_with_no_account_set_is_read_with_its_setup_or_blank_login() {
    for (username, password) in [("test@tp-link.net", "test"), ("", "")] {
        let fake = fake::start(Kind::Plug, AuthHash::new(username, password)).await;
        let mut session = Session::connect(fake.address, &auth())
            .await
            .expect("logged in with the fallback");
        let request =
            crate::smart::multiple(&[(crate::smart::DEVICE_INFO, serde_json::Value::Null)]);
        let response = session.request(&request).await.expect("answered");
        assert!(crate::smart::answers(&response).expect("ok")[crate::smart::DEVICE_INFO].is_ok());
    }
}

#[tokio::test]
async fn a_wrong_account_is_told_apart_from_an_unreachable_device() {
    let fake = fake::start(Kind::Plug, auth()).await;
    let wrong = AuthHash::new(USER, "wrong");
    assert!(matches!(
        Session::connect(fake.address, &wrong).await,
        Err(KlapError::Account)
    ));
    drop(fake);
    let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = closed.local_addr().expect("address");
    drop(closed);
    assert!(matches!(
        Session::connect(address, &auth()).await,
        Err(KlapError::Io(_))
    ));
}

#[tokio::test]
async fn a_plug_is_described_and_read() {
    let fake = fake::start(Kind::Plug, auth()).await;
    let world = world(settings(true)).await;
    let mac = "02:00:00:00:00:01";
    world.see(&fake, mac, "PP1(EU)").await;

    let states = world
        .states(&[
            "02:00:00:00:00:01-switch",
            "02:00:00:00:00:01-power",
            "02:00:00:00:00:01-voltage",
            "02:00:00:00:00:01-current",
            "02:00:00:00:00:01-energy_today",
            "02:00:00:00:00:01-energy_total",
            "02:00:00:00:00:01-led",
            "02:00:00:00:00:01-signal",
        ])
        .await;
    assert!(on(&states["02:00:00:00:00:01-switch"]));
    assert_eq!(number(&states["02:00:00:00:00:01-power"]), 80.5);
    assert_eq!(number(&states["02:00:00:00:00:01-voltage"]), 241.2);
    assert_eq!(number(&states["02:00:00:00:00:01-current"]), 0.412);
    assert_eq!(number(&states["02:00:00:00:00:01-energy_today"]), 1.5);
    assert_eq!(number(&states["02:00:00:00:00:01-energy_total"]), 300.0);
    assert_eq!(number(&states["02:00:00:00:00:01-signal"]), -48.0);

    {
        let log = world.log.lock().expect("log");
        assert_eq!(log.devices.len(), 1);
        assert_eq!(log.devices[0].name.as_str(), "Lamp");
        assert_eq!(log.devices[0].model.as_deref(), Some("PP1"));
        assert_eq!(log.entities.len(), 8);
    }

    // A change is reported; an unchanged value isn't sent again.
    fake.state.lock().expect("state").power_mw = 2_000_000;
    let states = world.states(&["02:00:00:00:00:01-power"]).await;
    assert_eq!(number(&states["02:00:00:00:00:01-power"]), 2000.0);
    assert!(!states.contains_key("02:00:00:00:00:01-voltage"));
    world.shutdown().await;
}

#[tokio::test]
async fn a_strip_has_an_outlet_device_per_socket_reached_through_it() {
    let fake = fake::start(Kind::Strip, auth()).await;
    let world = world(settings(true)).await;
    let mac = "02:00:00:00:00:03";
    world.see(&fake, mac, "PS4(EU)").await;

    let states = world
        .states(&[
            "02:00:00:00:00:03-outlet1-switch",
            "02:00:00:00:00:03-outlet2-switch",
            "02:00:00:00:00:03-outlet2-power",
            "02:00:00:00:00:03-led",
        ])
        .await;
    assert!(on(&states["02:00:00:00:00:03-outlet2-switch"]));
    assert!(!on(&states["02:00:00:00:00:03-outlet1-switch"]));
    assert_eq!(number(&states["02:00:00:00:00:03-outlet2-power"]), 2900.0);
    // The strip itself has no relay, and no meter of its own.
    assert!(!states.contains_key("02:00:00:00:00:03-switch"));
    assert!(!states.contains_key("02:00:00:00:00:03-power"));

    {
        let log = world.log.lock().expect("log");
        let names: Vec<&str> = log
            .devices
            .iter()
            .map(|device| device.name.as_str())
            .collect();
        assert_eq!(names, ["PS4 00:03", "PS4 00:03 outlet 1", "Desk Lamp"]);
        for outlet in &log.devices[1..] {
            assert_eq!(
                outlet.via_device_unique_id.as_ref().map(UniqueId::as_str),
                Some(mac)
            );
        }
    }
    world.shutdown().await;
}

#[tokio::test]
async fn an_outlet_that_cant_be_read_doesnt_list_the_next_one_twice() {
    let fake = fake::start(Kind::Strip, auth()).await;
    fake.state.lock().expect("state").unreadable_outlet = true;
    let world = world(settings(true)).await;
    world.see(&fake, "02:00:00:00:00:03", "PS4(EU)").await;
    world
        .states(&[
            "02:00:00:00:00:03-outlet1-switch",
            "02:00:00:00:00:03-outlet2-switch",
        ])
        .await;
    {
        let log = world.log.lock().expect("log");
        let ids: Vec<&str> = log.devices.iter().map(|d| d.unique_id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "02:00:00:00:00:03",
                "02:00:00:00:00:03-outlet1",
                "02:00:00:00:00:03-outlet2"
            ]
        );
    }
    world.shutdown().await;
}

#[tokio::test]
async fn switching_is_refused_while_read_only() {
    let world = world(settings(true)).await;
    let (incoming, answer) = irori_protocol::host::incoming_call(ServiceCall {
        unique_id: UniqueId::try_from("02:00:00:00:00:01-switch").expect("id"),
        service: Service::SwitchTurnOff,
        context: Context {
            id: ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9").expect("context"),
            parent_id: None,
            origin: Origin::System,
        },
    });
    world.calls.send(incoming).await.expect("running");
    let reply: Result<(), ServiceError> = tokio::time::timeout(Duration::from_secs(5), answer)
        .await
        .expect("in time")
        .expect("sent");
    let error = reply.expect_err("refused");
    assert_eq!(error.code, ServiceErrorCode::Failed);
    assert!(error.message.contains("only reads"), "{}", error.message);
    world.shutdown().await;
}

#[tokio::test]
async fn without_an_account_a_device_waits_for_one() {
    let fake = fake::start(Kind::Plug, auth()).await;
    let world = world(settings(false)).await;
    world.see(&fake, "02:00:00:00:00:01", "PP1(EU)").await;
    world
        .until("a waiting device", |log| {
            log.waiting.last().is_some_and(|waiting| {
                waiting.len() == 1 && waiting[0].reason.contains("`username` and `password`")
            })
        })
        .await;
    assert_eq!(fake.state.lock().expect("state").handshakes, 0);
    world.shutdown().await;
}

#[tokio::test]
async fn a_refused_account_is_shown_and_not_retried_at_once() {
    let fake = fake::start(Kind::Plug, AuthHash::new(USER, "another")).await;
    let world = world(settings(true)).await;
    world.see(&fake, "02:00:00:00:00:01", "PP1(EU)").await;
    world
        .until("the refusal", |log| {
            log.waiting.last().is_some_and(|waiting| {
                waiting.len() == 1 && waiting[0].reason.contains("doesn't accept")
            })
        })
        .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(fake.state.lock().expect("state").handshakes, 1);
    world.shutdown().await;
}

#[tokio::test]
async fn an_expired_session_logs_in_again_without_going_offline() {
    let fake = fake::start(Kind::Plug, auth()).await;
    let world = world(settings(true)).await;
    world.see(&fake, "02:00:00:00:00:01", "PP1(EU)").await;
    world.states(&["02:00:00:00:00:01-power"]).await;
    fake.state.lock().expect("state").expire_next = true;
    fake.state.lock().expect("state").power_mw = 1_000;
    let states = world.states(&["02:00:00:00:00:01-power"]).await;
    assert_eq!(number(&states["02:00:00:00:00:01-power"]), 1.0);
    assert_eq!(fake.state.lock().expect("state").handshakes, 2);
    assert!(
        !world
            .log
            .lock()
            .expect("log")
            .availability
            .contains(&Availability::Unavailable)
    );
    world.shutdown().await;
}

#[tokio::test]
async fn a_fresh_session_that_is_refused_waits_before_logging_in_again() {
    let fake = fake::start(Kind::Plug, auth()).await;
    fake.state.lock().expect("state").refuse_requests = true;
    let world = world(settings(true)).await;
    world.see(&fake, "02:00:00:00:00:01", "PP1(EU)").await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    // A miss, a second's wait, a miss, two seconds' wait: not a login per round trip.
    assert!(fake.state.lock().expect("state").handshakes <= 3);
    world.shutdown().await;
}

#[tokio::test]
async fn a_missed_meter_reading_is_unknown_until_the_next() {
    let fake = fake::start(Kind::Plug, auth()).await;
    let world = world(settings(true)).await;
    world.see(&fake, "02:00:00:00:00:01", "PP1(EU)").await;
    world.states(&["02:00:00:00:00:01-power"]).await;
    let described = world.log.lock().expect("log").entities.len();
    {
        let mut state = fake.state.lock().expect("state");
        state.meter_misses = 1;
        state.power_mw = 2_000;
    }
    // During the miss: unknown, not the last number.
    world
        .until_state("02:00:00:00:00:01-power", Option::is_none)
        .await;
    world
        .until_state("02:00:00:00:00:01-power", |state| {
            state.is_some() && number(state) == 2.0
        })
        .await;
    // The sensors were never described again or taken away.
    {
        let log = world.log.lock().expect("log");
        assert_eq!(log.entities.len(), described);
        assert!(log.removed.is_empty());
    }
    world.shutdown().await;
}

#[tokio::test]
async fn a_missed_outlet_reading_is_unknown_until_the_next() {
    let fake = fake::start(Kind::Strip, auth()).await;
    let world = world(settings(true)).await;
    world.see(&fake, "02:00:00:00:00:03", "PS4(EU)").await;
    world.states(&["02:00:00:00:00:03-outlet2-power"]).await;
    let described = world.log.lock().expect("log").entities.len();
    fake.state.lock().expect("state").outlet_meter_misses = 1;
    world
        .until_state("02:00:00:00:00:03-outlet2-power", Option::is_none)
        .await;
    world
        .until_state("02:00:00:00:00:03-outlet2-power", |state| {
            state.is_some() && number(state) == 2900.0
        })
        .await;
    {
        let log = world.log.lock().expect("log");
        assert_eq!(log.entities.len(), described);
        assert!(log.removed.is_empty());
    }
    world.shutdown().await;
}

#[tokio::test]
async fn a_plug_that_says_it_has_no_meter_loses_its_sensors() {
    let fake = fake::start(Kind::Plug, auth()).await;
    let world = world(settings(true)).await;
    world.see(&fake, "02:00:00:00:00:01", "PP1(EU)").await;
    world.states(&["02:00:00:00:00:01-power"]).await;
    fake.state.lock().expect("state").no_meter = true;
    world
        .until("the meter's sensors removed", |log| log.removed.len() == 5)
        .await;
    let removed: Vec<String> = world
        .log
        .lock()
        .expect("log")
        .removed
        .iter()
        .map(|id| id.as_str().to_owned())
        .collect();
    assert!(removed.contains(&"02:00:00:00:00:01-power".to_owned()));
    assert!(!removed.contains(&"02:00:00:00:00:01-switch".to_owned()));
    world.shutdown().await;
}

#[tokio::test]
async fn a_device_that_stops_answering_goes_unavailable_and_comes_back() {
    let fake = fake::start(Kind::Plug, auth()).await;
    let world = world(settings(true)).await;
    world.see(&fake, "02:00:00:00:00:01", "PP1(EU)").await;
    world.states(&["02:00:00:00:00:01-power"]).await;

    fake.state.lock().expect("state").gone = true;
    world
        .until("unavailable", |log| {
            log.availability.contains(&Availability::Unavailable)
                && log
                    .health
                    .last()
                    .is_some_and(|health| *health != irori_protocol::Health::Running)
        })
        .await;

    fake.state.lock().expect("state").gone = false;
    world
        .until("available again", |log| {
            log.availability.last() == Some(&Availability::Available)
                && log.health.last() == Some(&irori_protocol::Health::Running)
        })
        .await;
    // Every value is reported again after coming back.
    world.states(&["02:00:00:00:00:01-voltage"]).await;
    world.shutdown().await;
}
