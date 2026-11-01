//! The protocol against fake cameras on loopback ports. Nothing here probes the network.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use irori_protocol::host::{self, Reports};
use irori_protocol::{Picture, ProtocolError, ServiceError};
use irori_types::{
    Availability, BinarySensorClass, CameraGoToPreset, CameraMove, CameraPreset, CameraStatus,
    Capabilities, Context, ContextId, DeviceDescription, EntityDescription, Origin, Service,
    ServiceCall, State, StateReport, UniqueId, Waiting,
};
use tokio::sync::{mpsc, oneshot, watch};

use crate::discover::Found;
use crate::drive;
use crate::fake::{FakeCamera, JPEG};
use crate::http::Target;
use crate::settings::Settings;

const ID: &str = "05121508-0115-0400-0613-ec71dbc9da16";

/// The extension's store, kept between runs the way the core keeps it.
type Store = Arc<Mutex<BTreeMap<String, serde_json::Value>>>;

#[derive(Debug, Default)]
struct Log {
    devices: Vec<DeviceDescription>,
    entities: Vec<EntityDescription>,
    waiting: Vec<Waiting>,
    availability: Vec<Availability>,
}

struct World {
    discovery: mpsc::Sender<Found>,
    actions: mpsc::Sender<irori_protocol::IncomingAction>,
    reports: Reports,
    log: Arc<Mutex<Log>>,
    /// Also kept so `next` doesn't see the core's end close and stop the run.
    calls: mpsc::Sender<irori_protocol::IncomingCall>,
    _stop: watch::Sender<bool>,
    drive: tokio::task::JoinHandle<Result<(), ProtocolError>>,
}

fn settings(login: Option<(&str, &str)>) -> Settings {
    let cameras = match login {
        Some((username, password)) => serde_json::json!({
            ID: { "username": username, "password": password }
        }),
        None => serde_json::json!({}),
    };
    serde_json::from_value(serde_json::json!({ "cameras": cameras })).expect("settings")
}

async fn world(settings: Settings, store: &Store) -> World {
    let (ctx, host) = host::connect();
    let log = Arc::new(Mutex::new(Log::default()));
    let recorded = Arc::clone(&log);
    let store = Arc::clone(store);
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
                host::Op::SetWaiting(waiting) => {
                    recorded.lock().expect("log").waiting = waiting;
                }
                host::Op::Load(key, reply) => {
                    let value = store.lock().expect("store").get(&key).cloned();
                    let _ = reply.send(Ok(value));
                }
                host::Op::Store(key, value, reply) => {
                    let mut store = store.lock().expect("store");
                    match value {
                        Some(value) => store.insert(key, value),
                        None => store.remove(&key),
                    };
                    let _ = reply.send(Ok(()));
                }
                host::Op::RemoveDevice(_, reply) | host::Op::RemoveEntity(_, reply) => {
                    let _ = reply.send(Ok(()));
                }
                host::Op::SetHealth(_)
                | host::Op::SetUnmodeled(_)
                | host::Op::SetNetworks(_)
                | host::Op::SetActionOpen(_, _)
                | host::Op::SetAvailableActions(_) => {}
            }
        }
    });
    let (discovery, incoming) = mpsc::channel(8);
    let drive = tokio::spawn(drive(settings, ctx, incoming));
    World {
        discovery,
        actions: host.actions,
        reports: host.reports,
        log,
        calls: host.calls,
        _stop: host.stop,
        drive,
    }
}

impl World {
    async fn sees(&self, camera: &FakeCamera) {
        self.discovery
            .send(Found {
                id: ID.to_owned(),
                ip: "127.0.0.1".parse().expect("ip"),
                device_service: Target {
                    port: camera.port,
                    path: "/onvif/device_service".into(),
                },
                name: "Reolink".into(),
                hardware: Some("Reolink Duo 2V PoE".into()),
            })
            .await
            .expect("the protocol is running");
    }

    /// Waits until `check` holds for the log, or fails after a few seconds.
    async fn until(&self, what: &str, check: impl Fn(&Log) -> bool) {
        for _ in 0..200 {
            if check(&self.log.lock().expect("log")) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!(
            "never happened: {what}; log: {:?}",
            self.log.lock().expect("log")
        );
    }

    async fn picture(&self) -> Result<Picture, String> {
        let (action, reply): (_, oneshot::Receiver<Result<Picture, String>>) =
            host::incoming_picture(UniqueId::try_from(format!("camera:{ID}")).expect("id"));
        self.actions.send(action).await.expect("running");
        tokio::time::timeout(Duration::from_secs(5), reply)
            .await
            .expect("an answer in time")
            .expect("an answer")
    }

    async fn call(&self, service: Service) -> Result<(), ServiceError> {
        let (call, reply) = host::incoming_call(ServiceCall {
            unique_id: UniqueId::try_from(format!("camera:{ID}")).expect("id"),
            service,
            context: Context {
                id: ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9").expect("context"),
                parent_id: None,
                origin: Origin::System,
            },
        });
        self.calls.send(call).await.expect("running");
        tokio::time::timeout(Duration::from_secs(5), reply)
            .await
            .expect("an answer in time")
            .expect("an answer")
    }

    /// Waits for a state report that `check` accepts, keeping every report seen.
    async fn report(&self, what: &str, check: impl Fn(&StateReport) -> bool) -> StateReport {
        for _ in 0..200 {
            if let Some(found) = self
                .reports
                .drain()
                .into_iter()
                .find(|report| check(report))
            {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("never reported: {what}");
    }

    async fn stop(self) {
        drop(self.discovery);
        let _ = tokio::time::timeout(Duration::from_secs(2), self.drive).await;
    }
}

#[tokio::test]
async fn a_camera_without_a_login_asks_for_one_and_is_left_alone() {
    let camera = FakeCamera::start("admin", "right", true).await;
    let store = Store::default();
    let world = world(settings(None), &store).await;
    world.sees(&camera).await;
    world
        .until("it waits for a login", |log| !log.waiting.is_empty())
        .await;
    let waiting = world.log.lock().expect("log").waiting.clone();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].unique_id.as_str(), ID, "the device's own handle");
    assert_eq!(waiting[0].reason, "it needs the camera's login");
    let secret = waiting[0].secret.as_ref().expect("a login is asked for");
    assert_eq!(secret.path, ["cameras", ID]);
    assert!(
        secret.username.is_some(),
        "a username as well as a password"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(camera.seen().logins, 0, "nothing was sent with a login");
    assert!(world.log.lock().expect("log").devices.is_empty());
    world.stop().await;
}

#[tokio::test]
async fn a_camera_with_its_login_becomes_a_device_with_pictures() {
    let camera = FakeCamera::start("admin", "right", true).await;
    let store = Store::default();
    let world = world(settings(Some(("admin", "right"))), &store).await;
    world.sees(&camera).await;
    world
        .until("the camera is added", |log| !log.entities.is_empty())
        .await;
    {
        let log = world.log.lock().expect("log");
        let device = &log.devices[0];
        assert_eq!(device.unique_id.as_str(), ID);
        assert_eq!(device.name.as_str(), "Reolink Duo 2V PoE");
        assert_eq!(device.manufacturer.as_deref(), Some("Reolink"));
        let entity = &log.entities[0];
        assert_eq!(entity.unique_id.as_str(), format!("camera:{ID}"));
        assert!(matches!(entity.capabilities, Capabilities::Camera(_)));
        assert!(log.waiting.is_empty());
        assert_eq!(log.availability, [Availability::Available]);
    }
    let reports = world.reports.drain();
    assert!(reports.iter().any(|report| matches!(
        &report.state,
        Some(State::Camera(state)) if state.state == CameraStatus::Idle
    )));

    let picture = world.picture().await.expect("a picture");
    assert_eq!(picture.content_type, "image/jpeg");
    assert_eq!(picture.bytes, JPEG);
    let seen = camera.seen();
    assert_eq!(seen.pictures, 1);
    assert_eq!(seen.refused, 0);
    assert_eq!(
        seen.actions[..3],
        [
            "GetSystemDateAndTime",
            "GetDeviceInformation",
            "GetCapabilities"
        ]
    );
    world.stop().await;
}

/// The lockout rule: a refused login costs one failure, ever, until a different one is typed.
#[tokio::test]
async fn a_refused_login_is_never_sent_again_even_after_a_restart() {
    let camera = FakeCamera::start("admin", "right", true).await;
    let store = Store::default();

    let world = world(settings(Some(("admin", "wrong"))), &store).await;
    // Answering discovery over and over while it's being logged into starts no second attempt.
    for _ in 0..3 {
        world.sees(&camera).await;
    }
    world
        .until("it waits for a different login", |log| {
            log.waiting
                .first()
                .is_some_and(|waiting| waiting.reason == "the camera refused that login")
        })
        .await;
    for _ in 0..3 {
        world.sees(&camera).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(camera.seen().refused, 1, "{:?}", camera.seen());
    assert!(world.log.lock().expect("log").devices.is_empty());
    world.stop().await;

    // Giving another camera its login restarts the extension, with the same wrong one here.
    let world = world_again(settings(Some(("admin", "wrong"))), &store).await;
    world.sees(&camera).await;
    world
        .until("it still waits", |log| !log.waiting.is_empty())
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(camera.seen().refused, 1, "the same login wasn't sent again");
    assert_eq!(camera.seen().logins, 1);
    world.stop().await;

    // A different login is tried, once, and taken.
    let world = world_again(settings(Some(("admin", "right"))), &store).await;
    world.sees(&camera).await;
    world
        .until("the camera is added", |log| !log.entities.is_empty())
        .await;
    assert_eq!(camera.seen().refused, 1);
    assert!(
        store
            .lock()
            .expect("store")
            .get("refused_logins")
            .is_none_or(|value| value == &serde_json::json!({})),
        "an accepted login clears what was refused"
    );
    world.stop().await;
}

async fn world_again(settings: Settings, store: &Store) -> World {
    world(settings, store).await
}

#[tokio::test]
async fn a_camera_without_pictures_says_so() {
    let camera = FakeCamera::start("admin", "right", false).await;
    let store = Store::default();
    let world = world(settings(Some(("admin", "right"))), &store).await;
    world.sees(&camera).await;
    world
        .until("the camera is added", |log| !log.entities.is_empty())
        .await;
    assert_eq!(
        world.picture().await,
        Err("this camera only sends live video".into())
    );
    world.stop().await;
}

#[tokio::test]
async fn a_camera_that_cant_be_reached_spends_no_login() {
    // A port nothing listens on.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("port");
    let port = listener.local_addr().expect("address").port();
    drop(listener);
    let store = Store::default();
    let world = world(settings(Some(("admin", "right"))), &store).await;
    world
        .discovery
        .send(Found {
            id: ID.to_owned(),
            ip: "127.0.0.1".parse().expect("ip"),
            device_service: Target {
                port,
                path: "/onvif/device_service".into(),
            },
            name: "Reolink".into(),
            hardware: None,
        })
        .await
        .expect("running");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        world.log.lock().expect("log").waiting.is_empty(),
        "nothing refused it"
    );
    assert!(
        store.lock().expect("store").is_empty(),
        "nothing remembered as refused"
    );
    world.stop().await;
}

#[tokio::test]
async fn a_camera_s_sensors_follow_what_it_notices() {
    let camera = FakeCamera::start("admin", "right", true).await;
    let store = Store::default();
    let world = world(settings(Some(("admin", "right"))), &store).await;
    world.sees(&camera).await;
    world
        .until("its sensors are added", |log| log.entities.len() == 3)
        .await;
    {
        let log = world.log.lock().expect("log");
        let class = |handle: String| {
            log.entities
                .iter()
                .find(|entity| entity.unique_id.as_str() == handle)
                .map(|entity| match &entity.capabilities {
                    Capabilities::BinarySensor(sensor) => sensor.device_class,
                    other => panic!("not a sensor: {other:?}"),
                })
        };
        assert_eq!(
            class(format!("motion:{ID}")),
            Some(Some(BinarySensorClass::Motion))
        );
        assert_eq!(
            class(format!("person:{ID}")),
            Some(Some(BinarySensorClass::Occupancy))
        );
    }
    let motion = format!("motion:{ID}");
    // A camera that only sends changes still starts with its sensors off, not unknown.
    let person = format!("person:{ID}");
    let report = world
        .report("person starts off", |report| {
            report.unique_id.as_str() == person
        })
        .await;
    assert!(matches!(report.state, Some(State::BinarySensor(state)) if !state.on));
    camera.notice(
        "tns1:RuleEngine/CellMotionDetector/Motion",
        "IsMotion",
        true,
    );
    let report = world
        .report("motion", |report| report.unique_id.as_str() == motion)
        .await;
    assert!(matches!(report.state, Some(State::BinarySensor(state)) if state.on));
    camera.notice(
        "tns1:RuleEngine/CellMotionDetector/Motion",
        "IsMotion",
        false,
    );
    let report = world
        .report("no motion", |report| report.unique_id.as_str() == motion)
        .await;
    assert!(matches!(report.state, Some(State::BinarySensor(state)) if !state.on));

    // Something it didn't list gets a sensor when it's first noticed.
    let vehicle = format!("vehicle:{ID}");
    camera.notice(
        "tns1:RuleEngine/MyRuleDetector/VehicleDetect",
        "State",
        true,
    );
    world
        .report("a vehicle", |report| report.unique_id.as_str() == vehicle)
        .await;
    assert!(
        world
            .log
            .lock()
            .expect("log")
            .entities
            .iter()
            .any(|entity| entity.unique_id.as_str() == vehicle)
    );
    assert_eq!(camera.seen().refused, 0);
    world.stop().await;
}

#[tokio::test]
async fn a_camera_turns_stops_on_its_own_and_goes_to_presets() {
    let camera = FakeCamera::start("admin", "right", true).await;
    let store = Store::default();
    let world = world(settings(Some(("admin", "right"))), &store).await;
    world.sees(&camera).await;
    world
        .until("the camera is added", |log| !log.entities.is_empty())
        .await;
    {
        let log = world.log.lock().expect("log");
        let Capabilities::Camera(camera) = &log.entities[0].capabilities else {
            panic!("not a camera");
        };
        assert!(camera.pan_tilt && camera.presets && !camera.zoom);
    }
    let report = world
        .report("its presets", |report| {
            matches!(&report.state, Some(State::Camera(state))
                if state.presets.as_ref().is_some_and(|presets| !presets.is_empty()))
        })
        .await;
    let Some(State::Camera(state)) = report.state else {
        panic!("a camera's state");
    };
    assert_eq!(
        state.presets,
        Some(vec![
            CameraPreset {
                id: "1".into(),
                name: "Driveway".into()
            },
            CameraPreset {
                id: "2".into(),
                name: "Gate".into()
            },
        ])
    );

    let moving: CameraMove = serde_json::from_value(serde_json::json!({
        "pan": 0.5,
        "seconds": 0.2
    }))
    .expect("a move");
    world
        .call(Service::CameraMove(moving))
        .await
        .expect("it moves");
    assert_eq!(camera.seen().moves, ["0.500,0.000"]);
    assert_eq!(camera.seen().stops, 0, "still moving");
    // A lost "stop" doesn't leave it turning.
    for _ in 0..50 {
        if camera.seen().stops > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(camera.seen().stops, 1, "it stopped on its own");

    world
        .call(Service::CameraGoToPreset(CameraGoToPreset {
            preset: "2".into(),
        }))
        .await
        .expect("it goes there");
    assert_eq!(camera.seen().went_to, ["2"]);
    world.call(Service::CameraStop).await.expect("it stops");
    assert_eq!(camera.seen().stops, 2);
    assert_eq!(camera.seen().refused, 0);
    world.stop().await;
}
