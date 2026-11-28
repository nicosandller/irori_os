//! The protocol against fake monitors on loopback. Nothing here reaches the real network.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use irori_protocol::host::{self, Reports};
use irori_protocol::{
    AvailabilityTarget, Health, IncomingCall, ProtocolError, ServiceError, ServiceErrorCode,
};
use irori_types::{
    Availability, Context, ContextId, DeviceDescription, EntityDescription, Origin, SensorValue,
    Service, ServiceCall, State, UniqueId,
};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

use crate::fake;
use crate::measures::SENSORS;
use crate::monitor::Target;
use crate::{MAX_MONITORS, NO_HOSTS, drive, http};

/// Made-up serial numbers.
const FIRST: &str = "0a1b2c3d4e5f";
const SECOND: &str = "0a1b2c3d4e60";

#[derive(Debug, Default)]
struct Log {
    health: Vec<Health>,
    availability: Vec<(String, Availability)>,
    devices: Vec<DeviceDescription>,
    entities: Vec<EntityDescription>,
}

impl Log {
    fn entity_ids(&self) -> Vec<&str> {
        self.entities
            .iter()
            .map(|entity| entity.unique_id.as_str())
            .collect()
    }

    fn degraded(&self) -> bool {
        matches!(self.health.last(), Some(Health::Degraded(_)))
    }
}

struct World {
    calls: mpsc::Sender<IncomingCall>,
    reports: Reports,
    log: Arc<Mutex<Log>>,
    /// Kept so `next_call` doesn't see the stop channel close and end the run.
    _stop: watch::Sender<bool>,
    drive: tokio::task::JoinHandle<Result<(), ProtocolError>>,
}

/// Faster than the settings allow, so a test doesn't wait a minute for the next reading.
const POLL: Duration = Duration::from_millis(200);

async fn world(targets: Vec<Target>) -> World {
    world_polling(targets, POLL).await
}

async fn world_polling(targets: Vec<Target>, poll: Duration) -> World {
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
                host::Op::SetAvailability(target, availability, reply) => {
                    let device = match target {
                        AvailabilityTarget::Device(id) => id.as_str().to_owned(),
                        AvailabilityTarget::Entities(_) => "entities".to_owned(),
                    };
                    recorded
                        .lock()
                        .expect("log")
                        .availability
                        .push((device, availability));
                    let _ = reply.send(Ok(()));
                }
                host::Op::SetHealth(health) => {
                    recorded.lock().expect("log").health.push(health);
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
                host::Op::Load(_, reply) => {
                    let _ = reply.send(Ok(None));
                }
                host::Op::Store(_, _, reply) => {
                    let _ = reply.send(Ok(()));
                }
            }
        }
    });
    let drive = tokio::spawn(drive(poll, ctx, targets));
    World {
        calls: host.calls,
        reports: host.reports,
        log,
        _stop: host.stop,
        drive,
    }
}

impl World {
    /// Waits until `check` holds for the log, or fails after ten seconds.
    async fn until(&self, what: &str, check: impl Fn(&Log) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
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
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
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
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
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
        self.drive.abort();
        let _ = self.drive.await;
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

fn id(serial: &str, suffix: &str) -> String {
    format!("{serial}-{suffix}")
}

#[tokio::test]
async fn a_monitor_is_described_and_read_and_only_ever_asked_for_its_readings() {
    let fake = fake::start(fake::indoor(FIRST)).await;
    let world = world(vec![fake.target()]).await;

    let every: Vec<String> = SENSORS
        .iter()
        .map(|sensor| id(FIRST, sensor.suffix))
        .collect();
    let wanted: Vec<&str> = every.iter().map(String::as_str).collect();
    let states = world.states(&wanted).await;
    assert_eq!(number(&states[&id(FIRST, "co2")]), 612.0);
    assert_eq!(number(&states[&id(FIRST, "pm1")]), 3.0);
    assert_eq!(number(&states[&id(FIRST, "pm25")]), 7.0);
    assert_eq!(number(&states[&id(FIRST, "pm25_corrected")]), 6.4);
    assert_eq!(number(&states[&id(FIRST, "pm10")]), 8.0);
    assert_eq!(number(&states[&id(FIRST, "pm003_count")]), 442.0);
    assert_eq!(number(&states[&id(FIRST, "temperature")]), 21.87);
    assert_eq!(number(&states[&id(FIRST, "humidity")]), 43.0);
    assert_eq!(number(&states[&id(FIRST, "voc_index")]), 100.0);
    assert_eq!(number(&states[&id(FIRST, "nox_index")]), 1.0);
    assert_eq!(number(&states[&id(FIRST, "signal")]), -52.0);

    {
        let log = world.log.lock().expect("log");
        assert_eq!(log.devices.len(), 1);
        assert_eq!(log.devices[0].unique_id.as_str(), FIRST);
        assert_eq!(log.devices[0].serial_number.as_deref(), Some(FIRST));
        assert_eq!(log.entity_ids(), wanted);
        assert_eq!(log.health, [Health::Running]);
    }

    // A changed value is reported at the next reading; nothing else is.
    fake.set("rco2", json!(701));
    world
        .until_state(&id(FIRST, "co2"), |state| number(state) == 701.0)
        .await;

    // Read-only: the readings are the only thing a monitor is ever asked for.
    let requests = fake.requests();
    assert!(requests.len() >= 2);
    assert!(
        requests
            .iter()
            .all(|line| line == "GET /measures/current HTTP/1.1"),
        "{requests:?}"
    );
    world.shutdown().await;
}

#[tokio::test]
async fn a_reading_the_monitor_omits_or_gives_as_null_is_no_sensor() {
    let mut measures = fake::indoor(FIRST);
    measures["rco2"] = Value::Null;
    measures["pm02"] = json!(0);
    let fake = fake::start(measures).await;
    fake.remove("noxIndex");
    fake.remove("pm02Compensated");
    let world = world(vec![fake.target()]).await;

    let states = world.states(&[&id(FIRST, "signal")]).await;
    assert_eq!(
        number(&states[&id(FIRST, "pm25")]),
        0.0,
        "zero is a reading"
    );
    for absent in ["co2", "nox_index", "pm25_corrected"] {
        assert!(!states.contains_key(&id(FIRST, absent)), "{absent}");
    }
    {
        let log = world.log.lock().expect("log");
        let ids = log.entity_ids();
        assert_eq!(ids.len(), SENSORS.len() - 3, "{ids:?}");
        for absent in ["co2", "nox_index", "pm25_corrected"] {
            assert!(!ids.contains(&id(FIRST, absent).as_str()), "{absent}");
        }
    }

    // A sensor that starts answering later (carbon dioxide takes a while to warm up) appears then.
    fake.set("rco2", json!(540));
    world
        .until_state(&id(FIRST, "co2"), |state| number(state) == 540.0)
        .await;
    assert!(
        world
            .log
            .lock()
            .expect("log")
            .entity_ids()
            .contains(&id(FIRST, "co2").as_str())
    );
    world.shutdown().await;
}

#[tokio::test]
async fn a_reading_that_goes_missing_is_unknown_until_it_is_back() {
    let fake = fake::start(fake::indoor(FIRST)).await;
    let world = world(vec![fake.target()]).await;
    world
        .states(&[&id(FIRST, "co2"), &id(FIRST, "humidity")])
        .await;

    fake.set("rco2", Value::Null);
    fake.remove("rhum");
    world.until_state(&id(FIRST, "co2"), Option::is_none).await;
    // Both went in the same reading; the other's report may already have been drained above.
    fake.set("rco2", json!(655));
    fake.set("rhum", json!(44));
    world
        .until_state(&id(FIRST, "co2"), |state| number(state) == 655.0)
        .await;

    // The device was never marked unavailable for it, and no sensor was described twice.
    let log = world.log.lock().expect("log");
    assert!(log.availability.is_empty(), "{:?}", log.availability);
    assert_eq!(log.devices.len(), 1);
    each_sensor_described_once(&log);
}

fn each_sensor_described_once(log: &Log) {
    let mut ids = log.entity_ids();
    ids.sort_unstable();
    let before = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), before, "a sensor was described twice");
}

#[tokio::test]
async fn an_omitted_reading_is_reported_as_unknown() {
    let fake = fake::start(fake::indoor(FIRST)).await;
    let world = world(vec![fake.target()]).await;
    world.states(&[&id(FIRST, "humidity")]).await;
    fake.remove("rhum");
    world
        .until_state(&id(FIRST, "humidity"), Option::is_none)
        .await;
    world.shutdown().await;
}

#[tokio::test]
async fn a_monitor_that_stops_answering_goes_unavailable_and_comes_back() {
    let fake = fake::start(fake::indoor(FIRST)).await;
    let world = world(vec![fake.target()]).await;
    world.states(&[&id(FIRST, "co2")]).await;

    fake.gone(true);
    world
        .until("unavailable", |log| {
            log.availability.last() == Some(&(FIRST.to_owned(), Availability::Unavailable))
                && log.health.last()
                    == Some(&Health::Degraded(
                        "1 AirGradient monitor isn't answering".to_owned(),
                    ))
        })
        .await;

    fake.gone(false);
    world
        .until("available again", |log| {
            log.availability.last() == Some(&(FIRST.to_owned(), Availability::Available))
                && log.health.last() == Some(&Health::Running)
        })
        .await;
    // Every value is reported again after coming back, changed or not.
    world
        .states(&[&id(FIRST, "co2"), &id(FIRST, "signal")])
        .await;
    assert_eq!(world.log.lock().expect("log").devices.len(), 1);
    world.shutdown().await;
}

#[tokio::test]
async fn one_missed_reading_is_not_yet_unavailable() {
    let fake = fake::start(fake::indoor(FIRST)).await;
    let world = world(vec![fake.target()]).await;
    world.states(&[&id(FIRST, "co2")]).await;

    let before = fake.requests().len();
    fake.gone(true);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while fake.requests().len() == before {
        assert!(tokio::time::Instant::now() < deadline, "never asked again");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    fake.gone(false);
    fake.set("rco2", json!(640));
    world
        .until_state(&id(FIRST, "co2"), |state| number(state) == 640.0)
        .await;
    let log = world.log.lock().expect("log");
    assert!(log.availability.is_empty(), "{:?}", log.availability);
    assert_eq!(log.health, [Health::Running]);
}

#[tokio::test]
async fn a_host_that_never_answers_shows_in_health_and_is_picked_up_when_it_does() {
    let fake = fake::start(fake::indoor(FIRST)).await;
    fake.gone(true);
    let world = world(vec![fake.target()]).await;
    world.until("degraded", Log::degraded).await;
    assert!(world.log.lock().expect("log").devices.is_empty());

    fake.gone(false);
    world.states(&[&id(FIRST, "co2")]).await;
    world
        .until("running", |log| log.health.last() == Some(&Health::Running))
        .await;
    world.shutdown().await;
}

#[tokio::test]
async fn an_answer_that_isnt_a_monitors_json_is_never_a_device() {
    let elsewhere = fake::start(fake::indoor(SECOND)).await;
    let redirect = format!(
        "HTTP/1.1 302 Found\r\nLocation: http://{}/measures/current\r\nContent-Length: 0\r\n\r\n",
        elsewhere.address
    );
    let too_large = format!(
        "{{\"serialno\":\"{FIRST}\",\"model\":\"{}\"}}",
        "x".repeat(http::RESPONSE_CAP)
    );
    let not_a_reading: Vec<Vec<u8>> = vec![
        fake::respond("200 OK", "<html><body>Router login</body></html>"),
        fake::respond("200 OK", "[1,2,3]"),
        fake::respond("200 OK", "{\"rco2\":612}"),
        fake::respond("200 OK", "{\"serialno\":\"../../x\",\"rco2\":612}"),
        fake::respond(
            "500 Internal Server Error",
            &fake::indoor(FIRST).to_string(),
        ),
        redirect.into_bytes(),
        fake::respond("200 OK", &too_large),
        b"SSH-2.0-server\r\n\r\n".to_vec(),
    ];
    for raw in not_a_reading {
        let fake = fake::start(fake::indoor(FIRST)).await;
        fake.raw(Some(raw.clone()));
        let world = world(vec![fake.target()]).await;
        world.until("degraded", Log::degraded).await;
        let shown = String::from_utf8_lossy(&raw[..raw.len().min(60)]).into_owned();
        {
            let log = world.log.lock().expect("log");
            assert!(log.devices.is_empty(), "{shown}");
            assert!(log.entities.is_empty(), "{shown}");
        }
        assert!(world.reports.drain().is_empty(), "{shown}");
        world.shutdown().await;
    }
    // The redirect was never followed.
    assert!(elsewhere.requests().is_empty());
}

#[tokio::test]
async fn an_answer_past_the_size_limit_is_dropped_while_it_arrives() {
    let fake = fake::start(fake::indoor(FIRST)).await;
    // No Content-Length, and more than the limit: the read has to stop on its own.
    let mut raw = b"HTTP/1.1 200 OK\r\n\r\n".to_vec();
    raw.resize(raw.len() + http::RESPONSE_CAP + 1, b' ');
    fake.raw(Some(raw));
    let error = http::get(
        fake.address,
        "127.0.0.1",
        "/measures/current",
        http::EXCHANGE,
    )
    .await
    .expect_err("too large");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("too large"), "{error}");
}

#[tokio::test]
async fn a_monitor_that_never_answers_is_given_up_on_in_time() {
    let fake = fake::start(fake::indoor(FIRST)).await;
    fake.state.lock().expect("state").stalled = true;
    let started = tokio::time::Instant::now();
    let error = http::get(
        fake.address,
        "127.0.0.1",
        "/measures/current",
        Duration::from_millis(200),
    )
    .await
    .expect_err("no answer");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(5));
    // The request did arrive: it is the answer that never came.
    assert_eq!(fake.requests().len(), 1);
}

#[tokio::test]
async fn a_host_that_keeps_changing_its_serial_number_cant_add_devices_without_end() {
    let fake = fake::start(fake::indoor(FIRST)).await;
    fake.state.lock().expect("state").new_serial_each_time = true;
    let world = world_polling(vec![fake.target()], Duration::from_millis(5)).await;
    world
        .until("the limit", |log| log.devices.len() >= MAX_MONITORS)
        .await;
    // Well past the limit, nothing more has been added.
    let asked = fake.requests().len();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while fake.requests().len() < asked + 10 {
        assert!(tokio::time::Instant::now() < deadline, "stopped asking");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(world.log.lock().expect("log").devices.len(), MAX_MONITORS);
    world.shutdown().await;
}

#[tokio::test]
async fn two_known_hosts_that_are_one_monitor_are_one_device() {
    // The same monitor listed twice, as its name and its address would be.
    let fake = fake::start(fake::indoor(FIRST)).await;
    let world = world(vec![fake.target(), fake.target()]).await;
    world.states(&[&id(FIRST, "co2")]).await;
    // Both hosts have been read at least twice by the time a second changed value arrives.
    fake.set("rco2", json!(630));
    world
        .until_state(&id(FIRST, "co2"), |state| number(state) == 630.0)
        .await;
    fake.set("rco2", json!(631));
    world
        .until_state(&id(FIRST, "co2"), |state| number(state) == 631.0)
        .await;
    assert!(fake.requests().len() >= 4);

    let log = world.log.lock().expect("log");
    assert_eq!(log.devices.len(), 1);
    each_sensor_described_once(&log);
    assert_eq!(log.health, [Health::Running]);
}

#[tokio::test]
async fn a_monitor_still_reached_through_one_of_its_hosts_stays_available() {
    // The same monitor behind two known hosts, one of which stops answering.
    let reached = fake::start(fake::indoor(FIRST)).await;
    let lost = fake::start(fake::indoor(FIRST)).await;
    let world = world(vec![reached.target(), lost.target()]).await;
    world.states(&[&id(FIRST, "co2")]).await;

    lost.gone(true);
    world.until("degraded", Log::degraded).await;
    reached.set("rco2", json!(633));
    world
        .until_state(&id(FIRST, "co2"), |state| number(state) == 633.0)
        .await;
    let log = world.log.lock().expect("log");
    assert!(log.availability.is_empty(), "{:?}", log.availability);
    assert_eq!(log.devices.len(), 1);
}

#[tokio::test]
async fn a_different_monitor_at_the_same_address_is_a_different_device() {
    let fake = fake::start(fake::indoor(FIRST)).await;
    let world = world(vec![fake.target()]).await;
    world.states(&[&id(FIRST, "co2")]).await;

    // The address now belongs to another monitor.
    let mut other = fake::indoor(SECOND);
    other["rco2"] = json!(999);
    fake.state.lock().expect("state").measures = other;

    let states = world.states(&[&id(SECOND, "co2")]).await;
    assert_eq!(number(&states[&id(SECOND, "co2")]), 999.0);
    // None of the new monitor's readings went to the old one's sensors.
    assert!(
        states.keys().all(|key| key.starts_with(SECOND)),
        "{:?}",
        states.keys()
    );
    let log = world.log.lock().expect("log");
    let devices: Vec<&str> = log
        .devices
        .iter()
        .map(|device| device.unique_id.as_str())
        .collect();
    assert_eq!(devices, [FIRST, SECOND]);
    assert_eq!(
        log.availability,
        [(FIRST.to_owned(), Availability::Unavailable)]
    );
}

#[tokio::test]
async fn two_monitors_are_two_devices_and_one_going_quiet_leaves_the_other() {
    let first = fake::start(fake::indoor(FIRST)).await;
    let second = fake::start(fake::indoor(SECOND)).await;
    let world = world(vec![first.target(), second.target()]).await;
    world.states(&[&id(FIRST, "co2"), &id(SECOND, "co2")]).await;

    second.gone(true);
    world
        .until("the second is unavailable", |log| {
            log.availability == [(SECOND.to_owned(), Availability::Unavailable)]
        })
        .await;
    first.set("rco2", json!(650));
    world
        .until_state(&id(FIRST, "co2"), |state| number(state) == 650.0)
        .await;
    world.shutdown().await;
}

#[tokio::test]
async fn with_no_monitors_listed_health_says_what_to_do() {
    let world = world(Vec::new()).await;
    world.until("health", |log| !log.health.is_empty()).await;
    assert_eq!(
        world.log.lock().expect("log").health,
        [Health::Degraded(NO_HOSTS.to_owned())]
    );
    world.shutdown().await;
}

#[tokio::test]
async fn a_command_is_refused() {
    let world = world(Vec::new()).await;
    let (incoming, answer) = irori_protocol::host::incoming_call(ServiceCall {
        unique_id: UniqueId::try_from(id(FIRST, "co2")).expect("id"),
        service: Service::ButtonPress,
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

#[test]
fn the_manifest_is_valid_and_asks_only_for_the_local_network() {
    use irori_protocol::Protocol as _;
    let manifest =
        irori_protocol::parse_manifest(crate::AirGradient::MANIFEST).expect("a valid manifest");
    assert_eq!(manifest.extension.id.as_str(), "airgradient");
    let text = crate::AirGradient::MANIFEST;
    let permissions = text.split("[permissions]").nth(1).expect("permissions");
    let asked: Vec<&str> = permissions
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    assert_eq!(asked, ["lan = true"]);
}
