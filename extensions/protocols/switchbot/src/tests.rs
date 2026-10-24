//! The protocol against a fake SwitchBot cloud on a local port.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use irori_cloud::Secret;
use irori_protocol::host::{self, Reports};
use irori_protocol::{AvailabilityTarget, Health, IncomingCall, ProtocolError, ServiceError};
use irori_types::{
    Availability, Context, ContextId, DeviceDescription, LockCode, LockStatus, Origin, SensorValue,
    Service, ServiceCall, State, StateReport, UniqueId, Unmodeled,
};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

use crate::cloud::Cloud;
use crate::fake::{Answer, Fake};
use crate::settings::{Account, Settings};
use crate::{Timing, drive, sign};

const HUB: &str = "AABBCCDDEE01";
const METER: &str = "AABBCCDDEE02";
const LOCK: &str = "AABBCCDDEE03";

const FAST: Timing = Timing {
    recheck: Duration::from_millis(50),
    relist: Duration::from_secs(3600),
    min_gap: Duration::from_millis(20),
    per_device: Duration::ZERO,
    refused: Duration::from_secs(3600),
    heard: Duration::from_secs(600),
};

#[derive(Debug, Default)]
struct Log {
    health: Vec<Health>,
    devices: Vec<DeviceDescription>,
    entities: Vec<UniqueId>,
    removed: Vec<UniqueId>,
    availability: Vec<(AvailabilityTarget, Availability)>,
    unmodeled: Vec<Unmodeled>,
    stored: BTreeMap<String, Value>,
}

struct World {
    calls: mpsc::Sender<IncomingCall>,
    reports: Reports,
    log: Arc<Mutex<Log>>,
    states: BTreeMap<String, StateReport>,
    _stop: watch::Sender<bool>,
    drive: tokio::task::JoinHandle<Result<(), ProtocolError>>,
}

fn account() -> Account {
    Account {
        token: Secret::from("the-token".to_owned()),
        secret: Secret::from("the-secret".to_owned()),
    }
}

fn settings() -> Settings {
    Settings {
        token: Some(account().token),
        secret: Some(account().secret),
        daily_calls: 5_000,
        bluetooth: false,
        bluetooth_adapter: 0,
    }
}

/// Starts the protocol against `fake`, with `stored` as what an earlier run kept.
async fn world(fake: Option<&Fake>, stored: BTreeMap<String, Value>, timing: Timing) -> World {
    world_hearing(fake, stored, timing, None).await
}

/// The same, with `radio` as its Bluetooth.
async fn world_hearing(
    fake: Option<&Fake>,
    stored: BTreeMap<String, Value>,
    timing: Timing,
    radio: Option<mpsc::Receiver<crate::ble::Radio>>,
) -> World {
    let (ctx, host) = host::connect();
    let log = Arc::new(Mutex::new(Log::default()));
    let recorded = Arc::clone(&log);
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
                    log.entities.push(entity.unique_id);
                    let _ = reply.send(Ok(()));
                }
                host::Op::SetAvailability(target, availability, reply) => {
                    log.availability.push((target, availability));
                    let _ = reply.send(Ok(()));
                }
                host::Op::SetHealth(health) => log.health.push(health),
                host::Op::RemoveDevice(id, reply) | host::Op::RemoveEntity(id, reply) => {
                    log.removed.push(id);
                    let _ = reply.send(Ok(()));
                }
                host::Op::SetUnmodeled(unmodeled) => log.unmodeled = unmodeled,
                host::Op::SetWaiting(_)
                | host::Op::SetNetworks(_)
                | host::Op::SetActionOpen(_, _)
                | host::Op::SetAvailableActions(_) => {}
                host::Op::Load(key, reply) => {
                    let _ = reply.send(Ok(stored.get(&key).cloned()));
                }
                host::Op::Store(key, value, reply) => {
                    match value {
                        Some(value) => log.stored.insert(key, value),
                        None => log.stored.remove(&key),
                    };
                    let _ = reply.send(Ok(()));
                }
            }
        }
    });
    let cloud = fake.map(|fake| {
        Cloud::new(
            irori_cloud::client("irori-switchbot-test").expect("a client"),
            fake.base.clone(),
            account(),
        )
    });
    let settings = if cloud.is_some() {
        settings()
    } else {
        Settings {
            token: None,
            secret: None,
            daily_calls: 5_000,
            bluetooth: false,
            bluetooth_adapter: 0,
        }
    };
    let drive = tokio::spawn(drive(settings, ctx, cloud, radio, timing));
    World {
        calls: host.calls,
        reports: host.reports,
        log,
        states: BTreeMap::new(),
        _stop: host.stop,
        drive,
    }
}

impl World {
    async fn until<T>(&self, what: &str, mut check: impl FnMut(&Log) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(found) = check(&self.log.lock().expect("log")) {
                return found;
            }
            assert!(tokio::time::Instant::now() < deadline, "never saw {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn health(&self, containing: &str) {
        self.until(&format!("health saying {containing:?}"), |log| {
            match log.health.last() {
                Some(Health::Degraded(why)) if why.contains(containing) => Some(()),
                _ => None,
            }
        })
        .await;
    }

    /// Waits for a state of the entity `id` that `check` accepts.
    async fn state(
        &mut self,
        id: &str,
        mut check: impl FnMut(&StateReport) -> bool,
    ) -> StateReport {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            for report in self.reports.drain() {
                self.states
                    .insert(report.unique_id.as_str().to_owned(), report);
            }
            if let Some(report) = self.states.get(id)
                && check(report)
            {
                return report.clone();
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "never saw a state for {id}"
            );
            tokio::select! {
                _ = self.reports.ready() => {}
                _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
        }
    }

    async fn ask(&self, entity: &str, service: Service) -> Result<(), ServiceError> {
        let (incoming, answer) = host::incoming_call(ServiceCall {
            unique_id: UniqueId::try_from(entity).expect("an entity id"),
            service,
            context: Context {
                id: context(),
                parent_id: None,
                origin: Origin::System,
            },
        });
        self.calls
            .send(incoming)
            .await
            .expect("the protocol is running");
        tokio::time::timeout(Duration::from_secs(5), answer)
            .await
            .expect("a reply in time")
            .expect("the reply was sent")
    }

    async fn shutdown(self) {
        drop(self._stop);
        drop(self.calls);
        let _ = tokio::time::timeout(Duration::from_secs(2), self.drive).await;
    }
}

fn context() -> ContextId {
    ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9").expect("context")
}

fn listing() -> Answer {
    Answer::ok(json!({
        "deviceList": [
            { "deviceId": METER.to_ascii_lowercase(), "deviceName": "Bedroom", "deviceType": "Meter", "hubDeviceId": HUB },
            { "deviceId": HUB, "deviceName": "Hall hub", "deviceType": "Hub Mini", "hubDeviceId": "000000000000" },
            { "deviceId": LOCK, "deviceName": "Front door", "deviceType": "Smart Lock Pro", "hubDeviceId": HUB },
            { "deviceId": "AABBCCDDEE04", "deviceName": "Desk lamp", "deviceType": "Color Bulb", "hubDeviceId": "000000000000" }
        ],
        "infraredRemoteList": [
            { "deviceId": "01-202310", "deviceName": "Living room TV", "remoteType": "TV", "hubDeviceId": HUB }
        ]
    }))
}

async fn home() -> Fake {
    let fake = Fake::start().await;
    fake.with(|state| {
        state.devices = Some(listing());
        state.status.insert(
            METER.into(),
            Answer::ok(
                json!({ "temperature": 21.5, "humidity": 40, "battery": 90, "version": "V2.1" }),
            ),
        );
        state.status.insert(
            LOCK.into(),
            Answer::ok(json!({ "lockState": "unlocked", "doorState": "closed", "battery": 80 })),
        );
    });
    fake
}

fn lock_status(report: &StateReport) -> Option<LockStatus> {
    match &report.state {
        Some(State::Lock(lock)) => Some(lock.state),
        _ => None,
    }
}

#[tokio::test]
async fn devices_in_the_account_are_described_through_their_hub_and_read() {
    let fake = home().await;
    let mut world = world(Some(&fake), BTreeMap::new(), FAST).await;

    let devices = world
        .until("three devices", |log| {
            (log.devices.len() >= 3).then(|| log.devices.clone())
        })
        .await;
    assert_eq!(
        devices[0].unique_id.as_str(),
        HUB,
        "the hub is described first"
    );
    let meter = devices
        .iter()
        .find(|d| d.unique_id.as_str() == METER)
        .expect("the meter, its id upper case");
    assert_eq!(
        meter.via_device_unique_id.as_ref().map(UniqueId::as_str),
        Some(HUB)
    );
    assert_eq!(meter.manufacturer.as_deref(), Some("SwitchBot"));

    let temperature = world
        .state(&format!("{METER}:temperature"), |r| r.state.is_some())
        .await;
    assert!(matches!(
        temperature.state,
        Some(State::Sensor(ref sensor)) if sensor.value == SensorValue::Number(21.5)
    ));
    assert!(!temperature.replayed);
    let lock = world
        .state(&format!("{LOCK}:main"), |r| lock_status(r).is_some())
        .await;
    assert_eq!(lock_status(&lock), Some(LockStatus::Unlocked));

    // The bulb and the TV remote are listed as found but not usable.
    let unmodeled = world
        .until("two unmodeled", |log| {
            (log.unmodeled.len() == 2).then(|| log.unmodeled.clone())
        })
        .await;
    assert!(
        unmodeled
            .iter()
            .any(|u| u.platform.as_str() == "color_bulb")
    );
    assert!(
        unmodeled
            .iter()
            .any(|u| u.platform.as_str() == "infrared_remote")
    );

    // Every request is signed: the token, the time, a nonce, and their HMAC.
    let request = fake.requests().into_iter().next().expect("a request");
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("the-token")
    );
    let t: u128 = request.headers["t"].parse().expect("t in milliseconds");
    let expected = sign::sign(&account(), t, &request.headers["nonce"]);
    assert_eq!(request.headers.get("sign"), Some(&expected.sign));

    // The hub has nothing to read and isn't polled.
    assert!(!fake.requests().iter().any(|r| r.path.contains(HUB)));
    world
        .until("running", |log| {
            (log.health.last() == Some(&Health::Running)).then_some(())
        })
        .await;
    world.shutdown().await;
}

#[tokio::test]
async fn a_command_goes_out_and_the_device_is_read_again_on_its_behalf() {
    let fake = home().await;
    let mut world = world(Some(&fake), BTreeMap::new(), FAST).await;
    let lock = format!("{LOCK}:main");
    world
        .state(&lock, |r| lock_status(r) == Some(LockStatus::Unlocked))
        .await;

    fake.with(|state| {
        state.status.insert(
            LOCK.into(),
            Answer::ok(json!({ "lockState": "locked", "doorState": "closed", "battery": 80 })),
        );
    });
    world
        .ask(&lock, Service::LockLock(LockCode::default()))
        .await
        .expect("SwitchBot took it");
    let command = fake
        .requests()
        .into_iter()
        .find(|r| r.method == "POST")
        .expect("a command");
    assert_eq!(command.path, format!("/v1.1/devices/{LOCK}/commands"));
    assert_eq!(
        command.body,
        json!({ "commandType": "command", "command": "lock", "parameter": "default" })
    );

    let report = world
        .state(&lock, |r| lock_status(r) == Some(LockStatus::Locked))
        .await;
    assert_eq!(
        report.caused_by,
        Some(context()),
        "the re-read is on behalf of the call"
    );
    world.shutdown().await;
}

#[tokio::test]
async fn an_offline_device_is_unavailable_and_its_command_says_so() {
    let fake = home().await;
    fake.with(|state| {
        state.status.insert(LOCK.into(), Answer::code(161));
        state.commands.insert(LOCK.into(), Answer::code(161));
    });
    let world = world(Some(&fake), BTreeMap::new(), FAST).await;
    world
        .until("the lock unavailable", |log| {
            log.availability
                .iter()
                .any(|(target, availability)| {
                    matches!(target, AvailabilityTarget::Device(id) if id.as_str() == LOCK)
                        && *availability == Availability::Unavailable
                })
                .then_some(())
        })
        .await;
    let error = world
        .ask(
            &format!("{LOCK}:main"),
            Service::LockLock(LockCode::default()),
        )
        .await
        .expect_err("the lock is offline");
    assert_eq!(error, ServiceError::unavailable("the device is offline"));
    world.shutdown().await;
}

#[tokio::test]
async fn a_token_that_never_worked_is_reported_as_refused() {
    let fake = Fake::start().await;
    fake.with(|state| state.devices = Some(Answer::http(401)));
    let world = world(Some(&fake), BTreeMap::new(), FAST).await;
    world.health("refused the token").await;
    world.shutdown().await;
}

#[tokio::test]
async fn a_refusal_after_the_token_worked_today_means_the_calls_ran_out() {
    let fake = Fake::start().await;
    fake.with(|state| state.devices = Some(Answer::http(401)));
    let today = crate::unix_now() / 86_400;
    let ledger = json!({
        "budget": irori_cloud::Budget::new(irori_cloud::Period::Day, 5_000, crate::unix_now()),
        "worked_on": today,
    });
    let world = world(
        Some(&fake),
        BTreeMap::from([("ledger".into(), ledger)]),
        FAST,
    )
    .await;
    world.health("used up").await;
    let stored = world
        .until("the ledger stored", |log| log.stored.get("ledger").cloned())
        .await;
    assert_eq!(
        stored["budget"]["spent"],
        json!(5_000),
        "the day counts as spent"
    );
    world.shutdown().await;
}

#[tokio::test]
async fn without_an_account_it_asks_for_the_token() {
    let world = world(None, BTreeMap::new(), FAST).await;
    world.health("Add the SwitchBot token").await;
    world.shutdown().await;
}

#[tokio::test]
async fn a_restart_on_a_spent_day_still_shows_the_last_readings() {
    // Home Assistant used up the day's calls, and Irori starts again: SwitchBot refuses even the
    // device list until midnight UTC. The devices and readings the last run kept show meanwhile.
    let fake = Fake::start().await;
    fake.with(|state| state.devices = Some(Answer::http(401)));
    let stored = BTreeMap::from([
        (
            "ledger".to_owned(),
            json!({
                "budget": irori_cloud::Budget::new(irori_cloud::Period::Day, 5_000, crate::unix_now()),
                "worked_on": crate::unix_now() / 86_400,
            }),
        ),
        (
            "devices".to_owned(),
            json!([
                {
                    "listed": { "id": METER, "name": "Bedroom", "device_type": "Meter", "hub": HUB },
                    "reading": { "temperature": 19.0 }
                },
                {
                    "listed": { "id": HUB, "name": "Hall hub", "device_type": "Hub Mini", "hub": null },
                    "reading": {}
                }
            ]),
        ),
    ]);
    let mut world = world(Some(&fake), stored, FAST).await;
    let report = world
        .state(&format!("{METER}:temperature"), |r| r.state.is_some())
        .await;
    assert!(report.replayed);
    let devices = world
        .until("both devices", |log| {
            (log.devices.len() == 2).then(|| log.devices.clone())
        })
        .await;
    assert_eq!(
        devices[0].unique_id.as_str(),
        HUB,
        "the hub first, as at a listing"
    );
    world.health("used up").await;
    world.shutdown().await;
}

#[tokio::test]
async fn a_hub_going_offline_takes_its_devices_with_it_and_says_so() {
    let fake = home().await;
    fake.with(|state| {
        state.status.insert(METER.into(), Answer::code(171));
        state.status.insert(LOCK.into(), Answer::code(171));
    });
    let world = world(Some(&fake), BTreeMap::new(), FAST).await;
    let down = |log: &Log, id: &str| {
        log.availability.iter().any(|(target, availability)| {
            matches!(target, AvailabilityTarget::Device(device) if device.as_str() == id)
                && *availability == Availability::Unavailable
        })
    };
    world
        .until("the meter and its hub unavailable", |log| {
            (down(log, METER) && down(log, HUB)).then_some(())
        })
        .await;
    world.health("The hub Hall hub is offline").await;

    // The hub is back: its devices answer again.
    fake.with(|state| {
        state.status.insert(
            METER.into(),
            Answer::ok(json!({ "temperature": 21.5, "humidity": 40, "battery": 90 })),
        );
        state.status.insert(
            LOCK.into(),
            Answer::ok(json!({ "lockState": "locked", "doorState": "closed", "battery": 80 })),
        );
    });
    world
        .until("running again", |log| {
            (log.health.last() == Some(&Health::Running)).then_some(())
        })
        .await;
    world.shutdown().await;
}

#[tokio::test]
async fn a_device_deleted_in_the_app_is_removed_at_the_next_listing() {
    let fake = home().await;
    let world = world(
        Some(&fake),
        BTreeMap::new(),
        Timing {
            relist: Duration::from_millis(200),
            ..FAST
        },
    )
    .await;
    world
        .until("the meter", |log| {
            log.devices
                .iter()
                .any(|d| d.unique_id.as_str() == METER)
                .then_some(())
        })
        .await;
    fake.with(|state| {
        state.devices = Some(Answer::ok(json!({
            "deviceList": [
                { "deviceId": HUB, "deviceName": "Hall hub", "deviceType": "Hub Mini", "hubDeviceId": "000000000000" },
                { "deviceId": LOCK, "deviceName": "Front door", "deviceType": "Smart Lock Pro", "hubDeviceId": HUB }
            ]
        })));
    });
    world
        .until("the meter removed", |log| {
            log.removed
                .iter()
                .any(|id| id.as_str() == METER)
                .then_some(())
        })
        .await;
    world.shutdown().await;
}

/// An advert from a Meter Pro CO2 with `mac`, saying 22.9 °C.
fn meter_advert(mac: &str) -> crate::ble::Advert {
    let mut data: Vec<u8> = (0..6)
        .map(|i| u8::from_str_radix(&mac[i * 2..i * 2 + 2], 16).expect("hex"))
        .collect();
    data.extend([0x3f, 0x4c, 0x09, 0x96, 0x3c, 0x00, 0x1a, 0x04, 0x4d, 0x20]);
    crate::ble::Advert {
        address: Some([0; 6]),
        manufacturer: BTreeMap::from([(crate::ble::advert::COMPANY, data)]),
        service: BTreeMap::from([(
            crate::ble::advert::SERVICE.to_owned(),
            vec![0x35, 0x00, 0x4c],
        )]),
        rssi: Some(-70),
    }
}

#[tokio::test]
async fn a_device_heard_nearby_is_read_from_its_adverts_and_not_polled() {
    let fake = home().await;
    fake.with(|state| {
        let mut list = listing();
        list.body["deviceList"]
            .as_array_mut()
            .expect("a list")
            .push(json!({ "deviceId": "AABBCCDDEE05", "deviceName": "Office", "deviceType": "MeterPro(CO2)", "hubDeviceId": "000000000000" }));
        state.devices = Some(list);
    });
    let (radio, heard) = mpsc::channel(8);
    let mut world = world_hearing(Some(&fake), BTreeMap::new(), FAST, Some(heard)).await;
    world
        .until("the office meter", |log| {
            log.devices
                .iter()
                .any(|d| d.unique_id.as_str() == "AABBCCDDEE05")
                .then_some(())
        })
        .await;
    radio
        .send(crate::ble::Radio::Advert(meter_advert("AABBCCDDEE05")))
        .await
        .expect("listening");
    // A neighbour's meter, not in the account, is theirs.
    radio
        .send(crate::ble::Radio::Advert(meter_advert("FFEEDDCCBB99")))
        .await
        .expect("listening");
    let co2 = world.state("AABBCCDDEE05:co2", |r| r.state.is_some()).await;
    assert!(matches!(
        co2.state,
        Some(State::Sensor(ref sensor)) if sensor.value == SensorValue::Number(1101.0)
    ));
    // The sweep reads every listed device once; the office meter, heard first, isn't asked.
    world
        .state(&format!("{LOCK}:main"), |r| lock_status(r).is_some())
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !fake
            .requests()
            .iter()
            .any(|r| r.path.contains("AABBCCDDEE05")),
        "a device heard nearby isn't polled"
    );
    let neighbour = world
        .log
        .lock()
        .expect("log")
        .devices
        .iter()
        .any(|d| d.unique_id.as_str() == "FFEEDDCCBB99");
    assert!(!neighbour, "a neighbour's device isn't taken");
    world.shutdown().await;
}

#[tokio::test]
async fn without_an_account_bluetooth_alone_finds_and_reads_devices() {
    let (radio, heard) = mpsc::channel(8);
    let mut world = world_hearing(None, BTreeMap::new(), FAST, Some(heard)).await;
    radio
        .send(crate::ble::Radio::Advert(meter_advert("AABBCCDDEE07")))
        .await
        .expect("listening");
    let temperature = world
        .state("AABBCCDDEE07:temperature", |r| r.state.is_some())
        .await;
    assert!(matches!(
        temperature.state,
        Some(State::Sensor(ref sensor)) if sensor.value == SensorValue::Number(22.9)
    ));
    let device = world
        .until("the meter described", |log| log.devices.first().cloned())
        .await;
    assert_eq!(device.model.as_deref(), Some("Meter Pro CO2"));
    world
        .until("running", |log| {
            (log.health.last() == Some(&Health::Running)).then_some(())
        })
        .await;

    // Bluetooth failing, with no account to fall back on, is trouble.
    radio
        .send(crate::ble::Radio::Failed(
            "no Bluetooth adapter found".into(),
        ))
        .await
        .expect("listening");
    world
        .health("Bluetooth isn't working (no Bluetooth adapter found)")
        .await;
    world.shutdown().await;
}
