//! The protocol against a fake Tapo device on loopback. Nothing here probes the real network.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use irori_protocol::host::{self, Reports};
use irori_protocol::{IncomingCall, ProtocolError, ServiceError, ServiceErrorCode};
use irori_types::{
    Availability, Context, ContextId, DeviceDescription, EntityDescription, Origin, SensorValue,
    Service, ServiceCall, State, StateReport, UniqueId, Waiting,
};
use tokio::sync::{mpsc, watch};

use crate::discover::{Discovery, Found};
use crate::drive;
use crate::fake::{self, Fake, Kind, Write};
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
    asked: std::sync::atomic::AtomicU32,
    /// Kept so `next_call` doesn't see the stop channel close and end the run.
    _stop: watch::Sender<bool>,
    drive: tokio::task::JoinHandle<Result<(), ProtocolError>>,
}

async fn world(settings: Settings) -> World {
    world_with(settings, Duration::from_secs(8)).await
}

/// A world whose commands have `budget` to be taken by the device.
async fn world_with(settings: Settings, budget: Duration) -> World {
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
    let (discovery, incoming) = mpsc::channel(8);
    let drive = tokio::spawn(drive(settings, ctx, incoming, budget));
    World {
        discovery,
        calls: host.calls,
        reports: host.reports,
        log,
        asked: std::sync::atomic::AtomicU32::new(0),
        _stop: host.stop,
        drive,
    }
}

impl World {
    async fn see(&self, fake: &Fake, mac: &str, model: &str) {
        self.see_as(fake, mac, model, "SMART.TAPOPLUG").await;
    }

    async fn see_as(&self, fake: &Fake, mac: &str, model: &str, device_type: &str) {
        let found = Found {
            mac: mac.to_owned(),
            address: fake.address,
            model: model.to_owned(),
            device_type: device_type.to_owned(),
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

    /// Asks for a switch to be turned on or off, without waiting for the answer. The second
    /// value is the call's context, which the report that answers it names.
    async fn ask(
        &self,
        id: &str,
        on: bool,
    ) -> (
        tokio::sync::oneshot::Receiver<Result<(), ServiceError>>,
        ContextId,
    ) {
        // A context per call, as the core gives each its own.
        let count = self
            .asked
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let context = ContextId::try_from(format!("01K5B2Q9A1B2C3D4E5F6G7H8{count:02}").as_str())
            .expect("context");
        let (incoming, answer) = irori_protocol::host::incoming_call(ServiceCall {
            unique_id: UniqueId::try_from(id).expect("id"),
            service: if on {
                Service::SwitchTurnOn
            } else {
                Service::SwitchTurnOff
            },
            context: Context {
                id: context.clone(),
                parent_id: None,
                origin: Origin::System,
            },
        });
        self.calls.send(incoming).await.expect("running");
        (answer, context)
    }

    /// Asks for a switch to be turned on or off and waits for the answer, which has to come
    /// before the core would stop waiting.
    async fn switch(&self, id: &str, on: bool) -> (Result<(), ServiceError>, ContextId) {
        let (answer, context) = self.ask(id, on).await;
        let reply = tokio::time::timeout(Duration::from_secs(10), answer)
            .await
            .expect("answered before the core gives up")
            .expect("answered, not dropped");
        (reply, context)
    }

    /// The next report for `id`, whatever it says.
    async fn report(&self, id: &str) -> StateReport {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(report) = self
                .reports
                .drain()
                .into_iter()
                .find(|report| report.unique_id.as_str() == id)
            {
                return report;
            }
            tokio::select! {
                _ = self.reports.ready() => {}
                _ = tokio::time::sleep_until(deadline) => panic!("{id} was never reported"),
            }
        }
    }

    /// Every report for `id` over the next `wait`.
    async fn reports_for(&self, id: &str, wait: Duration) -> Vec<StateReport> {
        tokio::time::sleep(wait).await;
        self.reports
            .drain()
            .into_iter()
            .filter(|report| report.unique_id.as_str() == id)
            .collect()
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

const PLUG: &str = "02:00:00:00:00:01";
const PLUG_SWITCH: &str = "02:00:00:00:00:01-switch";
const STRIP: &str = "02:00:00:00:00:03";

/// A plug, read once, in a world where the next reading is a second away.
async fn a_plug_in(world: &World) -> Fake {
    let fake = fake::start(Kind::Plug, auth()).await;
    world.see(&fake, PLUG, "PP1(EU)").await;
    world.states(&[PLUG_SWITCH]).await;
    fake
}

fn writes(fake: &Fake) -> Vec<Write> {
    fake.state.lock().expect("state").writes.clone()
}

#[tokio::test]
async fn a_plug_is_switched_off_and_on_and_shows_what_it_then_says() {
    let world = world(settings(true)).await;
    let fake = a_plug_in(&world).await;

    let (reply, call) = world.switch(PLUG_SWITCH, false).await;
    assert_eq!(reply, Ok(()));
    let report = world.report(PLUG_SWITCH).await;
    assert!(!on(&report.state));
    assert_eq!(report.caused_by, Some(call));
    assert!(!fake.state.lock().expect("state").on);

    let (reply, call) = world.switch(PLUG_SWITCH, true).await;
    assert_eq!(reply, Ok(()));
    let report = world.report(PLUG_SWITCH).await;
    assert!(on(&report.state));
    assert_eq!(report.caused_by, Some(call));
    assert!(fake.state.lock().expect("state").on);

    let sent = |on| Write { child: None, on };
    assert_eq!(writes(&fake), [sent(false), sent(true)]);
    world.shutdown().await;
}

#[tokio::test]
async fn a_strips_second_socket_is_switched_through_the_strip() {
    let fake = fake::start(Kind::Strip, auth()).await;
    let world = world(settings(true)).await;
    world.see(&fake, STRIP, "PS4(EU)").await;
    let (first, second) = (
        "02:00:00:00:00:03-outlet1-switch",
        "02:00:00:00:00:03-outlet2-switch",
    );
    world.states(&[first, second]).await;

    let (reply, call) = world.switch(second, false).await;
    assert_eq!(reply, Ok(()));
    let report = world.report(second).await;
    assert!(!on(&report.state));
    assert_eq!(report.caused_by, Some(call));
    // The socket in position 2, by its own id, and only that one.
    assert_eq!(
        writes(&fake),
        [Write {
            child: Some("8022AAAA".to_owned()),
            on: false
        }]
    );
    {
        let state = fake.state.lock().expect("state");
        assert!(!state.outlets[0].on);
        assert!(!state.outlets[1].on);
    }

    // The first socket was off and is turned on; the second stays off.
    let (reply, _) = world.switch(first, true).await;
    assert_eq!(reply, Ok(()));
    assert!(on(&world.report(first).await.state));
    {
        let state = fake.state.lock().expect("state");
        assert!(!state.outlets[0].on);
        assert!(state.outlets[1].on);
    }
    world.shutdown().await;
}

#[tokio::test]
async fn a_plug_that_takes_a_command_and_stays_as_it_was_is_shown_as_it_is() {
    let world = world(settings(true)).await;
    let fake = a_plug_in(&world).await;
    fake.state.lock().expect("state").ignore_switch = true;

    let (reply, call) = world.switch(PLUG_SWITCH, false).await;
    assert_eq!(reply, Ok(()));
    // The device's answer to the call: still on, whatever was asked for.
    let report = world.report(PLUG_SWITCH).await;
    assert!(on(&report.state));
    assert_eq!(report.caused_by, Some(call));
    world.shutdown().await;
}

#[tokio::test]
async fn a_refused_command_says_so_and_the_plug_is_shown_as_it_was() {
    let world = world(settings(true)).await;
    let fake = a_plug_in(&world).await;
    fake.state.lock().expect("state").refuse_switch = Some(-1301);

    let (reply, _) = world.switch(PLUG_SWITCH, false).await;
    let error = reply.expect_err("refused");
    assert_eq!(error.code, ServiceErrorCode::Failed);
    assert!(error.message.contains("refused"), "{}", error.message);
    assert!(error.message.contains("-1301"), "{}", error.message);
    assert_eq!(writes(&fake).len(), 1);
    assert!(fake.state.lock().expect("state").on);
    // Over the next readings nothing says the plug is off, and nothing names the call.
    for report in world
        .reports_for(PLUG_SWITCH, Duration::from_millis(1500))
        .await
    {
        assert!(on(&report.state));
        assert_eq!(report.caused_by, None);
    }
    world.shutdown().await;
}

#[tokio::test]
async fn a_plug_that_doesnt_answer_in_time_is_an_error_and_is_never_switched_late() {
    let world = world_with(settings(true), Duration::from_millis(300)).await;
    let fake = a_plug_in(&world).await;
    fake.state.lock().expect("state").stall = true;

    // Two commands at once: the first runs out of time at the device, the second behind it.
    let (first, _) = world.ask(PLUG_SWITCH, false).await;
    let (second, _) = world.ask(PLUG_SWITCH, false).await;
    for answer in [first, second] {
        let reply = tokio::time::timeout(Duration::from_secs(2), answer)
            .await
            .expect("answered soon after the time ran out")
            .expect("answered, not dropped");
        let error = reply.expect_err("timed out");
        assert_eq!(error.code, ServiceErrorCode::Unavailable);
    }

    // The plug answers again. Neither command is sent now that its caller has been told no.
    fake.state.lock().expect("state").stall = false;
    for report in world
        .reports_for(PLUG_SWITCH, Duration::from_millis(2500))
        .await
    {
        assert!(on(&report.state));
        assert_eq!(report.caused_by, None);
    }
    assert_eq!(writes(&fake), []);
    assert!(fake.state.lock().expect("state").on);
    world.shutdown().await;
}

#[tokio::test]
async fn a_plug_that_cant_be_reached_is_an_error_and_nothing_is_reported() {
    let world = world(settings(true)).await;
    let fake = a_plug_in(&world).await;
    fake.state.lock().expect("state").gone = true;

    let (reply, _) = world.switch(PLUG_SWITCH, false).await;
    let error = reply.expect_err("unreachable");
    assert_eq!(error.code, ServiceErrorCode::Unavailable);
    assert!(
        error.message.contains("can't be reached"),
        "{}",
        error.message
    );
    assert_eq!(writes(&fake), []);
    // Nothing says it turned off: it is left as last read, until it goes unavailable.
    for report in world
        .reports_for(PLUG_SWITCH, Duration::from_millis(1500))
        .await
    {
        assert_ne!(
            report.state.as_ref().map(|_| on(&report.state)),
            Some(false)
        );
    }
    world.shutdown().await;
}

#[tokio::test]
async fn a_command_on_an_ended_session_logs_in_again_and_is_sent_once() {
    let world = world(settings(true)).await;
    let fake = a_plug_in(&world).await;
    fake.state.lock().expect("state").expire_next = true;

    let (reply, _) = world.switch(PLUG_SWITCH, false).await;
    assert_eq!(reply, Ok(()));
    assert!(!on(&world.report(PLUG_SWITCH).await.state));
    assert_eq!(
        writes(&fake),
        [Write {
            child: None,
            on: false
        }]
    );
    assert_eq!(fake.state.lock().expect("state").handshakes, 2);
    world.shutdown().await;
}

#[tokio::test]
async fn nothing_is_sent_for_a_switch_that_isnt_a_plugs_relay() {
    let world = world(settings(true)).await;
    let plug = a_plug_in(&world).await;
    let strip = fake::start(Kind::Strip, auth()).await;
    world.see(&strip, STRIP, "PS4(EU)").await;
    world.states(&["02:00:00:00:00:03-led"]).await;
    // Something that answers like a plug and says it is another kind of device.
    let other = fake::start(Kind::Plug, auth()).await;
    let other_mac = "02:00:00:00:00:05";
    world
        .see_as(&other, other_mac, "PB1(EU)", "SMART.TAPOBULB")
        .await;
    world.states(&["02:00:00:00:00:05-switch"]).await;

    for (id, reason) in [
        // The LED is shown, not changed.
        ("02:00:00:00:00:01-led", "LED"),
        ("02:00:00:00:00:03-led", "LED"),
        // A strip has no relay of its own, and no such entity.
        ("02:00:00:00:00:03-switch", "no TP-Link device"),
        // A device nobody has seen. Before switching existed every call was refused; one for
        // an entity that isn't there still is.
        ("02:00:00:00:00:09-switch", "no TP-Link device"),
        ("02:00:00:00:00:05-switch", "plugs and power strips only"),
    ] {
        for turn_on in [false, true] {
            let (reply, _) = world.switch(id, turn_on).await;
            let error = reply.expect_err("refused");
            assert_eq!(error.code, ServiceErrorCode::Failed, "{id}");
            assert!(error.message.contains(reason), "{id}: {}", error.message);
        }
    }
    for fake in [&plug, &strip, &other] {
        assert_eq!(writes(fake), []);
    }
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
async fn a_missed_meter_reading_right_after_logging_in_again_keeps_the_sensors() {
    let fake = fake::start(Kind::Plug, auth()).await;
    let world = world(settings(true)).await;
    world.see(&fake, "02:00:00:00:00:01", "PP1(EU)").await;
    world.states(&["02:00:00:00:00:01-power"]).await;
    let described = world.log.lock().expect("log").entities.len();
    {
        // The session ends, and the first reading on the new one misses its meter.
        let mut state = fake.state.lock().expect("state");
        state.expire_next = true;
        state.meter_misses = 1;
        state.power_mw = 3_000;
    }
    world
        .until_state("02:00:00:00:00:01-power", |state| {
            state.is_some() && number(state) == 3.0
        })
        .await;
    assert_eq!(fake.state.lock().expect("state").handshakes, 2);
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
async fn an_outlet_the_strip_stops_listing_is_forgotten_with_what_it_metered() {
    let fake = fake::start(Kind::Strip, auth()).await;
    let world = world(settings(true)).await;
    world.see(&fake, STRIP, "PS4(EU)").await;
    let power = "02:00:00:00:00:03-outlet2-power";
    world.states(&[power]).await;

    // The socket in position 2 goes: its entities are taken away.
    let gone = fake.state.lock().expect("state").outlets.remove(0);
    world
        .until("the outlet's power sensor removed", |log| {
            log.removed.iter().any(|id| id.as_str() == power)
        })
        .await;

    // It comes back while its meter isn't answering. Nothing is remembered of what it metered
    // before it went, so it has its switch and no power sensor until it meters again.
    let described = world.log.lock().expect("log").entities.len();
    {
        let mut state = fake.state.lock().expect("state");
        state.outlet_meter_misses = u32::MAX;
        state.outlets.insert(0, gone);
    }
    world
        .until("the outlet described again", |log| {
            log.entities[described..]
                .iter()
                .any(|entity| entity.unique_id.as_str() == "02:00:00:00:00:03-outlet2-switch")
        })
        .await;
    assert!(
        !world.log.lock().expect("log").entities[described..]
            .iter()
            .any(|entity| entity.unique_id.as_str() == power)
    );
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
