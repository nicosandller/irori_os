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
        .app_request(&engine, "flows.list".into(), serde_json::json!({}))
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
