//! The extension against a fake core on the other end of the wire: it arms the flows it finds,
//! calls services as its runs, and answers its page.

use std::collections::BTreeMap;

use irori_engine_automations::store::Store;
use irori_engine_automations::{Service, rpc};
use irori_protocol::engine::{Caller, Incoming};
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
        entity_category: None,
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
                    options: Vec::new(),
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

/// What the fake core says of each person, by id, as `get_reach` answers it. A test changes it
/// to move a person's rooms.
type People = std::sync::Arc<std::sync::Mutex<BTreeMap<String, serde_json::Value>>>;

/// What the engine told the core to write down: the action, what it was about, and why.
type Notes = mpsc::UnboundedSender<(String, String, String)>;

/// The core's side: answers every request, and reports the calls it was asked to make.
async fn fake_core(stream: tokio::io::DuplexStream, calls: mpsc::UnboundedSender<FromExt>) {
    fake_core_with(
        stream,
        calls,
        People::default(),
        mpsc::unbounded_channel().0,
    )
    .await;
}

async fn fake_core_with(
    stream: tokio::io::DuplexStream,
    calls: mpsc::UnboundedSender<FromExt>,
    people: People,
    notes: Notes,
) {
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
            FromExt::GetReach { id, user } => (
                *id,
                people
                    .lock()
                    .expect("people")
                    .get(user.as_str())
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({ "here": false })),
            ),
            FromExt::Note {
                action,
                target,
                detail,
            } => {
                let _ = notes.send((action.clone(), target.clone(), detail.clone()));
                continue;
            }
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

    let list = rpc::handle(
        &mut service,
        &Caller::owner(),
        "flows.list",
        serde_json::json!({}),
    )
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
    assert_eq!(command, "turn_on");
    assert_eq!(
        data.and_then(|d| d.get("brightness").cloned()),
        Some(153.into())
    );
    let (call_id, result) = answers.recv().await.expect("the answer");
    service
        .engine
        .call_finished(call_id, result, irori_engine_automations::now());
    service.apply_effects();

    // Waiting on occupancy: the page can see where.
    let active = rpc::handle(
        &mut service,
        &Caller::owner(),
        "runs.active",
        serde_json::json!({ "id": "hallway_motion_light" }),
    )
    .await
    .expect("answers");
    assert_eq!(active[0]["at"][0]["node"], "clear", "{active}");

    // The assistant is handed the logic, what each entity reports now, and the run under way.
    let brief = rpc::handle(
        &mut service,
        &Caller::owner(),
        "flow.brief",
        serde_json::json!({ "id": "hallway_motion_light" }),
    )
    .await
    .expect("a brief");
    let text = brief["text"].as_str().expect("words");
    for said in [
        "It is armed",
        "- None. Nothing in its definition stops it running.",
        "`motion` — trigger",
        "num('sensor.demo_luminosity_illuminance') < 30",
        "yes → `on`",
        "matched → `off`",
        "x (`sensor.demo_luminosity_illuminance`) = ",
        "x (`light.demo_hall_light`) = ",
        "A run is going right now. started",
        "called light.turn_on on x (`light.demo_hall_light`)",
        "it worked",
        "`clear` (wait until) is still going",
    ] {
        assert!(text.contains(said), "no {said:?} in:\n{text}");
    }

    // A dry run of a draft sends nothing.
    let dry = rpc::handle(
        &mut service,
        &Caller::owner(),
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
        &Caller::owner(),
        "flows.save",
        serde_json::json!({ "flow": broken }),
    )
    .await
    .expect("saves");
    assert_eq!(saved["problems"][0]["node"], "off", "{saved}");
    let list = rpc::handle(
        &mut service,
        &Caller::owner(),
        "flows.list",
        serde_json::json!({}),
    )
    .await
    .expect("lists");
    assert_eq!(list["flows"][0]["state"], "unarmed", "{list}");
    let versions = rpc::handle(
        &mut service,
        &Caller::owner(),
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
        &Caller::owner(),
        "runs.list",
        serde_json::json!({ "id": "hallway_motion_light" }),
    )
    .await
    .expect("runs");
    assert_eq!(runs[0]["outcome"], "aborted", "{runs}");

    // What stops it running is told with where it is, and so is how the last run ended.
    let brief = rpc::handle(
        &mut service,
        &Caller::owner(),
        "flow.brief",
        serde_json::json!({ "id": "hallway_motion_light" }),
    )
    .await
    .expect("a brief");
    let text = brief["text"].as_str().expect("words");
    for said in [
        "It is NOT armed and cannot run until this is fixed",
        "- Error (stops it running), at step `off`",
        "`light.nope` = no such entity in the home",
        "The last run that wasn't a test: started",
        "was aborted from outside (\"changed\")",
    ] {
        assert!(text.contains(said), "no {said:?} in:\n{text}");
    }
    let short = rpc::handle(
        &mut service,
        &Caller::owner(),
        "flow.brief",
        serde_json::json!({ "id": "hallway_motion_light", "budget": 900 }),
    )
    .await
    .expect("a short brief");
    let short = short["text"].as_str().expect("words");
    assert!(short.chars().count() <= 900, "{}", short.chars().count());
    assert!(short.contains("at step `off`"), "{short}");

    // The canvas asks what happened since it last looked: nothing, the first time; since a
    // moment it names, the run that just ended.
    let first = rpc::handle(
        &mut service,
        &Caller::owner(),
        "live",
        serde_json::json!({ "id": "hallway_motion_light" }),
    )
    .await
    .expect("live");
    assert_eq!(first["runs"], serde_json::json!([]), "{first}");
    let since = rpc::handle(
        &mut service,
        &Caller::owner(),
        "live",
        serde_json::json!({ "id": "hallway_motion_light", "after": "2000-01-01T00:00:00Z" }),
    )
    .await
    .expect("live");
    assert_eq!(since["runs"][0]["outcome"], "aborted", "{since}");
    assert!(since["now"].is_string());

    let backtest = rpc::handle(
        &mut service,
        &Caller::owner(),
        "backtest",
        serde_json::json!({ "id": "hallway_motion_light" }),
    )
    .await
    .expect("a backtest");
    assert_eq!(backtest["changes"], 0);

    // How a test was set up is kept, as the page gave it, and goes when the flow does.
    let id = serde_json::json!({ "id": "hallway_motion_light" });
    let none = rpc::handle(&mut service, &Caller::owner(), "tests.get", id.clone())
        .await
        .expect("nothing kept yet");
    assert!(none.is_null(), "{none}");
    let settings = serde_json::json!({ "mode": "dry", "pretend": { "sensor.lux": 20 } });
    rpc::handle(
        &mut service,
        &Caller::owner(),
        "tests.save",
        serde_json::json!({ "id": "hallway_motion_light", "settings": settings }),
    )
    .await
    .expect("kept");
    let kept = rpc::handle(&mut service, &Caller::owner(), "tests.get", id.clone())
        .await
        .expect("read back");
    assert_eq!(kept, settings);
    let huge = "x".repeat(20_000);
    assert!(
        rpc::handle(
            &mut service,
            &Caller::owner(),
            "tests.save",
            serde_json::json!({ "id": "hallway_motion_light", "settings": huge }),
        )
        .await
        .is_err()
    );
    rpc::handle(&mut service, &Caller::owner(), "flows.delete", id.clone())
        .await
        .expect("deleted");
    assert!(
        rpc::handle(
            &mut service,
            &Caller::owner(),
            "tests.save",
            serde_json::json!({ "id": "hallway_motion_light", "settings": settings }),
        )
        .await
        .is_err(),
        "no settings for a flow that isn't there"
    );
    let gone = rpc::handle(&mut service, &Caller::owner(), "tests.get", id)
        .await
        .expect("asked");
    assert!(gone.is_null(), "{gone}");

    // Windows are told by the engine's clock, and say why when they can't be.
    let windows = serde_json::json!({ "conditions": [
        { "type": "time", "after": "00:00" },
        { "type": "sun", "after": "sunset", "before": "sunrise", "after_offset": "-30m" },
        { "type": "sun", "after": "sunset", "offset": "1h", "after_offset": "-30m" },
    ] });
    let unset = rpc::handle(
        &mut service,
        &Caller::owner(),
        "clock.holds",
        windows.clone(),
    )
    .await
    .expect("asked");
    assert!(unset.get("time_zone").is_none(), "{unset}");
    assert!(
        unset["holds"][0]["why"]
            .as_str()
            .is_some_and(|why| why.contains("time zone")),
        "{unset}"
    );
    let brussels =
        irori_rules::clock::Place::new("Europe/Brussels", Some((50.8467, 4.3525))).expect("a zone");
    service.engine.set_place(Some(brussels), stamp());
    let told = rpc::handle(&mut service, &Caller::owner(), "clock.holds", windows)
        .await
        .expect("asked");
    assert_eq!(told["time_zone"], "Europe/Brussels");
    assert_eq!(told["location"], true);
    // After midnight is every moment of the day.
    assert_eq!(told["holds"][0]["holds"], true, "{told}");
    assert!(told["holds"][1]["holds"].is_boolean(), "{told}");
    assert!(
        told["holds"][2]["why"]
            .as_str()
            .is_some_and(|why| why.contains("not both")),
        "{told}"
    );
    assert!(told["sun"]["sunset"].is_string(), "{told}");
}

/// A suggestion is checked and handed back to draw. The file stays as it was.
#[tokio::test]
async fn a_suggestion_is_checked_and_not_written() {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(dir.path().join("flows")).expect("mkdir");
    std::fs::write(
        dir.path().join("flows/hallway_motion_light.json"),
        include_str!("../../../fixtures/types/flow/valid/hallway.json"),
    )
    .expect("wrote the flow");

    let (core_end, engine_end) = tokio::io::duplex(1 << 20);
    let (calls_tx, _calls) = mpsc::unbounded_channel();
    tokio::spawn(fake_core(core_end, calls_tx));
    let (read, write) = tokio::io::split(engine_end);
    let (_, client, _incoming) = irori_protocol::engine::connect(read, write)
        .await
        .expect("connects");
    let store = Store::open(dir.path().join("flows"), dir.path().join("data"));
    let (mut service, _answers) = Service::start(client, store).await.expect("starts");

    let source = rpc::handle(
        &mut service,
        &Caller::owner(),
        "flow.source",
        serde_json::json!({ "id": "hallway_motion_light" }),
    )
    .await
    .expect("the source");
    let source = &source["flow"];
    assert!(source.get("layout").is_none(), "{source}");
    assert!(source.get("groups").is_none(), "{source}");
    assert!(source.get("enabled").is_none(), "{source}");
    assert!(source["nodes"]["motion"].is_object(), "{source}");

    rpc::handle(
        &mut service,
        &Caller::owner(),
        "flows.enable",
        serde_json::json!({ "id": "hallway_motion_light", "enabled": false }),
    )
    .await
    .expect("turned off");
    let mut kept = rpc::handle(
        &mut service,
        &Caller::owner(),
        "flows.get",
        serde_json::json!({ "id": "hallway_motion_light" }),
    )
    .await
    .expect("the saved flow");
    kept["flow"]["groups"] = serde_json::json!({ "motion": "Night" });
    rpc::handle(
        &mut service,
        &Caller::owner(),
        "flows.save",
        serde_json::json!({ "flow": kept["flow"] }),
    )
    .await
    .expect("groups kept");

    let mut suggested = source.clone();
    suggested["nodes"]["on"]["data"]["brightness_pct"] = serde_json::json!(40);
    suggested["nodes"]["pause"] = serde_json::json!({ "type": "delay", "for": "5s" });
    suggested["enabled"] = serde_json::json!(true);
    let proposed = rpc::handle(
        &mut service,
        &Caller::owner(),
        "flows.propose",
        serde_json::json!({ "id": "hallway_motion_light", "flow": suggested }),
    )
    .await
    .expect("a suggestion");
    assert_eq!(proposed["ok"], true, "{proposed}");
    assert_eq!(
        proposed["flow"]["enabled"], false,
        "the switch stays as saved"
    );
    assert_eq!(proposed["flow"]["groups"]["motion"], "Night");
    assert_eq!(
        proposed["flow"]["layout"]["on"],
        serde_json::json!([560.0, 0.0])
    );
    assert_eq!(
        proposed["flow"]["layout"]["pause"],
        serde_json::json!([1400.0, 0.0]),
        "a new node sits to the right"
    );
    assert!(
        proposed["problems"]
            .as_array()
            .is_some_and(|problems| problems.iter().any(|problem| {
                problem["severity"] == "warning"
                    && problem["message"]
                        .as_str()
                        .is_some_and(|message| message.contains("never runs"))
            })),
        "{proposed}"
    );

    let saved = rpc::handle(
        &mut service,
        &Caller::owner(),
        "flows.get",
        serde_json::json!({ "id": "hallway_motion_light" }),
    )
    .await
    .expect("still the file");
    assert_eq!(saved["flow"]["nodes"]["on"]["data"]["brightness_pct"], 60);
    assert!(saved["flow"]["nodes"].get("pause").is_none(), "{saved}");

    let mut omitted = source.clone();
    omitted.as_object_mut().expect("an object").remove("id");
    let filled = rpc::handle(
        &mut service,
        &Caller::owner(),
        "flows.propose",
        serde_json::json!({ "id": "hallway_motion_light", "flow": omitted }),
    )
    .await
    .expect("the id filled in");
    assert_eq!(filled["ok"], true, "{filled}");
    assert_eq!(filled["flow"]["id"], "hallway_motion_light");

    let mut other = source.clone();
    other["id"] = serde_json::json!("other");
    let refused = rpc::handle(
        &mut service,
        &Caller::owner(),
        "flows.propose",
        serde_json::json!({ "id": "hallway_motion_light", "flow": other }),
    )
    .await
    .expect("refused");
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(
        refused["problems"][0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("keep that id")),
        "{refused}"
    );

    let mut coded = source.clone();
    coded["nodes"]["open"] = serde_json::json!({
        "type": "call",
        "service": "lock.unlock",
        "entity": "lock.front_door",
        "data": { "code": "1234" },
    });
    let refused = rpc::handle(
        &mut service,
        &Caller::owner(),
        "flows.propose",
        serde_json::json!({ "id": "hallway_motion_light", "flow": coded }),
    )
    .await
    .expect("refused");
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(
        refused.to_string().contains("code"),
        "a code is refused: {refused}"
    );
    assert!(refused.get("flow").is_none(), "{refused}");

    let refused = rpc::handle(
        &mut service,
        &Caller::owner(),
        "flows.propose",
        serde_json::json!({ "id": "hallway_motion_light", "flow": { "name": "Nope" } }),
    )
    .await
    .expect("refused");
    assert_eq!(refused["ok"], false, "{refused}");
}

/// A suggestion for a flow nobody has saved yet is checked, laid out left to right, and
/// not written.
#[tokio::test]
async fn a_suggestion_for_a_new_flow_is_laid_out_and_not_written() {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(dir.path().join("flows")).expect("mkdir");

    let (core_end, engine_end) = tokio::io::duplex(1 << 20);
    let (calls_tx, _calls) = mpsc::unbounded_channel();
    tokio::spawn(fake_core(core_end, calls_tx));
    let (read, write) = tokio::io::split(engine_end);
    let (_, client, _incoming) = irori_protocol::engine::connect(read, write)
        .await
        .expect("connects");
    let store = Store::open(dir.path().join("flows"), dir.path().join("data"));
    let (mut service, _answers) = Service::start(client, store).await.expect("starts");

    let proposed = rpc::handle(
        &mut service,
        &Caller::owner(),
        "flows.propose",
        serde_json::json!({ "flow": {
            "name": "Morning",
            "nodes": {
                "at": { "type": "trigger", "trigger": { "type": "startup" } },
                "pause": { "type": "delay", "for": "5s" },
                "stop": { "type": "stop" }
            },
            "wires": [["at", "pause"], ["pause", "stop"]]
        }}),
    )
    .await
    .expect("answers");
    assert_eq!(proposed["ok"], true, "{proposed}");
    let layout = &proposed["flow"]["layout"];
    let x = |node: &str| layout[node][0].as_f64().expect("placed");
    assert!(x("at") < x("pause") && x("pause") < x("stop"), "{layout}");
    assert!(
        std::fs::read_dir(dir.path().join("flows"))
            .expect("flows")
            .next()
            .is_none(),
        "nothing was written"
    );
}

fn person(name: &str, entities: &[&str]) -> serde_json::Value {
    serde_json::json!({ "here": true, "name": name, "owner": false, "entities": entities })
}

fn leo() -> Caller {
    Caller {
        user: Some("leo".parse().expect("a user id")),
        owner: false,
    }
}

/// A flow in the hall: motion turns the light on.
fn leos_flow(id: &str, sensor: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "name": "Leo's light",
        "nodes": {
            "motion": { "type": "trigger", "trigger": { "type": "state", "entity": sensor, "to": true } },
            "on": { "type": "call", "service": "light.turn_on", "entity": LIGHT }
        },
        "wires": [["motion", "on"]]
    })
}

/// A regular user has flows of their own, and only what they can reach: someone else's flow is
/// no flow, what they cannot see is unknown to a flow of theirs, and a flow that stops being
/// theirs to run is turned off once, with the reason.
#[tokio::test]
async fn a_regular_user_has_their_own_flows_and_only_what_they_can_reach() {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(dir.path().join("flows")).expect("mkdir");
    std::fs::write(
        dir.path().join("flows/hallway_motion_light.json"),
        include_str!("../../../fixtures/types/flow/valid/hallway.json"),
    )
    .expect("wrote the flow");

    let people = People::default();
    people
        .lock()
        .expect("people")
        .insert("leo".into(), person("Leo", &[MOTION, LIGHT]));
    let (notes_tx, mut notes) = mpsc::unbounded_channel();
    let (core_end, engine_end) = tokio::io::duplex(1 << 20);
    let (calls_tx, _calls) = mpsc::unbounded_channel();
    tokio::spawn(fake_core_with(core_end, calls_tx, people.clone(), notes_tx));
    let (read, write) = tokio::io::split(engine_end);
    let (_, client, _incoming) = irori_protocol::engine::connect(read, write)
        .await
        .expect("connects");
    let store = Store::open(dir.path().join("flows"), dir.path().join("data"));
    let (mut service, _answers) = Service::start(client, store).await.expect("starts");

    // Leo is shown nothing of the home's. The owner is shown the home's.
    let list = rpc::handle(&mut service, &leo(), "flows.list", serde_json::json!({}))
        .await
        .expect("lists");
    assert_eq!(list["flows"], serde_json::json!([]), "{list}");
    let list = rpc::handle(
        &mut service,
        &Caller::owner(),
        "flows.list",
        serde_json::json!({}),
    )
    .await
    .expect("lists");
    assert_eq!(list["flows"][0]["id"], "hallway_motion_light");

    // Somebody else's flow is no flow, to read, to change, to run, or to delete.
    for method in [
        "flows.get",
        "flow.brief",
        "flow.source",
        "flows.delete",
        "versions.list",
        "runs.list",
        "nearmiss.list",
        "tests.get",
    ] {
        let said = rpc::handle(
            &mut service,
            &leo(),
            method,
            serde_json::json!({ "id": "hallway_motion_light" }),
        )
        .await
        .expect_err(method);
        assert_eq!(said, "there's no flow `hallway_motion_light`", "{method}");
    }
    let missing = rpc::handle(
        &mut service,
        &leo(),
        "flows.get",
        serde_json::json!({ "id": "zq_nothing_here" }),
    )
    .await
    .expect_err("there's none");
    assert_eq!(
        missing.replace("zq_nothing_here", "ID"),
        "there's no flow `ID`",
        "a flow of someone else's answers as one that was never there"
    );
    let overwritten = rpc::handle(
        &mut service,
        &leo(),
        "flows.save",
        serde_json::json!({ "flow": leos_flow("hallway_motion_light", MOTION) }),
    )
    .await
    .expect_err("not his to write over");
    assert!(overwritten.contains("exists already"), "{overwritten}");
    assert!(
        service
            .store
            .flow(&"hallway_motion_light".parse().expect("id"))
            .is_some_and(|flow| flow.owner.is_none())
    );

    // His own: saved, his, and armed because he has both the sensor and the light.
    let saved = rpc::handle(
        &mut service,
        &leo(),
        "flows.save",
        serde_json::json!({ "flow": leos_flow("hall_light", MOTION) }),
    )
    .await
    .expect("saves");
    assert_eq!(saved["problems"], serde_json::json!([]), "{saved}");
    let list = rpc::handle(&mut service, &leo(), "flows.list", serde_json::json!({}))
        .await
        .expect("lists");
    assert_eq!(list["flows"][0]["id"], "hall_light", "{list}");
    assert_eq!(list["flows"][0]["state"], "armed", "{list}");
    assert_eq!(list["flows"].as_array().map(Vec::len), Some(1), "{list}");
    // The page cannot say whose it is: Leo naming someone else as the owner is not heard.
    let mut forged = leos_flow("hall_light_2", MOTION);
    forged["owner"] = serde_json::json!("nico");
    rpc::handle(
        &mut service,
        &leo(),
        "flows.save",
        serde_json::json!({ "flow": forged }),
    )
    .await
    .expect("saves");
    assert_eq!(
        service
            .store
            .flow(&"hall_light_2".parse().expect("id"))
            .and_then(|flow| flow.owner.as_ref().map(ToString::to_string))
            .as_deref(),
        Some("leo")
    );
    // The owner is told whose each is.
    let list = rpc::handle(
        &mut service,
        &Caller::owner(),
        "flows.list",
        serde_json::json!({}),
    )
    .await
    .expect("lists");
    let whose: Vec<_> = list["flows"]
        .as_array()
        .expect("flows")
        .iter()
        .map(|flow| {
            (
                flow["id"].as_str().unwrap_or(""),
                flow["owner"]["name"].as_str(),
            )
        })
        .collect();
    assert!(whose.contains(&("hall_light", Some("Leo"))), "{whose:?}");
    assert!(whose.contains(&("hallway_motion_light", None)), "{whose:?}");

    // What he cannot see is unknown to a flow of his, and says no more than a missing one.
    let hidden = rpc::handle(
        &mut service,
        &leo(),
        "flows.validate",
        serde_json::json!({ "flow": leos_flow("probe", OCCUPANCY) }),
    )
    .await
    .expect("answers");
    let missing = rpc::handle(
        &mut service,
        &leo(),
        "flows.validate",
        serde_json::json!({ "flow": leos_flow("probe", "binary_sensor.zq_nothing_here") }),
    )
    .await
    .expect("answers");
    assert!(hidden.to_string().contains("no such entity"), "{hidden}");
    assert_eq!(
        hidden.to_string().replace(OCCUPANCY, "ID"),
        missing
            .to_string()
            .replace("binary_sensor.zq_nothing_here", "ID"),
        "a hidden sensor is told as a missing one is"
    );
    // A draft that names it cannot read it either: a dry run is told nothing of it.
    let dry = rpc::handle(
        &mut service,
        &leo(),
        "test",
        serde_json::json!({
            "id": "probe",
            "dry": true,
            "trigger": "start",
            "flow": {
                "id": "probe",
                "name": "Probe",
                "nodes": {
                    "start": { "type": "trigger", "trigger": { "type": "startup" } },
                    "gate": { "type": "gate", "condition": { "type": "expr", "expr": "num('sensor.demo_luminosity_illuminance') < 30" } }
                },
                "wires": [["start", "gate"]]
            }
        }),
    )
    .await;
    let dry = dry.expect("a dry run of his own draft");
    assert!(
        dry.to_string().contains("no such entity") && !dry.to_string().contains("\"value\":8"),
        "a dry run read a sensor outside his rooms: {dry}"
    );

    // His motion sensor leaves his rooms: the flow is turned off, and the log is told once.
    people
        .lock()
        .expect("people")
        .insert("leo".into(), person("Leo", &[LIGHT]));
    assert!(service.incoming(Incoming::RegistryChanged).await);
    let list = rpc::handle(&mut service, &leo(), "flows.list", serde_json::json!({}))
        .await
        .expect("lists");
    assert_eq!(list["flows"][0]["state"], "unarmed", "{list}");
    assert!(
        list["flows"][0]["reason"]
            .as_str()
            .is_some_and(|why| why.contains("no such entity")),
        "{list}"
    );
    let mut told = Vec::new();
    while let Ok(note) = notes.try_recv() {
        told.push(note);
    }
    let for_hall_light: Vec<_> = told
        .iter()
        .filter(|(_, target, _)| target == "hall_light")
        .collect();
    assert_eq!(for_hall_light.len(), 1, "{told:?}");
    assert_eq!(for_hall_light[0].0, "flow_turned_off");
    // Nothing changed since, so nothing more is said.
    assert!(service.incoming(Incoming::RegistryChanged).await);
    assert!(notes.try_recv().is_err(), "said again with nothing new");

    // He is removed: his flows are kept, off, and the home's own.
    people
        .lock()
        .expect("people")
        .insert("leo".into(), serde_json::json!({ "here": false }));
    assert!(service.incoming(Incoming::RegistryChanged).await);
    let kept = service
        .store
        .flow(&"hall_light".parse().expect("id"))
        .cloned()
        .expect("still there");
    assert!(kept.owner.is_none() && !kept.enabled, "{kept:?}");
    let mut handed = Vec::new();
    while let Ok(note) = notes.try_recv() {
        handed.push(note);
    }
    assert!(
        handed
            .iter()
            .any(|(action, target, _)| action == "flow_handed_over" && target == "hall_light"),
        "{handed:?}"
    );
}
