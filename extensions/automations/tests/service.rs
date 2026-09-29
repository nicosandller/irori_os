//! The extension against a fake core on the other end of the wire: it arms the flows it finds,
//! calls services as its runs, and answers its page.

use std::collections::BTreeMap;

use irori_engine_automations::store::Store;
use irori_engine_automations::{Service, rpc};
use irori_protocol::engine::Incoming;
use irori_protocol::{FromExt, ToExt};
use irori_types::{
    Availability, BinarySensorCapabilities, BinarySensorState, Capabilities, Context, ContextId,
    Entity, EntityId, EntityState, LightCapabilities, LightState, Name, Origin, SensorCapabilities,
    SensorState, SensorValue, SensorValueType, State, Timestamp, UniqueId,
};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
use tokio::sync::mpsc;

const MOTION: &str = "binary_sensor.demo_movement_motion";
const LUX: &str = "sensor.demo_luminosity_illuminance";
const OCCUPANCY: &str = "binary_sensor.demo_mmwave_occupancy";
const LIGHT: &str = "light.demo_hall_light";

fn id(s: &str) -> EntityId {
    s.parse().expect("an entity id")
}

fn stamp() -> Timestamp {
    "2026-09-29T20:00:00Z".parse().expect("a time")
}

fn state(entity: &str, value: State) -> EntityState {
    EntityState {
        entity_id: id(entity),
        availability: Availability::Available,
        state: Some(value),
        attributes: BTreeMap::default(),
        last_changed: stamp(),
        last_updated: stamp(),
        last_reported: stamp(),
        context: Context {
            id: ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9").expect("valid"),
            parent_id: None,
            origin: Origin::System,
        },
    }
}

fn binary(entity: &str, on: bool) -> EntityState {
    state(entity, State::BinarySensor(BinarySensorState { on }))
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
    }
}

fn home() -> (Vec<Entity>, Vec<EntityState>) {
    let flag = || Capabilities::BinarySensor(BinarySensorCapabilities { device_class: None });
    (
        vec![
            entity(MOTION, flag()),
            entity(OCCUPANCY, flag()),
            entity(
                LUX,
                Capabilities::Sensor(SensorCapabilities {
                    value_type: SensorValueType::Number,
                    device_class: None,
                    unit: None,
                    state_class: None,
                }),
            ),
            entity(
                LIGHT,
                Capabilities::Light(LightCapabilities {
                    brightness: true,
                    color_temp_kelvin: None,
                    rgb: false,
                }),
            ),
        ],
        vec![
            binary(MOTION, false),
            binary(OCCUPANCY, true),
            state(
                LUX,
                State::Sensor(SensorState {
                    value: SensorValue::Number(8.0),
                }),
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
            ),
        ],
    )
}

/// The core's side: answers every request, and reports the calls it was asked to make.
async fn fake_core(stream: tokio::io::DuplexStream, calls: mpsc::UnboundedSender<FromExt>) {
    let (read, mut write) = tokio::io::split(stream);
    let (entities, states) = home();
    write
        .write_all(b"{\"type\":\"hello\",\"settings\":{}}\n")
        .await
        .expect("hello");
    let mut lines = tokio::io::BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let message: FromExt = serde_json::from_str(&line).expect("a message");
        let (id, value) = match &message {
            FromExt::Subscribe { id, .. } => (*id, serde_json::Value::Null),
            FromExt::GetStates { id } => (*id, serde_json::json!(states)),
            FromExt::GetRegistry { id } => (
                *id,
                serde_json::json!({ "entities": entities, "timezone": false, "location": false }),
            ),
            FromExt::GetHistory { id, .. } => (*id, serde_json::json!({})),
            FromExt::CallService { id, .. } => (*id, serde_json::Value::Null),
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
        let mut text = serde_json::to_string(&reply).expect("serializes");
        text.push('\n');
        write.write_all(text.as_bytes()).await.expect("write");
    }
}

#[tokio::test]
async fn it_arms_what_it_finds_calls_as_its_runs_and_answers_its_page() {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(dir.path().join("flows")).expect("mkdir");
    std::fs::write(
        dir.path().join("flows/hallway_motion_light.json"),
        include_str!("../../../fixtures/types/flow/valid/hallway.json"),
    )
    .expect("wrote the flow");

    let (core_end, engine_end) = tokio::io::duplex(1 << 20);
    let (calls_tx, mut calls) = mpsc::unbounded_channel();
    tokio::spawn(fake_core(core_end, calls_tx));
    let (read, write) = tokio::io::split(engine_end);
    let (_, client, _incoming) = irori_protocol::engine::connect(read, write)
        .await
        .expect("connects");
    let store = Store::open(dir.path().join("flows"), dir.path().join("data"));
    let (mut service, mut answers) = Service::start(client, store).await.expect("starts");

    let list = rpc::handle(&mut service, "flows.list", serde_json::json!({}))
        .await
        .expect("lists");
    assert_eq!(list["flows"][0]["state"], "armed", "{list}");

    // Motion in the dark: the light is asked to come on, at 60% as the core counts it.
    let mut moved = binary(MOTION, true);
    moved.last_updated =
        Timestamp::from_jiff(stamp().as_jiff() + jiff::SignedDuration::from_secs(1));
    moved.last_changed = moved.last_updated;
    assert!(
        service
            .incoming(Incoming::StateChanged {
                entity_id: id(MOTION),
                old_state: Some(Box::new(binary(MOTION, false))),
                new_state: Box::new(moved),
            })
            .await
    );
    let call = calls.recv().await.expect("a call");
    let FromExt::CallService {
        entity_id,
        command,
        data,
        ..
    } = call
    else {
        panic!("a service call");
    };
    assert_eq!(entity_id, id(LIGHT));
    assert_eq!(command, irori_protocol::WireCommand::TurnOn);
    assert_eq!(data.and_then(|d| d.brightness), Some(153));
    let (call_id, result) = answers.recv().await.expect("the answer");
    service
        .engine
        .call_finished(call_id, result, irori_engine_automations::now());
    service.apply_effects();

    // Waiting on occupancy: the page can see where.
    let active = rpc::handle(
        &mut service,
        "runs.active",
        serde_json::json!({ "id": "hallway_motion_light" }),
    )
    .await
    .expect("answers");
    assert_eq!(active[0]["at"][0]["node"], "clear", "{active}");

    // A dry run of a draft sends nothing.
    let dry = rpc::handle(
        &mut service,
        "test",
        serde_json::json!({ "id": "hallway_motion_light", "trigger": "motion", "dry": true }),
    )
    .await
    .expect("a dry run");
    assert_eq!(dry["test"], "dry");
    assert!(calls.try_recv().is_err(), "nothing was called for real");

    // Saving a draft that names a missing light keeps it, unarmed, with the problem pinned.
    let mut broken: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/types/flow/valid/hallway.json"
    ))
    .expect("json");
    broken["nodes"]["off"]["entity"] = serde_json::json!("light.nope");
    let saved = rpc::handle(
        &mut service,
        "flows.save",
        serde_json::json!({ "flow": broken }),
    )
    .await
    .expect("saves");
    assert_eq!(saved["problems"][0]["node"], "off", "{saved}");
    let list = rpc::handle(&mut service, "flows.list", serde_json::json!({}))
        .await
        .expect("lists");
    assert_eq!(list["flows"][0]["state"], "unarmed", "{list}");
    let versions = rpc::handle(
        &mut service,
        "versions.list",
        serde_json::json!({ "id": "hallway_motion_light" }),
    )
    .await
    .expect("versions");
    assert_eq!(
        versions.as_array().map(Vec::len),
        Some(2),
        "the file as found, and the edit"
    );

    // The run the edit cut short was kept, and says why.
    let runs = rpc::handle(
        &mut service,
        "runs.list",
        serde_json::json!({ "id": "hallway_motion_light" }),
    )
    .await
    .expect("runs");
    assert_eq!(runs[0]["outcome"], "aborted", "{runs}");

    let backtest = rpc::handle(
        &mut service,
        "backtest",
        serde_json::json!({ "id": "hallway_motion_light" }),
    )
    .await
    .expect("a backtest");
    assert_eq!(backtest["changes"], 0);
}
