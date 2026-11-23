//! Many registry changes at once, as when an extension describes a big network: the home is
//! read again a handful of times, not once per change, while flows keep firing and the
//! extension still stops in time (issue #185).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use irori_engine_automations::store::Store;
use irori_engine_automations::{Service, serve};
use irori_protocol::{FromExt, ToExt};
use irori_types::{
    Availability, BinarySensorCapabilities, BinarySensorState, Capabilities, Context, ContextId,
    Entity, EntityId, EntityState, LightCapabilities, LightState, Name, Origin, SensorCapabilities,
    SensorState, SensorValue, SensorValueType, State, Timestamp, UniqueId,
};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
use tokio::sync::mpsc;
use tokio::time::Instant;

const MOTION: &str = "binary_sensor.demo_movement_motion";
const LUX: &str = "sensor.demo_luminosity_illuminance";
const OCCUPANCY: &str = "binary_sensor.demo_mmwave_occupancy";
const LIGHT: &str = "light.demo_hall_light";
/// The trigger of a flow whose entity only arrives with the burst.
const LATE: &str = "binary_sensor.bulk_late_motion";
/// About what a big bridged home or Zigbee network describes when it starts.
const BURST: usize = 2_000;

fn id(s: &str) -> EntityId {
    s.parse().expect("an entity id")
}

fn stamp(seconds: i64) -> Timestamp {
    let at: Timestamp = "2026-09-29T20:00:00Z".parse().expect("a time");
    Timestamp::from_jiff(at.as_jiff() + jiff::SignedDuration::from_secs(seconds))
}

fn state(entity: &str, value: State, at: i64) -> EntityState {
    EntityState {
        entity_id: id(entity),
        availability: Availability::Available,
        state: Some(value),
        attributes: BTreeMap::default(),
        last_changed: stamp(at),
        last_updated: stamp(at),
        last_reported: stamp(at),
        context: Context {
            id: ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9").expect("valid"),
            parent_id: None,
            origin: Origin::System,
        },
    }
}

fn binary(entity: &str, on: bool, at: i64) -> EntityState {
    state(entity, State::BinarySensor(BinarySensorState { on }), at)
}

fn entity(entity: &str, capabilities: Capabilities) -> Entity {
    Entity {
        id: id(entity),
        protocol: "demo".parse().expect("valid"),
        unique_id: UniqueId::try_from(entity.replace('.', "-")).expect("valid"),
        name: Name::try_from("x").expect("valid"),
        device_id: None,
        area_id: None,
        capabilities,
        entity_category: None,
    }
}

fn flag() -> Capabilities {
    Capabilities::BinarySensor(BinarySensorCapabilities { device_class: None })
}

fn number() -> Capabilities {
    Capabilities::Sensor(SensorCapabilities {
        value_type: SensorValueType::Number,
        device_class: None,
        unit: None,
        state_class: None,
        options: Vec::new(),
    })
}

/// What the fake core holds and counts, shared with the test.
#[derive(Clone, Default)]
struct Home {
    entities: Arc<Mutex<Vec<Entity>>>,
    states: Arc<Mutex<Vec<EntityState>>>,
    /// How many times the registry was asked for.
    reads: Arc<AtomicUsize>,
    /// How long each registry answer takes, as a big home's does.
    slow: Arc<Mutex<Duration>>,
    /// Registry questions go unanswered, as when the core is busy stopping.
    silent: Arc<AtomicBool>,
}

impl Home {
    /// The demo hallway: motion, occupancy, a dark room and a light.
    fn hallway() -> Self {
        let home = Self::default();
        home.entities.lock().expect("entities").extend([
            entity(MOTION, flag()),
            entity(OCCUPANCY, flag()),
            entity(LUX, number()),
            entity(
                LIGHT,
                Capabilities::Light(LightCapabilities {
                    brightness: true,
                    color_temp_kelvin: None,
                    rgb: false,
                }),
            ),
        ]);
        home.states.lock().expect("states").extend([
            binary(MOTION, false, 0),
            binary(OCCUPANCY, true, 0),
            state(
                LUX,
                State::Sensor(SensorState {
                    value: SensorValue::Number(8.0),
                }),
                0,
            ),
            state(
                LIGHT,
                State::Light(LightState {
                    on: false,
                    brightness: None,
                    color_mode: None,
                    color_temp_kelvin: None,
                    rgb: None,
                }),
                0,
            ),
        ]);
        home
    }

    /// Another sensor, as an extension describing its network adds one.
    fn add(&self, name: &str, capabilities: Capabilities, value: State) {
        self.entities
            .lock()
            .expect("entities")
            .push(entity(name, capabilities));
        self.states
            .lock()
            .expect("states")
            .push(state(name, value, 0));
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

/// The core's side of the wire: answers every request from `home`, writes the lines `pushes`
/// hands it, and reports each service call it was asked to make.
async fn fake_core(
    stream: tokio::io::DuplexStream,
    home: Home,
    mut pushes: mpsc::UnboundedReceiver<String>,
    calls: mpsc::UnboundedSender<FromExt>,
) {
    let (read, write) = tokio::io::split(stream);
    let write = Arc::new(tokio::sync::Mutex::new(write));
    async fn send(
        write: &tokio::sync::Mutex<tokio::io::WriteHalf<tokio::io::DuplexStream>>,
        line: &str,
    ) {
        let mut write = write.lock().await;
        let _ = write.write_all(line.as_bytes()).await;
        let _ = write.write_all(b"\n").await;
    }
    send(&write, r#"{"type":"hello","settings":{}}"#).await;
    let pusher = {
        let write = Arc::clone(&write);
        tokio::spawn(async move {
            while let Some(line) = pushes.recv().await {
                send(&write, &line).await;
            }
        })
    };
    let mut lines = tokio::io::BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let message: FromExt = serde_json::from_str(&line).expect("a message");
        let (id, value) = match &message {
            FromExt::Subscribe { id, .. } | FromExt::CallService { id, .. } => {
                (*id, serde_json::Value::Null)
            }
            FromExt::GetStates { id } => {
                let states = home.states.lock().expect("states").clone();
                (*id, serde_json::json!(states))
            }
            FromExt::GetRegistry { id } => {
                home.reads.fetch_add(1, Ordering::SeqCst);
                if home.silent.load(Ordering::SeqCst) {
                    continue;
                }
                let slow = *home.slow.lock().expect("slow");
                tokio::time::sleep(slow).await;
                let entities = home.entities.lock().expect("entities").clone();
                (
                    *id,
                    serde_json::json!({ "entities": entities, "timezone": false, "location": false }),
                )
            }
            FromExt::GetReach { id, .. } => (*id, serde_json::json!({ "here": false })),
            _ => continue,
        };
        if matches!(message, FromExt::CallService { .. }) {
            let _ = calls.send(message);
        }
        let reply = ToExt::Answer {
            id,
            value: Some(value),
            error: None,
        };
        send(&write, &serde_json::to_string(&reply).expect("serializes")).await;
    }
    pusher.abort();
}

/// What a test drives: the home, the lines to push, the calls made, and the running extension.
struct Running {
    home: Home,
    push: mpsc::UnboundedSender<String>,
    calls: mpsc::UnboundedReceiver<FromExt>,
    served: tokio::task::JoinHandle<()>,
}

/// Starts the extension on the hallway with its flow, and a second flow on [`LATE`], which the
/// home doesn't have yet.
async fn started(dir: &std::path::Path, home: Home) -> Running {
    let hallway = include_str!("../../../fixtures/types/flow/valid/hallway.json");
    std::fs::create_dir_all(dir.join("flows")).expect("mkdir");
    std::fs::write(dir.join("flows/hallway_motion_light.json"), hallway).expect("wrote");
    std::fs::write(
        dir.join("flows/late_motion_light.json"),
        hallway
            .replace("hallway_motion_light", "late_motion_light")
            .replace(MOTION, LATE),
    )
    .expect("wrote");

    let (core_end, engine_end) = tokio::io::duplex(1 << 20);
    let (push, pushes) = mpsc::unbounded_channel();
    let (calls_tx, calls) = mpsc::unbounded_channel();
    tokio::spawn(fake_core(core_end, home.clone(), pushes, calls_tx));
    let (read, write) = tokio::io::split(engine_end);
    let (_, client, incoming) = irori_protocol::engine::connect(read, write)
        .await
        .expect("connects");
    let store = Store::open(dir.join("flows"), dir.join("data"));
    let (service, answers) = Service::start(client, store).await.expect("starts");
    let served = tokio::task::spawn_local(serve(service, incoming, answers));
    Running {
        home,
        push,
        calls,
        served,
    }
}

fn changed(entity: &str, on: bool, at: i64) -> String {
    serde_json::to_string(&ToExt::StateChanged {
        entity_id: id(entity),
        old_state: Some(Box::new(binary(entity, !on, at - 1))),
        new_state: Box::new(binary(entity, on, at)),
    })
    .expect("serializes")
}

const REGISTRY_CHANGED: &str = r#"{"type":"registry_changed"}"#;

/// The next service call, if one comes within `within`.
async fn call_within(calls: &mut mpsc::UnboundedReceiver<FromExt>, within: Duration) -> bool {
    matches!(
        tokio::time::timeout(within, calls.recv()).await,
        Ok(Some(FromExt::CallService { .. }))
    )
}

/// An extension describes 2,000 entities over about two seconds. The home is read again a
/// handful of times, not 2,000; the hallway's flow fires within a second in the middle of it;
/// and a flow on an entity that came with the burst works once it settles.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_burst_of_registry_changes_is_read_a_handful_of_times() {
    tokio::task::LocalSet::new()
        .run_until(a_burst_of_registry_changes_is_read_a_handful_of_times_locally())
        .await;
}

async fn a_burst_of_registry_changes_is_read_a_handful_of_times_locally() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut running = started(dir.path(), Home::hallway()).await;
    let at_start = running.home.reads();
    let began = Instant::now();

    let chunks = 20;
    for chunk in 0..chunks {
        for i in 0..BURST / chunks {
            let n = chunk * (BURST / chunks) + i;
            if n == BURST / 2 {
                running.home.add(
                    LATE,
                    flag(),
                    State::BinarySensor(BinarySensorState { on: false }),
                );
            } else {
                running.home.add(
                    &format!("sensor.bulk_{n}"),
                    number(),
                    State::Sensor(SensorState {
                        value: SensorValue::Number(1.0),
                    }),
                );
            }
            running
                .push
                .send(REGISTRY_CHANGED.to_owned())
                .expect("push");
        }
        if chunk == chunks / 2 {
            // Motion in the dark hallway, mid-burst: the light is asked to come on, soon.
            running.push.send(changed(MOTION, true, 1)).expect("push");
            let asked = Instant::now();
            assert!(
                call_within(&mut running.calls, Duration::from_secs(1)).await,
                "the hallway flow didn't fire within a second during the burst"
            );
            eprintln!("the hallway flow fired after {:?}", asked.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // While changes keep coming the home is read at most once a second, and once more as
    // they settle: two or three times here, more only if this machine stretched the burst.
    let lasted = began.elapsed();
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let reads = running.home.reads() - at_start;
    let allowed = usize::try_from(lasted.as_secs()).unwrap_or(usize::MAX) + 3;
    eprintln!("{BURST} registry changes over {lasted:?} were read {reads} times");
    assert!(
        (1..=allowed).contains(&reads),
        "{BURST} registry changes over {lasted:?} were read {reads} times, more than {allowed}"
    );

    // Settled: the flow on the entity that came with the burst works.
    running.push.send(changed(LATE, true, 2)).expect("push");
    assert!(
        call_within(&mut running.calls, Duration::from_secs(1)).await,
        "the flow on an entity that came with the burst didn't fire"
    );

    running
        .push
        .send(r#"{"type":"stop"}"#.to_owned())
        .expect("push");
    tokio::time::timeout(Duration::from_secs(5), running.served)
        .await
        .expect("stops in time")
        .expect("joins");
}

/// Told to stop in the middle of a burst whose every read is slow, it stops within the host's
/// five-second grace instead of reading the home once for each change first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn it_stops_in_time_in_the_middle_of_a_burst() {
    tokio::task::LocalSet::new()
        .run_until(it_stops_in_time_in_the_middle_of_a_burst_locally())
        .await;
}

async fn it_stops_in_time_in_the_middle_of_a_burst_locally() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let running = started(dir.path(), Home::hallway()).await;
    *running.home.slow.lock().expect("slow") = Duration::from_millis(20);
    for _ in 0..BURST {
        running
            .push
            .send(REGISTRY_CHANGED.to_owned())
            .expect("push");
    }
    running
        .push
        .send(r#"{"type":"stop"}"#.to_owned())
        .expect("push");
    let told = Instant::now();
    tokio::time::timeout(Duration::from_secs(5), running.served)
        .await
        .expect("stops within the grace")
        .expect("joins");
    eprintln!("stopped after {:?}", told.elapsed());
}

/// Told to stop while the core hasn't answered a re-read, it stops at once rather than waiting
/// out the request's thirty seconds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn it_stops_in_time_while_a_read_is_unanswered() {
    tokio::task::LocalSet::new()
        .run_until(it_stops_in_time_while_a_read_is_unanswered_locally())
        .await;
}

async fn it_stops_in_time_while_a_read_is_unanswered_locally() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let running = started(dir.path(), Home::hallway()).await;
    running.home.silent.store(true, Ordering::SeqCst);
    let before = running.home.reads();
    running
        .push
        .send(REGISTRY_CHANGED.to_owned())
        .expect("push");
    let deadline = Instant::now() + Duration::from_secs(3);
    while running.home.reads() == before {
        assert!(Instant::now() < deadline, "the change was never read");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    running
        .push
        .send(r#"{"type":"stop"}"#.to_owned())
        .expect("push");
    tokio::time::timeout(Duration::from_secs(5), running.served)
        .await
        .expect("stops within the grace")
        .expect("joins");
}

/// An entity a flow waits on is added and reports its first state straight away, before the
/// change would settle: the home is read first, so the flow takes that state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_state_of_a_just_added_entity_reaches_its_flow() {
    tokio::task::LocalSet::new()
        .run_until(the_first_state_of_a_just_added_entity_reaches_its_flow_locally())
        .await;
}

async fn the_first_state_of_a_just_added_entity_reaches_its_flow_locally() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mut running = started(dir.path(), Home::hallway()).await;
    running.home.add(
        LATE,
        flag(),
        State::BinarySensor(BinarySensorState { on: false }),
    );
    running
        .push
        .send(REGISTRY_CHANGED.to_owned())
        .expect("push");
    running.push.send(changed(LATE, true, 1)).expect("push");
    assert!(
        // Any call at all shows it was read first: on the old reading the state is dropped.
        call_within(&mut running.calls, Duration::from_millis(1_500)).await,
        "the flow on the entity just added missed its first state"
    );
    running
        .push
        .send(r#"{"type":"stop"}"#.to_owned())
        .expect("push");
    tokio::time::timeout(Duration::from_secs(5), running.served)
        .await
        .expect("stops in time")
        .expect("joins");
}

async fn stopped(running: Running) {
    running
        .push
        .send(r#"{"type":"stop"}"#.to_owned())
        .expect("push");
    tokio::time::timeout(Duration::from_secs(5), running.served)
        .await
        .expect("stops in time")
        .expect("joins");
}

/// A flow that can't arm because its light has only just arrived takes the motion right behind
/// that news, though the motion sensor itself was always known.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_known_trigger_reaches_a_flow_whose_light_just_arrived() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().expect("a temp dir");
            let home = Home::hallway();
            home.entities
                .lock()
                .expect("entities")
                .retain(|e| e.id != id(LIGHT));
            home.states
                .lock()
                .expect("states")
                .retain(|s| s.entity_id != id(LIGHT));
            let mut running = started(dir.path(), home).await;
            running.home.add(
                LIGHT,
                Capabilities::Light(LightCapabilities {
                    brightness: true,
                    color_temp_kelvin: None,
                    rgb: false,
                }),
                State::Light(LightState {
                    on: false,
                    brightness: None,
                    color_mode: None,
                    color_temp_kelvin: None,
                    rgb: None,
                }),
            );
            running
                .push
                .send(REGISTRY_CHANGED.to_owned())
                .expect("push");
            running.push.send(changed(MOTION, true, 1)).expect("push");
            let fired = call_within(&mut running.calls, Duration::from_millis(1_500)).await;
            stopped(running).await;
            assert!(
                fired,
                "motion right after the light arrived didn't turn it on"
            );
        })
        .await;
}

/// After missed events the home is read before the next state is judged: the light stays off
/// in a hallway that got bright while the change was lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_missed_events_a_gate_reads_the_home_as_it_is() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().expect("a temp dir");
            let mut running = started(dir.path(), Home::hallway()).await;
            for s in running.home.states.lock().expect("states").iter_mut() {
                if s.entity_id == id(LUX) {
                    s.state = Some(State::Sensor(SensorState {
                        value: SensorValue::Number(500.0),
                    }));
                }
            }
            running
                .push
                .send(r#"{"type":"registry_changed","missed":5}"#.to_owned())
                .expect("push");
            running.push.send(changed(MOTION, true, 1)).expect("push");
            let fired = call_within(&mut running.calls, Duration::from_millis(1_500)).await;
            stopped(running).await;
            assert!(
                !fired,
                "the gate read the illuminance from before the missed events"
            );
        })
        .await;
}

/// A flow naming a device that never comes is always waiting. Its busy trigger, in the middle
/// of a burst, must not bring back a reading per change, and must not hold up other flows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_flow_on_a_missing_device_does_not_bring_back_a_read_per_change() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().expect("a temp dir");
            let home = Home::hallway();
            home.entities
                .lock()
                .expect("entities")
                .retain(|e| e.id != id(OCCUPANCY));
            let running = started(dir.path(), home).await;
            let at_start = running.home.reads();
            let began = Instant::now();
            // The hallway flow waits on occupancy, which never comes; its motion sensor chatters.
            for n in 0..BURST {
                running
                    .push
                    .send(REGISTRY_CHANGED.to_owned())
                    .expect("push");
                let at = i64::try_from(n).expect("small") + 1;
                running
                    .push
                    .send(changed(MOTION, n % 2 == 0, at))
                    .expect("push");
                if n % 100 == 99 {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
            tokio::time::sleep(Duration::from_millis(600)).await;
            let lasted = began.elapsed();
            let reads = running.home.reads() - at_start;
            let allowed = usize::try_from(lasted.as_millis() / 100).unwrap_or(usize::MAX) + 2;
            eprintln!("{BURST} changes with a busy waiting trigger: {reads} reads in {lasted:?}");
            stopped(running).await;
            assert!(
                reads <= allowed,
                "{reads} reads in {lasted:?}, more than {allowed}"
            );
        })
        .await;
}

/// A flow can't arm for want of an entity named only in an expression (the hallway's "dark"
/// gate on the lux sensor). It still counts as waiting, so the motion right behind the news
/// that the sensor arrived isn't lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_flow_whose_missing_entity_is_only_in_an_expression_still_waits() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().expect("a temp dir");
            let home = Home::hallway();
            home.entities
                .lock()
                .expect("entities")
                .retain(|e| e.id != id(LUX));
            home.states
                .lock()
                .expect("states")
                .retain(|s| s.entity_id != id(LUX));
            let mut running = started(dir.path(), home).await;
            running.home.add(
                LUX,
                number(),
                State::Sensor(SensorState {
                    value: SensorValue::Number(10.0),
                }),
            );
            running
                .push
                .send(REGISTRY_CHANGED.to_owned())
                .expect("push");
            running.push.send(changed(MOTION, true, 1)).expect("push");
            let fired = call_within(&mut running.calls, Duration::from_millis(1_500)).await;
            stopped(running).await;
            assert!(
                fired,
                "motion right after the lux sensor arrived was dropped"
            );
        })
        .await;
}
