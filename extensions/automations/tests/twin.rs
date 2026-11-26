//! Issue #199: a flow brought over from Home Assistant, turned on and off, against a fake core
//! that plays the Home Assistant extension's part (its twin list and the automations' states).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use irori_engine_automations::store::Store;
use irori_engine_automations::{Service, serve};
use irori_protocol::{FromExt, ToExt};
use irori_types::{
    Capabilities, Entity, EntityId, EntityState, LightCapabilities, LightState, Name, State,
    Timestamp, UniqueId,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
use tokio::sync::mpsc;

const LIGHT: &str = "light.hall";
const TWIN: &str = "automation.hall_light";

/// What the fake Home Assistant extension holds: each automation's state, the twins it switched
/// off, whether it refuses, and every ask in order.
#[derive(Debug, Default)]
struct Ha {
    on: BTreeMap<String, bool>,
    held: Vec<String>,
    refuse: bool,
    asked: Vec<String>,
}

type Shared = Arc<Mutex<Ha>>;

fn light() -> (Entity, EntityState) {
    let entity = Entity {
        id: EntityId::try_from(LIGHT).expect("an id"),
        protocol: "homeassistant".parse().expect("valid"),
        unique_id: UniqueId::try_from("r-hall").expect("valid"),
        name: Name::try_from("Hall light").expect("valid"),
        device_id: None,
        area_id: None,
        capabilities: Capabilities::Light(LightCapabilities {
            brightness: false,
            color_temp_kelvin: None,
            rgb: false,
        }),
        entity_category: None,
    };
    let at: Timestamp = "2026-09-29T20:00:00Z".parse().expect("a time");
    let state = EntityState {
        entity_id: entity.id.clone(),
        state: Some(State::Light(LightState {
            on: false,
            brightness: None,
            color_mode: None,
            color_temp_kelvin: None,
            rgb: None,
        })),
        attributes: BTreeMap::default(),
        availability: irori_types::Availability::Available,
        last_changed: at,
        last_updated: at,
        last_reported: at,
        context: irori_types::Context {
            id: irori_types::ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9").expect("id"),
            parent_id: None,
            origin: irori_types::Origin::System,
        },
    };
    (entity, state)
}

/// The Home Assistant extension's answer to `method`, as it would give it.
fn answer(ha: &mut Ha, method: &str, params: &Value) -> Result<Value, String> {
    ha.asked.push(method.to_owned());
    let entity_id = params["entity_id"].as_str().unwrap_or("").to_owned();
    match method {
        "automations.list" => Ok(json!({ "automations": [], "entities": {} })),
        "twins.list" => Ok(json!({
            "twins": ha.held.iter().map(|e| json!({ "entity_id": e, "name": e })).collect::<Vec<_>>()
        })),
        "twin.hand_over" if ha.refuse => {
            Err("Home Assistant didn't switch it off: locked".to_owned())
        }
        "twin.hand_over" => {
            if ha.on.get(&entity_id) == Some(&true) {
                ha.on.insert(entity_id.clone(), false);
                ha.held.push(entity_id);
                Ok(json!({ "switched_off": true }))
            } else {
                Ok(json!({ "switched_off": false, "already_off": true }))
            }
        }
        "twin.give_back" => {
            if ha.held.contains(&entity_id) {
                ha.held.retain(|e| *e != entity_id);
                ha.on.insert(entity_id, true);
                Ok(json!({ "switched_on": true }))
            } else {
                Ok(json!({ "switched_on": false }))
            }
        }
        other => Err(format!("no `{other}`")),
    }
}

/// The core: answers the engine, passes on the page's questions from `asks`, and their answers
/// back on `answers`.
async fn fake_core(
    stream: tokio::io::DuplexStream,
    ha: Shared,
    mut asks: mpsc::UnboundedReceiver<ToExt>,
    answers: mpsc::UnboundedSender<(u64, Result<Value, String>)>,
) {
    let (read, mut write) = tokio::io::split(stream);
    let (entity, state) = light();
    write
        .write_all(b"{\"type\":\"hello\",\"settings\":{}}\n")
        .await
        .expect("hello");
    let mut lines = tokio::io::BufReader::new(read).lines();
    let send = |message: ToExt| {
        let mut text = serde_json::to_string(&message).expect("serializes");
        text.push('\n');
        text
    };
    loop {
        tokio::select! {
            Some(ask) = asks.recv() => {
                write.write_all(send(ask).as_bytes()).await.expect("write");
            }
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { return };
                let message: FromExt = serde_json::from_str(&line).expect("a message");
                let reply = match message {
                    FromExt::Subscribe { id, .. } | FromExt::CallService { id, .. } => {
                        ToExt::Answer { id, value: Some(Value::Null), error: None }
                    }
                    FromExt::GetStates { id } => {
                        ToExt::Answer { id, value: Some(json!([state])), error: None }
                    }
                    FromExt::GetRegistry { id } => ToExt::Answer {
                        id,
                        value: Some(json!({ "entities": [entity], "timezone": true, "location": true })),
                        error: None,
                    },
                    FromExt::GetHistory { id, .. } => {
                        ToExt::Answer { id, value: Some(json!({})), error: None }
                    }
                    FromExt::AskExtension { id, extension, method, params } => {
                        assert_eq!(extension.as_str(), "homeassistant");
                        let answered = answer(&mut ha.lock().expect("ha"), &method, &params);
                        let (value, error) = match answered {
                            Ok(value) => (Some(value), None),
                            Err(why) => (None, Some(why)),
                        };
                        ToExt::Answer { id, value, error }
                    }
                    FromExt::AppAnswer { id, value, error } => {
                        let _ = answers.send((id, match error {
                            Some(why) => Err(why),
                            None => Ok(value.unwrap_or(Value::Null)),
                        }));
                        continue;
                    }
                    _ => continue,
                };
                write.write_all(send(reply).as_bytes()).await.expect("write");
            }
        }
    }
}

struct Page {
    asks: mpsc::UnboundedSender<ToExt>,
    answers: mpsc::UnboundedReceiver<(u64, Result<Value, String>)>,
    next: u64,
}

impl Page {
    async fn ask(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next += 1;
        let id = self.next;
        self.asks
            .send(ToExt::AppRequest {
                id,
                method: method.to_owned(),
                params,
                caller: None,
                from: None,
            })
            .expect("the core");
        let (answered, result) = tokio::time::timeout(Duration::from_secs(10), self.answers.recv())
            .await
            .expect("an answer in time")
            .expect("an answer");
        assert_eq!(answered, id);
        result
    }
}

fn flow(enabled: bool) -> Value {
    json!({
        "id": "hall_light",
        "name": "Hall light",
        "enabled": enabled,
        "home_assistant": { "entity_id": TWIN, "id": "1700000000001" },
        "nodes": {
            "t": { "type": "trigger", "trigger": { "type": "state", "entity": LIGHT, "to": true } },
            "off": { "type": "call", "service": "light.turn_off", "entity": LIGHT },
        },
        "wires": [["t", "off"]],
    })
}

async fn started(dir: &std::path::Path, ha: &Shared) -> Page {
    let (core_end, engine_end) = tokio::io::duplex(1 << 20);
    let (asks_tx, asks) = mpsc::unbounded_channel();
    let (answers_tx, answers) = mpsc::unbounded_channel();
    tokio::spawn(fake_core(core_end, ha.clone(), asks, answers_tx));
    let (read, write) = tokio::io::split(engine_end);
    let (_, client, incoming) = irori_protocol::engine::connect(read, write)
        .await
        .expect("connects");
    let store = Store::open(dir.join("flows"), dir.join("data"));
    let (service, calls) = Service::start(client, store).await.expect("starts");
    // The service's loop isn't `Send`: it runs on the test's own thread.
    tokio::task::spawn_local(serve(service, incoming, calls));
    Page {
        asks: asks_tx,
        answers,
        next: 0,
    }
}

fn enabled(dir: &std::path::Path) -> bool {
    let text = std::fs::read_to_string(dir.join("flows/hall_light.json")).expect("the flow");
    let flow: Value = serde_json::from_str(&text).expect("json");
    flow["enabled"] != json!(false)
}

#[tokio::test]
async fn turning_a_brought_over_flow_on_hands_its_twin_over_and_back() {
    tokio::task::LocalSet::new()
        .run_until(hands_over_and_back())
        .await;
}

async fn hands_over_and_back() {
    let dir = tempfile::tempdir().expect("a dir");
    std::fs::create_dir_all(dir.path().join("flows")).expect("mkdir");
    std::fs::write(
        dir.path().join("flows/hall_light.json"),
        flow(false).to_string(),
    )
    .expect("wrote");
    let ha = Shared::default();
    ha.lock().expect("ha").on.insert(TWIN.to_owned(), true);
    let mut page = started(dir.path(), &ha).await;

    // Not without the page's warning: nothing asked of Home Assistant, the flow stays off.
    let why = page
        .ask(
            "flows.enable",
            json!({ "id": "hall_light", "enabled": true }),
        )
        .await
        .expect_err("needs the warning");
    assert!(why.contains(TWIN), "{why}");
    assert!(!enabled(dir.path()));
    // Nor by saving it turned on, or any other way round.
    page.ask("flows.save", json!({ "flow": flow(true) }))
        .await
        .expect("saved");
    assert!(!enabled(dir.path()), "a save doesn't turn it on");
    assert!(ha.lock().expect("ha").asked.is_empty());

    // Home Assistant refuses: the flow stays off, and the page hears why.
    ha.lock().expect("ha").refuse = true;
    let why = page
        .ask(
            "flows.enable",
            json!({ "id": "hall_light", "enabled": true, "hand_over": true }),
        )
        .await
        .expect_err("refused");
    assert!(why.contains("stays off") && why.contains("locked"), "{why}");
    assert!(!enabled(dir.path()));
    ha.lock().expect("ha").refuse = false;

    // Confirmed: the twin goes off first, then the flow comes on.
    let answered = page
        .ask(
            "flows.enable",
            json!({ "id": "hall_light", "enabled": true, "hand_over": true }),
        )
        .await
        .expect("turned on");
    assert_eq!(answered["switched_off"], true, "{answered}");
    assert!(enabled(dir.path()));
    assert!(!ha.lock().expect("ha").on[TWIN]);

    // The list shows it, with its flow; nothing is given back while the flow runs.
    let listed = page
        .ask("home_assistant.automations", json!({}))
        .await
        .expect("a list");
    assert_eq!(
        listed["switched_off"],
        json!([{ "entity_id": TWIN, "name": TWIN, "waiting": false, "flow": "hall_light" }])
    );
    assert!(!ha.lock().expect("ha").on[TWIN]);

    // Off: the flow first, then the twin back on.
    page.ask(
        "flows.enable",
        json!({ "id": "hall_light", "enabled": false }),
    )
    .await
    .expect("turned off");
    assert!(!enabled(dir.path()));
    assert!(ha.lock().expect("ha").on[TWIN]);
    assert!(ha.lock().expect("ha").held.is_empty());

    // On again, then deleted: back on in Home Assistant too.
    page.ask(
        "flows.enable",
        json!({ "id": "hall_light", "enabled": true, "hand_over": true }),
    )
    .await
    .expect("turned on");
    assert!(!ha.lock().expect("ha").on[TWIN]);
    page.ask("flows.delete", json!({ "id": "hall_light" }))
        .await
        .expect("deleted");
    assert!(ha.lock().expect("ha").on[TWIN]);

    assert_eq!(
        ha.lock().expect("ha").asked,
        vec![
            "twin.hand_over",
            "twin.hand_over",
            "automations.list",
            "twins.list",
            "twin.give_back",
            "twin.hand_over",
            "twin.give_back",
        ]
    );
}

/// A flow whose file went while its twin was off: opening the list asks for the twin back.
#[tokio::test]
async fn a_twin_whose_flow_is_gone_is_given_back() {
    tokio::task::LocalSet::new().run_until(orphan()).await;
}

async fn orphan() {
    let dir = tempfile::tempdir().expect("a dir");
    std::fs::create_dir_all(dir.path().join("flows")).expect("mkdir");
    let ha = Shared::default();
    {
        let mut ha = ha.lock().expect("ha");
        ha.on.insert(TWIN.to_owned(), false);
        ha.held.push(TWIN.to_owned());
    }
    let mut page = started(dir.path(), &ha).await;
    let listed = page
        .ask("home_assistant.automations", json!({}))
        .await
        .expect("a list");
    assert_eq!(listed["switched_off"][0]["waiting"], true, "{listed}");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !ha.lock().expect("ha").on[TWIN] {
        assert!(tokio::time::Instant::now() < deadline, "never given back");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A twin the owner switched off in Home Assistant before is left alone: the flow comes on, and
/// turning it off never switches the automation on.
#[tokio::test]
async fn a_twin_already_off_stays_off() {
    tokio::task::LocalSet::new().run_until(already_off()).await;
}

async fn already_off() {
    let dir = tempfile::tempdir().expect("a dir");
    std::fs::create_dir_all(dir.path().join("flows")).expect("mkdir");
    std::fs::write(
        dir.path().join("flows/hall_light.json"),
        flow(false).to_string(),
    )
    .expect("wrote");
    let ha = Shared::default();
    ha.lock().expect("ha").on.insert(TWIN.to_owned(), false);
    let mut page = started(dir.path(), &ha).await;
    let answered = page
        .ask(
            "flows.enable",
            json!({ "id": "hall_light", "enabled": true, "hand_over": true }),
        )
        .await
        .expect("turned on");
    assert_eq!(answered["already_off"], true, "{answered}");
    assert!(enabled(dir.path()));
    page.ask(
        "flows.enable",
        json!({ "id": "hall_light", "enabled": false }),
    )
    .await
    .expect("turned off");
    assert!(!ha.lock().expect("ha").on[TWIN]);
}
