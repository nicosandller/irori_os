//! Automation engines through the host, end to end (`docs/specs/automations.md` §B2): a real
//! process that asks for states, calls a lamp as one of its runs, is refused what it didn't ask
//! for, and answers its page.

use std::sync::Arc;
use std::time::Duration;

use irori_core::{Core, ExtensionHost, ExtensionStatus, SystemClock, Timing};
use irori_protocol::types::{
    Capabilities, ContextId, DeviceDescription, EntityDescription, EntityId, ExtensionId,
    LightCapabilities, LightState, Name, Origin, Service, State, StateReport, UniqueId,
};
use irori_protocol::{NoSettings, Protocol, ProtocolContext, ProtocolError, builtin};

fn uid(s: &str) -> UniqueId {
    UniqueId::try_from(s).expect("valid")
}

fn report(on: bool, caused_by: Option<ContextId>) -> StateReport {
    StateReport {
        unique_id: uid("lamp-light"),
        state: Some(State::Light(LightState {
            on,
            brightness: None,
            color_mode: None,
            color_temp_kelvin: None,
            rgb: None,
        })),
        attributes: Default::default(),
        caused_by,
        replayed: false,
    }
}

struct Lamp;
impl Protocol for Lamp {
    type Config = NoSettings;
    const MANIFEST: &'static str = r#"
        [extension]
        id = "lamp"
        name = "Lamp"
        version = "0.1.0"
        irori = ">=0.0.0"

        [[contributes.protocol]]
        iot_class = "local_push"
        entity_kinds = ["light"]
    "#;
    async fn run(_: NoSettings, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
        ctx.describe_device(DeviceDescription {
            unique_id: uid("lamp"),
            name: Name::try_from("Lamp")?,
            manufacturer: None,
            model: None,
            sw_version: None,
            hw_version: None,
            serial_number: None,
            suggested_area: None,
            via_device_unique_id: None,
        })
        .await?;
        ctx.describe_entity(EntityDescription {
            unique_id: uid("lamp-light"),
            name: None,
            device_unique_id: Some(uid("lamp")),
            suggested_object_id: None,
            capabilities: Capabilities::Light(LightCapabilities {
                brightness: false,
                color_temp_kelvin: None,
                rgb: false,
            }),
            entity_category: None,
        })
        .await?;
        ctx.report_state(report(true, None));
        while let Some(incoming) = ctx.next_call().await {
            let caused_by = Some(incoming.call.context.id.clone());
            let on = matches!(incoming.call.service, Service::LightTurnOn(_));
            incoming.reply(Ok(()));
            ctx.report_state(report(on, caused_by));
        }
        Ok(())
    }
}

const RUN_ID: &str = "01K5B2Q9A1B2C3D4E5F6G7H8J9";

/// The engine: one request at a time, each answer echoed to stderr so the test can read it from
/// the extension's log.
const ENGINE: &str = r#"#!/bin/sh
read hello
sleep 1
echo '{"type":"get_states","id":1}'
read answer; echo "states: $answer" >&2
echo '{"type":"call_service","id":2,"entity_id":"light.lamp_lamp","command":"turn_off","run_id":"01K5B2Q9A1B2C3D4E5F6G7H8J9","parent_id":"01K5B2Q9A1B2C3D4E5F6G7H8JA"}'
read answer; echo "call: $answer" >&2
echo '{"type":"get_history","id":3,"entities":["light.lamp_lamp"],"since":"2000-01-01T00:00:00Z"}'
read answer; echo "history: $answer" >&2
echo '{"type":"describe_device","id":4,"device":{"unique_id":"x","name":"X"}}'
read answer; echo "describe: $answer" >&2
read request; echo "page: $request" >&2
echo '{"type":"app_answer","id":0,"value":{"flows":3}}'
while read line; do :; done
"#;

async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..500 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for: {what}");
}

#[tokio::test]
async fn an_engine_acts_as_its_own_runs_within_its_scopes_and_answers_its_page() {
    use std::os::unix::fs::PermissionsExt;

    let core = Core::new(Arc::new(SystemClock));
    let packages_dir = tempfile::tempdir().expect("temp dir");
    let package = packages_dir.path().join("engine");
    std::fs::create_dir_all(package.join("bin")).expect("made the package dir");
    std::fs::write(
        package.join("irori-extension.toml"),
        r#"
            [extension]
            id = "engine"
            name = "Engine"
            version = "0.1.0"
            irori = ">=0.0.0"

            [[contributes.automation]]
            run = { command = "bin/engine" }

            [[contributes.app]]
            label = "Engine"
            entry = "app/index.html"

            [permissions]
            api = ["states:read", "services:call"]
        "#,
    )
    .expect("wrote the manifest");
    let program = package.join("bin/engine");
    std::fs::write(&program, ENGINE).expect("wrote the program");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
        .expect("made it executable");

    let host = ExtensionHost::start_with_packages(
        &core,
        vec![builtin::<Lamp>().expect("valid")],
        Timing::default(),
        packages_dir.path().to_path_buf(),
    )
    .expect("starts");
    let engine = ExtensionId::try_from("engine").expect("valid");
    let said = |what: &str| core.log(&engine).iter().any(|line| line.contains(what));

    eventually("the engine's call is answered", || said("call: ")).await;
    assert!(
        said(r#"states: {"type":"answer","id":1,"value":["#),
        "{:?}",
        core.log(&engine)
    );
    assert!(
        said(r#"call: {"type":"answer","id":2,"value":null}"#),
        "{:?}",
        core.log(&engine)
    );

    // The call happened: the lamp says it's off, because the engine asked.
    let lamp = EntityId::try_from("light.lamp_lamp").expect("valid");
    eventually("the lamp turns off", || {
        core.state(&lamp)
            .and_then(|state| state.state)
            .is_some_and(|state| matches!(state, State::Light(LightState { on: false, .. })))
    })
    .await;

    // What it didn't ask for, it doesn't get; and an engine has no devices to describe.
    eventually("the history request is answered", || said("history: ")).await;
    assert!(
        said("this extension didn't ask for history:read"),
        "{:?}",
        core.log(&engine)
    );
    eventually("the describe request is answered", || said("describe: ")).await;
    assert!(said("contributes no protocol"), "{:?}", core.log(&engine));

    // Its page's question reaches it and the answer comes back.
    let answer = core
        .app_request(
            &engine,
            "flows.list".into(),
            serde_json::json!({}),
            irori_protocol::engine::Caller::owner(),
        )
        .await
        .expect("answered");
    assert_eq!(answer, serde_json::json!({"flows": 3}));
    assert!(said(r#""method":"flows.list""#), "{:?}", core.log(&engine));

    let overview = core.extensions().get(&engine).cloned().expect("listed");
    assert_eq!(overview.status, ExtensionStatus::Running);
    let info = overview.info.expect("described");
    assert!(info.engine);
    assert_eq!(info.app.expect("a page").label.as_str(), "Engine");

    host.shutdown().await;
}

/// The run id on every call it makes is the engine's, with this extension as the origin.
#[tokio::test]
async fn the_core_names_the_engine_as_the_origin_of_its_calls() {
    use std::os::unix::fs::PermissionsExt;

    let core = Core::new(Arc::new(SystemClock));
    let mut events = core.subscribe();
    let packages_dir = tempfile::tempdir().expect("temp dir");
    let package = packages_dir.path().join("engine");
    std::fs::create_dir_all(package.join("bin")).expect("made the package dir");
    std::fs::write(
        package.join("irori-extension.toml"),
        r#"
            [extension]
            id = "engine"
            name = "Engine"
            version = "0.1.0"
            irori = ">=0.0.0"

            [[contributes.automation]]
            run = { command = "bin/engine" }

            [permissions]
            api = ["states:read", "services:call"]
        "#,
    )
    .expect("wrote the manifest");
    let program = package.join("bin/engine");
    std::fs::write(&program, ENGINE).expect("wrote the program");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
        .expect("made it executable");
    let host = ExtensionHost::start_with_packages(
        &core,
        vec![builtin::<Lamp>().expect("valid")],
        Timing::default(),
        packages_dir.path().to_path_buf(),
    )
    .expect("starts");

    let origin = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(irori_core::Event::ServiceCalled { context, .. }) = events.recv().await {
                return context;
            }
        }
    })
    .await
    .expect("the engine's call went out");
    assert_eq!(
        origin.origin,
        Origin::Automation {
            extension: ExtensionId::try_from("engine").expect("valid"),
            run_id: ContextId::try_from(RUN_ID).expect("valid"),
        }
    );
    assert_eq!(
        origin.parent_id,
        Some(ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8JA").expect("valid"))
    );
    host.shutdown().await;
}

/// A lamp whose light is one of its diagnostics: `entity_category` is newer than the first
/// entity format.
struct DiagnosticLamp;
impl Protocol for DiagnosticLamp {
    type Config = NoSettings;
    const MANIFEST: &'static str = r#"
        [extension]
        id = "lamp"
        name = "Lamp"
        version = "0.1.0"
        irori = ">=0.0.0"

        [[contributes.protocol]]
        iot_class = "local_push"
        entity_kinds = ["light"]
    "#;
    async fn run(_: NoSettings, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
        ctx.describe_device(DeviceDescription {
            unique_id: uid("lamp"),
            name: Name::try_from("Lamp")?,
            manufacturer: None,
            model: None,
            sw_version: None,
            hw_version: None,
            serial_number: None,
            suggested_area: None,
            via_device_unique_id: None,
        })
        .await?;
        ctx.describe_entity(EntityDescription {
            unique_id: uid("lamp-light"),
            name: None,
            device_unique_id: Some(uid("lamp")),
            suggested_object_id: None,
            capabilities: Capabilities::Light(LightCapabilities {
                brightness: false,
                color_temp_kelvin: None,
                rgb: false,
            }),
            entity_category: Some(irori_protocol::types::EntityCategory::Diagnostic),
        })
        .await?;
        ctx.stopped().await;
        Ok(())
    }
}

/// An engine installed before entity formats existed says nothing about one, and must be sent
/// only what it was built to read: an entity with a field it doesn't know would fail its whole
/// registry, and the engine would stop.
#[tokio::test]
async fn an_engine_built_before_entity_formats_is_sent_what_it_can_read() {
    use std::os::unix::fs::PermissionsExt;

    let core = Core::new(Arc::new(SystemClock));
    let packages_dir = tempfile::tempdir().expect("temp dir");
    let mut engines = Vec::new();
    for (id, format) in [("old", ""), ("new", "entity_format = 2")] {
        let package = packages_dir.path().join(id);
        std::fs::create_dir_all(package.join("bin")).expect("made the package dir");
        std::fs::write(
            package.join("irori-extension.toml"),
            format!(
                r#"
                    [extension]
                    id = "{id}"
                    name = "Engine"
                    version = "0.1.0"
                    irori = ">=0.0.0"
                    {format}

                    [[contributes.automation]]
                    run = {{ command = "bin/engine" }}

                    [permissions]
                    api = ["registry:read"]
                "#
            ),
        )
        .expect("wrote the manifest");
        let program = package.join("bin/engine");
        std::fs::write(
            &program,
            "#!/bin/sh\nread hello\nsleep 1\necho '{\"type\":\"get_registry\",\"id\":1}'\n\
             read answer; echo \"registry: $answer\" >&2\nwhile read line; do :; done\n",
        )
        .expect("wrote the program");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("made it executable");
        engines.push(ExtensionId::try_from(id).expect("valid"));
    }

    let host = ExtensionHost::start_with_packages(
        &core,
        vec![builtin::<DiagnosticLamp>().expect("valid")],
        Timing::default(),
        packages_dir.path().to_path_buf(),
    )
    .expect("starts");
    let registry_of = |engine: &ExtensionId| {
        core.log(engine).iter().find_map(|line| {
            line.split_once("registry: ")
                .map(|(_, json)| json.to_owned())
        })
    };
    for engine in &engines {
        eventually("the registry is answered", || registry_of(engine).is_some()).await;
    }
    let old = registry_of(&engines[0]).expect("answered");
    let new = registry_of(&engines[1]).expect("answered");
    assert!(old.contains("light.lamp_lamp"), "{old}");
    assert!(!old.contains("entity_category"), "{old}");
    assert!(new.contains(r#""entity_category":"diagnostic""#), "{new}");

    host.shutdown().await;
}

/// An engine that limits what people can do: it asks where two people stand, and about one that
/// isn't in the home, then says two things to write down, one of them not its to say, and waits
/// for its page to ask.
const PEOPLE_ENGINE: &str = r#"#!/bin/sh
read hello
sleep 1
echo '{"type":"get_reach","id":1,"user":"leo"}'
read answer; echo "leo: $answer" >&2
echo '{"type":"get_reach","id":2,"user":"mia"}'
read answer; echo "mia: $answer" >&2
echo '{"type":"get_reach","id":3,"user":"gone"}'
read answer; echo "gone: $answer" >&2
echo '{"type":"note","action":"flow_turned_off","target":"hall_light","detail":"the hall sensor is not in your rooms any more"}'
echo '{"type":"note","action":"person_removed","target":"nico","detail":"forged"}'
n=0
while read line; do echo "page: $line" >&2; echo "{\"type\":\"app_answer\",\"id\":$n,\"value\":{}}"; n=$((n+1)); done
"#;

#[derive(Debug, Default)]
struct Written(std::sync::Mutex<Vec<(String, String, String)>>);

impl irori_core::Notes for Written {
    fn note(&self, _: &ExtensionId, action: &str, target: &str, detail: &str) {
        self.0
            .lock()
            .expect("notes")
            .push((action.into(), target.into(), detail.into()));
    }
}

fn people_package(dir: &std::path::Path, id: &str, users: bool) {
    use std::os::unix::fs::PermissionsExt;
    let package = dir.join(id);
    std::fs::create_dir_all(package.join("bin")).expect("made the package dir");
    std::fs::write(
        package.join("irori-extension.toml"),
        format!(
            r#"
            [extension]
            id = "{id}"
            name = "People"
            version = "0.1.0"
            irori = ">=0.0.0"

            [[contributes.automation]]
            run = {{ command = "bin/engine" }}

            [[contributes.app]]
            label = "People"
            entry = "app/index.html"
            {users}

            [permissions]
            api = ["states:read"]
        "#,
            users = if users { "users = true" } else { "" }
        ),
    )
    .expect("wrote the manifest");
    let program = package.join("bin/engine");
    std::fs::write(&program, PEOPLE_ENGINE).expect("wrote the program");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
        .expect("made it executable");
}

/// A regular user's page is for an extension that says it limits what they can do, and the home
/// trusts it to; only that one is told who asked, or where a person stands, or may say what to
/// write down (`docs/specs/access.md` §4.1).
#[tokio::test]
async fn an_engine_that_limits_people_is_told_who_asks_and_the_others_are_not() {
    use irori_protocol::engine::Caller;
    use irori_types::UserId;

    let core = Core::new(Arc::new(SystemClock));
    let notes = Arc::new(Written::default());
    core.use_notes(notes.clone());
    let person =
        |id: &str, name: &str, rooms: Option<Vec<irori_types::AreaId>>| irori_core::Person {
            id: UserId::try_from(id).expect("valid"),
            name: name.into(),
            owner: false,
            rooms,
        };
    core.apply_people(vec![
        person("leo", "Leo", Some(Vec::new())),
        person("mia", "Mia", None),
    ]);
    let packages_dir = tempfile::tempdir().expect("temp dir");
    people_package(packages_dir.path(), "people", true);
    people_package(packages_dir.path(), "stranger", true);
    let people = ExtensionId::try_from("people").expect("valid");
    let stranger = ExtensionId::try_from("stranger").expect("valid");
    // Both say `users = true`; the home trusts one of them.
    core.trust_with_people([people.clone()]);
    let host = ExtensionHost::start_with_packages(
        &core,
        Vec::new(),
        Timing::default(),
        packages_dir.path().to_path_buf(),
    )
    .expect("starts");
    let said = |who: &ExtensionId, what: &str| core.log(who).iter().any(|line| line.contains(what));

    eventually("the trusted engine heard about the three", || {
        said(&people, "gone: ")
    })
    .await;
    let log = core.log(&people);
    let line = |start: &str| {
        log.iter()
            .find(|line| line.starts_with(start))
            .cloned()
            .unwrap_or_default()
    };
    // Leo has no rooms, so no entities. Mia has the whole home: no list. The third is gone.
    let value = |start: &str| -> serde_json::Value {
        let text = line(start);
        let answer: serde_json::Value =
            serde_json::from_str(text.split_once(": ").map_or("", |(_, rest)| rest)).expect("json");
        answer["value"].clone()
    };
    assert_eq!(
        value("leo: "),
        serde_json::json!({ "here": true, "name": "Leo", "owner": false, "entities": [] })
    );
    assert_eq!(
        value("mia: "),
        serde_json::json!({ "here": true, "name": "Mia", "owner": false })
    );
    assert_eq!(value("gone: ")["here"], false);
    // Of what it said to write down, the log took what is an engine's to say.
    eventually("the note is written", || {
        !notes.0.lock().expect("notes").is_empty()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let written = notes.0.lock().expect("notes").clone();
    assert_eq!(written.len(), 1, "{written:?}");
    assert_eq!(written[0].0, "flow_turned_off");

    // The stranger isn't trusted: it is told it isn't one that limits people, and writes nothing.
    eventually("the stranger asked", || said(&stranger, "gone: ")).await;
    assert!(
        said(&stranger, "isn't one that limits what people can do"),
        "{:?}",
        core.log(&stranger)
    );

    // A regular user's question reaches the trusted engine with who asked on it; the owner's
    // carries no one. The stranger is never sent a regular user's question.
    let leo = Caller {
        user: Some(UserId::try_from("leo").expect("valid")),
        owner: false,
    };
    core.app_request(
        &people,
        "flows.list".into(),
        serde_json::json!({}),
        leo.clone(),
    )
    .await
    .expect("answered");
    assert!(
        said(&people, r#""caller":{"user":"leo","owner":false}"#),
        "{:?}",
        core.log(&people)
    );
    core.app_request(
        &people,
        "flows.list".into(),
        serde_json::json!({}),
        Caller::owner(),
    )
    .await
    .expect("answered");
    let asked = core
        .log(&people)
        .iter()
        .filter(|line| line.starts_with("page: "))
        .count();
    assert_eq!(asked, 2);
    assert!(
        core.log(&people)
            .iter()
            .filter(|line| line.starts_with("page: "))
            .any(|line| !line.contains("caller")),
        "the owner's question says nobody"
    );
    let refused = core
        .app_request(&stranger, "flows.list".into(), serde_json::json!({}), leo)
        .await
        .expect_err("not for regular users");
    assert!(
        refused.to_string().contains("not for regular users"),
        "{refused}"
    );
    assert!(!said(&stranger, "page: "), "{:?}", core.log(&stranger));

    // Changing who is in the home tells engines, as any change to the home does.
    let mut events = core.subscribe();
    core.apply_people(vec![person("mia", "Mia", None)]);
    assert!(matches!(
        events.try_recv(),
        Ok(irori_core::Event::SettingsChanged)
    ));
    core.apply_people(vec![person("mia", "Mia", None)]);
    assert!(events.try_recv().is_err(), "nothing changed");
    assert!(
        !core
            .person_reach(&UserId::try_from("leo").expect("valid"))
            .here
    );

    host.shutdown().await;
}
