//! The extension against the fake Home Assistant on a local port.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use irori_protocol::host;
use irori_protocol::{
    AvailabilityTarget, Health, IncomingAction, IncomingCall, ProtocolError, ServiceError,
};
use irori_types::{
    Availability, Context, ContextId, DeviceDescription, EntityDescription, LightState,
    LightTurnOn, Origin, Service, ServiceCall, State, StateReport, SuggestedLayout, UniqueId,
    Unmodeled, Waiting,
};
use serde_json::json;
use tokio::sync::{mpsc, watch};

use crate::client::ALLOWED;
use crate::fake::{FakeHa, World};
use crate::settings::Settings;
use crate::{Timing, drive};

const TOKEN: &str = "the-token";

const FAST: Timing = Timing {
    first_retry: Duration::from_millis(50),
    most_retry: Duration::from_millis(200),
    refused_retry: Duration::from_secs(3600),
    settle: Duration::from_millis(50),
    starting_retry: Duration::from_millis(50),
    steady: Duration::from_secs(3600),
    tick: Duration::from_millis(10),
};

#[derive(Debug, Default)]
struct Log {
    health: Vec<Health>,
    devices: Vec<DeviceDescription>,
    entities: Vec<EntityDescription>,
    removed: Vec<UniqueId>,
    availability: Vec<(AvailabilityTarget, Availability)>,
    unmodeled: Vec<Unmodeled>,
    waiting: Vec<Waiting>,
    layout: Option<SuggestedLayout>,
}

struct Running {
    calls: mpsc::Sender<IncomingCall>,
    /// Kept open: a closed channel reads as the core going away.
    _actions: mpsc::Sender<IncomingAction>,
    reports: host::Reports,
    log: Arc<Mutex<Log>>,
    states: BTreeMap<String, Vec<StateReport>>,
    stop: watch::Sender<bool>,
    drive: tokio::task::JoinHandle<Result<(), ProtocolError>>,
}

/// A small home: two floors, a kitchen with a dimmable light (a Hue bulb, reached through its
/// bridge) and a temperature sensor, and a speaker's text-to-speech, which Irori has no kind for.
fn home() -> World {
    let mut world = World::new(TOKEN);
    world
        .floor("ground", "Ground floor", 0)
        .floor("up", "Upstairs", 1)
        .area("kitchen", "Kitchen", Some("ground"))
        .area("bedroom", "Bedroom", Some("up"))
        .device(json!({ "id": "hue-bridge", "name": "Hue Bridge", "manufacturer": "Signify", "area_id": null }))
        .device(json!({
            "id": "bulb", "name": "Kitchen light", "name_by_user": null, "manufacturer": "Signify",
            "model": "LCA001", "area_id": "kitchen", "via_device_id": "hue-bridge",
            "connections": [["zigbee", "00:17:88:01:0b:ad:be:ef"]], "serial_number": null,
        }))
        .device(json!({ "id": "speaker", "name": "Kitchen speaker", "area_id": "kitchen" }))
        .entity("e-light", "light.kitchen", Some("bulb"), "off", json!({ "supported_color_modes": ["brightness"] }))
        .entity("e-temp", "sensor.kitchen_temperature", Some("bulb"), "21.5", json!({ "device_class": "temperature", "unit_of_measurement": "°C", "state_class": "measurement" }))
        .entity("e-player", "tts.kitchen", Some("speaker"), "unknown", json!({}));
    world
}

fn settings(fake: &FakeHa) -> Settings {
    serde_json::from_value(json!({ "url": fake.url, "token": TOKEN })).expect("settings")
}

fn context(n: u8) -> ContextId {
    ContextId::try_from(format!("01J00000000000000000000{n:03}").as_str()).expect("a ULID")
}

async fn world(fake: &FakeHa) -> Running {
    world_with(settings(fake)).await
}

async fn world_with(settings: Settings) -> Running {
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
                    log.entities.push(entity);
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
                host::Op::SetWaiting(waiting) => log.waiting = waiting,
                host::Op::SuggestLayout(layout) => log.layout = Some(layout),
                host::Op::SetNetworks(_)
                | host::Op::SetActionOpen(_, _)
                | host::Op::SetAvailableActions(_) => {}
                host::Op::Load(_, reply) => {
                    let _ = reply.send(Ok(None));
                }
                host::Op::Store(_, _, reply) => {
                    let _ = reply.send(Ok(()));
                }
            }
        }
    });
    let drive = tokio::spawn(drive(settings, ctx, FAST));
    Running {
        calls: host.calls,
        _actions: host.actions,
        reports: host.reports,
        log,
        states: BTreeMap::new(),
        stop: host.stop,
        drive,
    }
}

impl Running {
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

    fn collect(&mut self) {
        for report in self.reports.drain() {
            self.states
                .entry(report.unique_id.as_str().to_owned())
                .or_default()
                .push(report);
        }
    }

    /// Waits for a report of the entity `id` that `check` accepts.
    async fn report(
        &mut self,
        id: &str,
        mut check: impl FnMut(&StateReport) -> bool,
    ) -> StateReport {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            self.collect();
            if let Some(report) = self
                .states
                .get(id)
                .and_then(|all| all.iter().rev().find(|r| check(r)))
            {
                return report.clone();
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "never saw a report of {id}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn ask(&self, entity: &str, service: Service, n: u8) -> Result<(), ServiceError> {
        let (incoming, answer) = host::incoming_call(ServiceCall {
            unique_id: UniqueId::try_from(entity).expect("an id"),
            service,
            context: Context {
                id: context(n),
                parent_id: None,
                origin: Origin::System,
            },
        });
        self.calls.send(incoming).await.expect("running");
        tokio::time::timeout(Duration::from_secs(5), answer)
            .await
            .expect("an answer in time")
            .expect("an answer")
    }

    async fn running(&self) {
        self.until("health running", |log| {
            (log.health.last() == Some(&Health::Running)).then_some(())
        })
        .await;
    }

    async fn stop(self) {
        let _ = self.stop.send(true);
        let ended = tokio::time::timeout(Duration::from_secs(5), self.drive)
            .await
            .expect("stopped in time")
            .expect("didn't panic");
        assert!(ended.is_ok(), "{ended:?}");
    }
}

fn on(report: &StateReport) -> Option<bool> {
    match &report.state {
        Some(State::Light(LightState { on, .. })) => Some(*on),
        _ => None,
    }
}

#[tokio::test]
async fn brings_over_devices_rooms_and_states() {
    let fake = FakeHa::start(home()).await.expect("a fake");
    let mut world = world(&fake).await;
    world.running().await;
    let devices = world
        .until("devices", |log| {
            (log.devices.len() == 2).then(|| log.devices.clone())
        })
        .await;
    // The bridge first, then the bulb that's reached through it; the speaker has nothing that
    // comes over, so it doesn't either.
    assert_eq!(devices[0].unique_id.as_str(), "hue-bridge");
    assert_eq!(devices[1].unique_id.as_str(), "bulb");
    assert_eq!(
        devices[1]
            .via_device_unique_id
            .as_ref()
            .map(UniqueId::as_str),
        Some("hue-bridge")
    );
    assert_eq!(
        devices[1].suggested_area.as_ref().map(|a| a.as_str()),
        Some("Kitchen")
    );
    assert_eq!(devices[1].connections.len(), 1);
    assert_eq!(devices[1].connections[0].value(), "001788010badbeef");

    let entities = world
        .until("entities", |log| {
            (log.entities.len() == 2).then(|| log.entities.clone())
        })
        .await;
    assert_eq!(entities[0].unique_id.as_str(), "e-light");
    assert_eq!(
        entities[0].device_unique_id.as_ref().map(UniqueId::as_str),
        Some("bulb")
    );

    let layout = world.until("a layout", |log| log.layout.clone()).await;
    let floors: Vec<_> = layout
        .floors
        .iter()
        .map(|f| f.name.as_str().to_owned())
        .collect();
    assert_eq!(floors, ["Ground floor", "Upstairs"]);
    let areas: Vec<_> = layout
        .areas
        .iter()
        .map(|a| {
            (
                a.name.as_str().to_owned(),
                a.floor.as_ref().map(|f| f.as_str().to_owned()),
            )
        })
        .collect();
    assert_eq!(
        areas,
        [
            ("Kitchen".to_owned(), Some("Ground floor".to_owned())),
            ("Bedroom".to_owned(), Some("Upstairs".to_owned()))
        ]
    );

    let unmodeled = world
        .until("unmodeled", |log| {
            (!log.unmodeled.is_empty()).then(|| log.unmodeled.clone())
        })
        .await;
    assert_eq!(unmodeled[0].platform.as_str(), "tts");
    assert!(unmodeled[0].reason.is_none());

    // What's already there is a replay, not something happening now.
    let light = world.report("e-light", |r| on(r) == Some(false)).await;
    assert!(light.replayed);
    world
        .until("available", |log| {
            log.availability
                .iter()
                .any(|(target, availability)| {
                    *availability == Availability::Available
                        && matches!(target, AvailabilityTarget::Entities(ids) if ids.len() == 2)
                })
                .then_some(())
        })
        .await;

    fake.set_state("light.kitchen", "on", json!({ "brightness": 77 }));
    let light = world.report("e-light", |r| on(r) == Some(true)).await;
    assert!(!light.replayed);
    assert_eq!(light.caused_by, None);
    world.stop().await;
}

#[tokio::test]
async fn a_command_becomes_a_service_call_and_its_change_says_so() {
    let fake = FakeHa::start(home()).await.expect("a fake");
    let mut world = world(&fake).await;
    world.running().await;
    world.report("e-light", |r| on(r) == Some(false)).await;
    let answer = world
        .ask(
            "e-light",
            Service::LightTurnOn(LightTurnOn {
                brightness: Some(128),
                color_temp_kelvin: None,
                rgb: None,
            }),
            1,
        )
        .await;
    assert_eq!(answer, Ok(()));
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["domain"], "light");
    assert_eq!(calls[0]["service"], "turn_on");
    assert_eq!(calls[0]["target"], json!({ "entity_id": "light.kitchen" }));
    assert_eq!(calls[0]["service_data"], json!({ "brightness": 128 }));
    // The fake sends the change before its answer, as Home Assistant does: it's reported again
    // once the answer says which command caused it.
    let light = world.report("e-light", |r| r.caused_by.is_some()).await;
    assert_eq!(light.caused_by, Some(context(1)));
    assert_eq!(on(&light), Some(true));

    // A command of another kind is never sent.
    assert!(
        world
            .ask("e-light", Service::SwitchTurnOn, 2)
            .await
            .is_err()
    );
    assert!(
        world
            .ask("no-such-entity", Service::LightTurnOff, 3)
            .await
            .is_err()
    );
    assert_eq!(fake.calls().len(), 1);
    world.stop().await;
}

#[tokio::test]
async fn a_value_that_did_not_change_is_not_reported_again() {
    let fake = FakeHa::start(home()).await.expect("a fake");
    let mut world = world(&fake).await;
    world.running().await;
    world.report("e-temp", |_| true).await;
    // Only an attribute changes; the value Irori sees is the same.
    fake.set_state(
        "sensor.kitchen_temperature",
        "21.5",
        json!({ "friendly_name": "Renamed" }),
    );
    fake.set_state("sensor.kitchen_temperature", "22.0", json!({}));
    world.report("e-temp", |r| matches!(&r.state, Some(State::Sensor(s)) if s.value == irori_types::SensorValue::Number(22.0))).await;
    let reports = world.states["e-temp"].len();
    assert_eq!(reports, 2, "{:?}", world.states["e-temp"]);
    world.stop().await;
}

#[tokio::test]
async fn a_sensor_that_changes_fast_reports_about_once_a_second() {
    let fake = FakeHa::start(home()).await.expect("a fake");
    let mut world = world(&fake).await;
    world.running().await;
    world.report("e-temp", |_| true).await;
    let start = tokio::time::Instant::now();
    for n in 0..50 {
        fake.set_state(
            "sensor.kitchen_temperature",
            &format!("{}", 30 + n),
            json!({}),
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    // 1.5 s of changes, 50 of them: two or three reports at most, the last one the newest.
    world.report("e-temp", |r| matches!(&r.state, Some(State::Sensor(s)) if s.value == irori_types::SensorValue::Number(79.0))).await;
    let elapsed = start.elapsed();
    let reports = world.states["e-temp"].len() - 1;
    let most = elapsed.as_secs() as usize + 2;
    assert!(reports <= most, "{reports} reports in {elapsed:?}");
    world.stop().await;
}

/// Rule 5: whatever happens, only the allowed messages reach Home Assistant.
#[tokio::test]
async fn nothing_but_reads_subscriptions_and_commands_ever_reaches_home_assistant() {
    let fake = FakeHa::start(home()).await.expect("a fake");
    let mut world = world(&fake).await;
    world.running().await;
    world.report("e-light", |_| true).await;
    world
        .ask("e-light", Service::LightTurnOff, 4)
        .await
        .expect("a call");
    // A registry change: read again, and follow a new entity.
    fake.with_world(|w| {
        w.entity("e-switch", "switch.kettle", Some("bulb"), "on", json!({}));
    });
    fake.registry_changed("entity_registry_updated");
    world
        .until("the new entity", |log| {
            log.entities
                .iter()
                .any(|e| e.unique_id.as_str() == "e-switch")
                .then_some(())
        })
        .await;
    // And a reconnect.
    fake.drop_connections();
    world
        .until("a second connection", |_| {
            (fake.connections() >= 2).then_some(())
        })
        .await;
    world.running().await;
    world.stop().await;
    let types = fake.received_types();
    for kind in &types {
        assert!(
            ALLOWED.contains(&kind.as_str()),
            "{kind} was sent: {types:?}"
        );
    }
    assert!(types.iter().any(|t| t == "unsubscribe_events"), "{types:?}");
    assert_eq!(types.iter().filter(|t| *t == "call_service").count(), 1);
    // The token never appears in what was recorded (the fake blanks it), and was only sent once
    // per connection, to sign in.
    assert_eq!(types.iter().filter(|t| *t == "auth").count(), 2);
}

#[tokio::test]
async fn losing_home_assistant_marks_everything_unavailable_until_it_is_back() {
    let fake = FakeHa::start(home()).await.expect("a fake");
    let mut world = world(&fake).await;
    world.running().await;
    world.report("e-light", |_| true).await;
    let entities = |log: &Log, availability| {
        log.availability
            .iter()
            .filter(move |(target, a)| {
                *a == availability
                    && matches!(target, AvailabilityTarget::Entities(ids) if ids.len() == 2)
            })
            .count()
    };
    world
        .until("available", |log| {
            (entities(log, Availability::Available) == 1).then_some(())
        })
        .await;
    fake.drop_connections();
    world
        .until("unavailable", |log| {
            (entities(log, Availability::Unavailable) == 1).then_some(())
        })
        .await;
    world
        .until("available again", |log| {
            (entities(log, Availability::Available) == 2).then_some(())
        })
        .await;
    // Nothing was described twice: the reconnect only sent what changed, which was nothing.
    let described = {
        let log = world.log.lock().expect("log");
        (log.devices.len(), log.entities.len())
    };
    assert_eq!(described, (2, 2));
    world.stop().await;
}

#[tokio::test]
async fn an_entity_that_stops_qualifying_is_removed() {
    let fake = FakeHa::start(home()).await.expect("a fake");
    let world = world(&fake).await;
    world.running().await;
    world
        .until("entities", |log| (log.entities.len() == 2).then_some(()))
        .await;
    fake.with_world(|w| {
        for entity in &mut w.entities {
            if entity["id"] == "e-temp" {
                entity["hidden_by"] = json!("user");
            }
        }
    });
    fake.registry_changed("entity_registry_updated");
    world
        .until("the removal", |log| {
            log.removed
                .iter()
                .any(|id| id.as_str() == "e-temp")
                .then_some(())
        })
        .await;
    world.stop().await;
}

/// An integration reloading drops its states for a moment. Read then, its entities are still in
/// Home Assistant: they stay in the home, unavailable, rather than being removed with the room
/// and name they were given there.
#[tokio::test]
async fn an_entity_without_a_state_for_a_moment_stays_unavailable() {
    let fake = FakeHa::start(home()).await.expect("a fake");
    let world = world(&fake).await;
    world.running().await;
    world
        .until("entities", |log| (log.entities.len() == 2).then_some(()))
        .await;
    fake.with_world(|w| w.states.remove("light.kitchen"));
    fake.registry_changed("device_registry_updated");
    world
        .until("the light unavailable", |log| {
            log.availability
                .iter()
                .any(|(target, a)| {
                    *a == Availability::Unavailable
                        && matches!(target, AvailabilityTarget::Entities(ids)
                            if ids.iter().any(|id| id.as_str() == "e-light"))
                })
                .then_some(())
        })
        .await;
    {
        let log = world.log.lock().expect("log");
        assert!(log.removed.is_empty(), "removed {:?}", log.removed);
        // Nor described again: it's what it was.
        assert_eq!(log.entities.len(), 2);
    }
    world.stop().await;
}

#[tokio::test]
async fn a_token_that_cannot_read_the_registries_waits_for_an_administrators() {
    let mut home = home();
    home.admin = false;
    let fake = FakeHa::start(home).await.expect("a fake");
    let world = world(&fake).await;
    let waiting = world
        .until("a waiting item", |log| {
            (!log.waiting.is_empty()).then(|| log.waiting.clone())
        })
        .await;
    assert_eq!(waiting[0].unique_id.as_str(), crate::NOT_ADMIN);
    assert!(
        waiting[0].reason.contains("administrator"),
        "{}",
        waiting[0].reason
    );
    let secret = waiting[0].secret.as_ref().expect("asks for a token");
    assert_eq!(secret.path, ["token"]);
    assert!(matches!(
        world.log.lock().expect("log").health.last(),
        Some(Health::Degraded(_))
    ));
    world.stop().await;
}

#[tokio::test]
async fn a_home_assistant_still_starting_is_waited_for_before_anything_comes_over() {
    let mut home = home();
    home.running = Some("STARTING".into());
    let fake = FakeHa::start(home).await.expect("a fake");
    let world = world(&fake).await;
    world
        .until("health saying so", |log| match log.health.last() {
            Some(Health::Degraded(why)) if why.contains("starting") => Some(()),
            _ => None,
        })
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    {
        let log = world.log.lock().expect("log");
        assert!(log.entities.is_empty() && log.removed.is_empty(), "{log:?}");
    }
    fake.with_world(|w| w.running = None);
    world.running().await;
    world
        .until("entities", |log| (log.entities.len() == 2).then_some(()))
        .await;
    world.stop().await;
}

#[tokio::test]
async fn a_refused_token_says_so_and_commands_meanwhile_are_unavailable() {
    let fake = FakeHa::start(home()).await.expect("a fake");
    let mut settings = settings(&fake);
    settings.token = irori_cloud::Secret::from("wrong".to_owned());
    let world = world_with(settings).await;
    world
        .until("health saying so", |log| match log.health.last() {
            Some(Health::Degraded(why)) if why.contains("refused the access token") => Some(()),
            _ => None,
        })
        .await;
    let answer = world.ask("e-light", Service::LightTurnOff, 5).await;
    assert!(
        matches!(answer, Err(ref e) if e.message.contains("isn't connected")),
        "{answer:?}"
    );
    assert!(fake.calls().is_empty());
    world.stop().await;
}

#[tokio::test]
async fn nothing_there_is_retried_with_a_growing_wait() {
    // A port nothing listens on: bind one, then let it go.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("a port")
        .local_addr()
        .expect("addr")
        .port();
    let settings: Settings = serde_json::from_value(
        json!({ "url": format!("http://127.0.0.1:{port}"), "token": TOKEN }),
    )
    .expect("settings");
    let world = world_with(settings).await;
    world
        .until("two tries", |log| {
            (log.health
                .iter()
                .filter(|h| matches!(h, Health::Degraded(_)))
                .count()
                >= 2)
                .then_some(())
        })
        .await;
    world.stop().await;
}
