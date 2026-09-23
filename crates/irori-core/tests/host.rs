//! The extension host end to end: protocols started, fed into the core, called, supervised.
//! Time is paused, so waits of seconds or minutes run instantly.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use irori_core::{
    CallError, Command, Core, Event, ExtensionHost, ExtensionStatus, SystemClock, Timing,
};
use irori_protocol::types::{
    Availability, Capabilities, Context, ContextId, DeviceDescription, EntityDescription, EntityId,
    ExtensionId, LightCapabilities, LightState, LightTurnOn, Name, Origin, Service, State,
    StateReport, UniqueId, UserId, Version,
};
use irori_protocol::{Health, NoSettings, Protocol, ProtocolContext, ProtocolError, builtin};

fn uid(s: &str) -> UniqueId {
    UniqueId::try_from(s).expect("valid")
}

/// The lamp's entity id under the extension that described it: ids are made from the device's
/// id, which is the protocol and its handle for the device (ROADMAP D36).
fn lamp_id(protocol: &str) -> EntityId {
    EntityId::try_from(format!("light.{protocol}_lamp")).expect("valid")
}

async fn describe_lamp(ctx: &ProtocolContext) -> Result<(), ProtocolError> {
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
            brightness: true,
            color_temp_kelvin: None,
            rgb: false,
        }),
    })
    .await?;
    Ok(())
}

fn light(on: bool, brightness: Option<u8>, caused_by: Option<ContextId>) -> StateReport {
    StateReport {
        unique_id: uid("lamp-light"),
        state: Some(State::Light(LightState {
            on,
            brightness,
            color_mode: None,
            color_temp_kelvin: None,
            rgb: None,
        })),
        attributes: BTreeMap::new(),
        caused_by,
    }
}

/// Someone using the UI.
fn user_context() -> Context {
    Context {
        id: ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9").expect("valid"),
        parent_id: None,
        origin: Origin::User {
            user_id: UserId::try_from("nico").expect("valid"),
        },
    }
}

/// Polls until `check` passes, letting paused time advance.
async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..10_000 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for: {what}");
}

fn status(core: &Core, id: &str) -> Option<ExtensionStatus> {
    core.extensions()
        .get(&ExtensionId::try_from(id).expect("valid"))
        .map(|overview| overview.status.clone())
}

fn start(core: &Core, builtin: irori_protocol::Builtin) -> ExtensionHost {
    ExtensionHost::start(core, vec![builtin], Timing::default()).expect("unique ids")
}

// --- A well-behaved lamp ------------------------------------------------------------------

struct Lamp;
impl Protocol for Lamp {
    type Config = NoSettings;
    const MANIFEST: &'static str = LAMP_MANIFEST;
    async fn run(_: NoSettings, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
        describe_lamp(&ctx).await?;
        ctx.report_state(light(true, Some(100), None));
        while let Some(incoming) = ctx.next_call().await {
            let caused_by = Some(incoming.call.context.id.clone());
            let on = matches!(incoming.call.service, Service::LightTurnOn(_));
            incoming.reply(Ok(()));
            ctx.report_state(light(on, Some(100), caused_by));
        }
        // On the way out: the core must still take this in.
        ctx.report_state(light(false, Some(7), None));
        Ok(())
    }
}
const LAMP_MANIFEST: &str = r#"
    [extension]
    id = "lamp"
    name = "Lamp"
    version = "0.1.0"
    irori = ">=0.0.0"

    [[contributes.protocol]]
    iot_class = "local_push"
    entity_kinds = ["light"]
"#;

#[tokio::test(start_paused = true)]
async fn devices_appear_and_commands_round_trip_with_their_context() {
    let core = Core::new(Arc::new(SystemClock));
    let mut events = core.subscribe();
    let host = start(&core, builtin::<Lamp>().expect("valid"));

    eventually("the lamp reports it's on", || {
        core.state(&lamp_id("lamp"))
            .and_then(|s| s.state)
            .is_some_and(|s| matches!(s, State::Light(LightState { on: true, .. })))
    })
    .await;
    assert_eq!(status(&core, "lamp"), Some(ExtensionStatus::Running));
    assert_eq!(core.devices().len(), 1);

    let context = user_context();
    core.call_service(&lamp_id("lamp"), Command::Toggle, context.clone())
        .await
        .expect("toggle works");
    eventually("the lamp reports it's off", || {
        core.state(&lamp_id("lamp"))
            .and_then(|s| s.state)
            .is_some_and(|s| matches!(s, State::Light(LightState { on: false, .. })))
    })
    .await;
    // The change points back to the person who asked.
    let state = core.state(&lamp_id("lamp")).expect("state");
    assert_eq!(state.context.parent_id, Some(context.id));

    let mut saw_call = false;
    while let Ok(event) = events.try_recv() {
        saw_call |= matches!(event, Event::ServiceCalled { .. });
    }
    assert!(saw_call, "a ServiceCalled event was published");

    let err = core
        .call_service(
            &lamp_id("lamp"),
            Command::TurnOn(LightTurnOn {
                rgb: Some([1, 2, 3]),
                ..LightTurnOn::default()
            }),
            user_context(),
        )
        .await
        .expect_err("no rgb");
    assert!(matches!(err, CallError::NotSupported(_)), "{err}");

    host.shutdown().await;
    assert_eq!(status(&core, "lamp"), Some(ExtensionStatus::Disabled));
    let state = core.state(&lamp_id("lamp")).expect("kept");
    assert_eq!(state.availability, Availability::Unavailable);
    // What it reported while shutting down was applied, and survives going offline.
    assert_eq!(
        state.state,
        Some(State::Light(LightState {
            on: false,
            brightness: Some(7),
            color_mode: None,
            color_temp_kelvin: None,
            rgb: None,
        }))
    );
}

/// A lamp like a real one: it accepts the command at once, but its new value only arrives a
/// moment later.
struct SlowLamp;
impl Protocol for SlowLamp {
    type Config = NoSettings;
    const MANIFEST: &'static str = r#"
        [extension]
        id = "slow_lamp"
        name = "Slow lamp"
        version = "0.1.0"
        irori = ">=0.0.0"

        [[contributes.protocol]]
        iot_class = "local_push"
        entity_kinds = ["light"]
    "#;
    async fn run(_: NoSettings, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
        describe_lamp(&ctx).await?;
        ctx.report_state(light(true, None, None));
        while let Some(incoming) = ctx.next_call().await {
            let caused_by = Some(incoming.call.context.id.clone());
            let on = matches!(incoming.call.service, Service::LightTurnOn(_));
            incoming.reply(Ok(()));
            tokio::time::sleep(Duration::from_millis(200)).await;
            ctx.report_state(light(on, None, caused_by));
        }
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn two_toggles_at_once_cancel_each_other_out() {
    let core = Core::new(Arc::new(SystemClock));
    let host = start(&core, builtin::<SlowLamp>().expect("valid"));
    let on = || {
        core.state(&lamp_id("slow_lamp"))
            .and_then(|s| s.state)
            .is_some_and(|s| matches!(s, State::Light(LightState { on: true, .. })))
    };
    eventually("the lamp is on", on).await;

    // Two people press toggle at the same moment, before the lamp has confirmed the first:
    // off, then on again.
    let lamp = lamp_id("slow_lamp");
    let (first, second) = tokio::join!(
        core.call_service(&lamp, Command::Toggle, user_context()),
        core.call_service(&lamp, Command::Toggle, user_context()),
    );
    first.expect("first toggle");
    second.expect("second toggle");

    // Both confirmations land, and the lamp ends up as it started.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(on(), "two toggles should cancel out");
    host.shutdown().await;
}

/// A lamp whose commands always fail, counting what it was asked to do.
static TURN_OFFS: AtomicUsize = AtomicUsize::new(0);

struct BrokenLamp;
impl Protocol for BrokenLamp {
    type Config = NoSettings;
    const MANIFEST: &'static str = r#"
        [extension]
        id = "broken_lamp"
        name = "Broken lamp"
        version = "0.1.0"
        irori = ">=0.0.0"

        [[contributes.protocol]]
        iot_class = "local_push"
        entity_kinds = ["light"]
    "#;
    async fn run(_: NoSettings, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
        describe_lamp(&ctx).await?;
        ctx.report_state(light(true, None, None));
        while let Some(incoming) = ctx.next_call().await {
            if matches!(incoming.call.service, Service::LightTurnOff) {
                TURN_OFFS.fetch_add(1, Ordering::SeqCst);
            }
            incoming.reply(Err(irori_protocol::ServiceError::unavailable(
                "the lamp is unplugged",
            )));
        }
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn a_failed_command_doesnt_change_what_the_core_thinks() {
    let core = Core::new(Arc::new(SystemClock));
    let host = start(&core, builtin::<BrokenLamp>().expect("valid"));
    eventually("the lamp is on", || {
        core.state(&lamp_id("broken_lamp"))
            .and_then(|s| s.state)
            .is_some_and(|s| matches!(s, State::Light(LightState { on: true, .. })))
    })
    .await;

    // Both toggles see a lamp that's still on, so both try to turn it off.
    for _ in 0..2 {
        let err = core
            .call_service(&lamp_id("broken_lamp"), Command::Toggle, user_context())
            .await
            .expect_err("the lamp is unplugged");
        assert!(matches!(err, CallError::Unavailable(_)), "{err}");
    }
    assert_eq!(TURN_OFFS.load(Ordering::SeqCst), 2);
    host.shutdown().await;
}

// --- Crashes and restarts -----------------------------------------------------------------

static CRASHY_RUNS: AtomicUsize = AtomicUsize::new(0);

struct Crashy;
impl Protocol for Crashy {
    type Config = NoSettings;
    const MANIFEST: &'static str = CRASHY_MANIFEST;
    async fn run(_: NoSettings, ctx: ProtocolContext) -> Result<(), ProtocolError> {
        describe_lamp(&ctx).await?;
        ctx.report_state(light(true, None, None));
        let run = CRASHY_RUNS.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        if run < 3 {
            panic!("boom {run}");
        }
        let mut ctx = ctx;
        ctx.stopped().await;
        Ok(())
    }
}
const CRASHY_MANIFEST: &str = r#"
    [extension]
    id = "crashy"
    name = "Crashy"
    version = "0.1.0"
    irori = ">=0.0.0"

    [[contributes.protocol]]
    iot_class = "local_push"
    entity_kinds = ["light"]
"#;

#[tokio::test(start_paused = true)]
async fn a_crashing_protocol_is_restarted_with_growing_delays() {
    let core = Core::new(Arc::new(SystemClock));
    let host = start(&core, builtin::<Crashy>().expect("valid"));

    eventually("first crash", || {
        matches!(
            status(&core, "crashy"),
            Some(ExtensionStatus::Failed { .. })
        )
    })
    .await;
    match status(&core, "crashy") {
        Some(ExtensionStatus::Failed { reason, retry_at }) => {
            assert_eq!(reason, "crashed: boom 0");
            assert!(retry_at.is_some());
        }
        other => panic!("expected failed, got {other:?}"),
    }
    assert_eq!(
        core.state(&lamp_id("crashy")).expect("kept").availability,
        Availability::Unavailable
    );

    let started = tokio::time::Instant::now();
    eventually("it keeps running after three crashes", || {
        CRASHY_RUNS.load(Ordering::SeqCst) == 4
            && status(&core, "crashy") == Some(ExtensionStatus::Running)
    })
    .await;
    // Waits of 1 s, 2 s, and 4 s between the four runs (plus 0.1 s per run).
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_secs(6) && waited < Duration::from_secs(8),
        "{waited:?}"
    );
    eventually("available again", || {
        core.state(&lamp_id("crashy"))
            .is_some_and(|s| s.availability == Availability::Available)
    })
    .await;

    host.shutdown().await;
}

static EAGER_STARTS: AtomicUsize = AtomicUsize::new(0);

/// Panics before it even returns its future.
struct PanicsOnStart;
impl Protocol for PanicsOnStart {
    type Config = NoSettings;
    const MANIFEST: &'static str = r#"
        [extension]
        id = "eager"
        name = "Eager"
        version = "0.1.0"
        irori = ">=0.0.0"

        [[contributes.protocol]]
        iot_class = "local_push"
        entity_kinds = ["light"]
    "#;
    #[allow(clippy::manual_async_fn)]
    fn run(
        _: NoSettings,
        _: ProtocolContext,
    ) -> impl std::future::Future<Output = Result<(), ProtocolError>> + Send {
        EAGER_STARTS.fetch_add(1, Ordering::SeqCst);
        panic!("bad wiring");
        #[allow(unreachable_code)]
        async {
            Ok(())
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_panic_while_starting_is_a_crash_that_gets_retried() {
    let core = Core::new(Arc::new(SystemClock));
    let host = start(&core, builtin::<PanicsOnStart>().expect("valid"));
    eventually("failed with a retry", || {
        matches!(
            status(&core, "eager"),
            Some(ExtensionStatus::Failed { ref reason, retry_at: Some(_) })
                if reason == "crashed while starting: bad wiring"
        )
    })
    .await;
    eventually("tried again", || EAGER_STARTS.load(Ordering::SeqCst) >= 2).await;
    host.shutdown().await;
    assert_eq!(status(&core, "eager"), Some(ExtensionStatus::Disabled));
}

// --- Stopping -----------------------------------------------------------------------------

struct Stubborn;
impl Protocol for Stubborn {
    type Config = NoSettings;
    const MANIFEST: &'static str = STUBBORN_MANIFEST;
    async fn run(_: NoSettings, ctx: ProtocolContext) -> Result<(), ProtocolError> {
        let _keep = ctx;
        // Ignores the request to stop.
        std::future::pending::<()>().await;
        Ok(())
    }
}
const STUBBORN_MANIFEST: &str = r#"
    [extension]
    id = "stubborn"
    name = "Stubborn"
    version = "0.1.0"
    irori = ">=0.0.0"

    [[contributes.protocol]]
    iot_class = "local_push"
    entity_kinds = ["light"]
"#;

#[tokio::test(start_paused = true)]
async fn a_protocol_that_ignores_stop_is_cancelled_after_the_grace_period() {
    let core = Core::new(Arc::new(SystemClock));
    let host = start(&core, builtin::<Stubborn>().expect("valid"));
    eventually("running", || {
        status(&core, "stubborn") == Some(ExtensionStatus::Running)
    })
    .await;
    let started = tokio::time::Instant::now();
    host.shutdown().await;
    assert_eq!(started.elapsed(), Duration::from_secs(5));
    assert_eq!(status(&core, "stubborn"), Some(ExtensionStatus::Disabled));
}

// --- Calls that never come back, health, versions -----------------------------------------

struct Silent;
impl Protocol for Silent {
    type Config = NoSettings;
    const MANIFEST: &'static str = SILENT_MANIFEST;
    async fn run(_: NoSettings, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
        describe_lamp(&ctx).await?;
        ctx.set_health(Health::Degraded("1 of 1 lamps is sulking".into()))
            .await;
        let mut held = Vec::new();
        while let Some(incoming) = ctx.next_call().await {
            held.push(incoming); // never replies
        }
        Ok(())
    }
}
const SILENT_MANIFEST: &str = r#"
    [extension]
    id = "silent"
    name = "Silent"
    version = "0.1.0"
    irori = ">=0.0.0"

    [[contributes.protocol]]
    iot_class = "local_push"
    entity_kinds = ["light"]
"#;

#[tokio::test(start_paused = true)]
async fn calls_time_out_and_health_is_shown() {
    let core = Core::new(Arc::new(SystemClock));
    let host = start(&core, builtin::<Silent>().expect("valid"));
    eventually("degraded", || {
        matches!(
            status(&core, "silent"),
            Some(ExtensionStatus::Degraded { .. })
        )
    })
    .await;

    let started = tokio::time::Instant::now();
    // Two calls on the same entity: the second queues behind the first, and the ten seconds
    // cover the wait as well, rather than ten seconds each.
    let lamp = lamp_id("silent");
    let (first, second) = tokio::join!(
        core.call_service(&lamp, Command::TurnOff, user_context()),
        core.call_service(&lamp, Command::TurnOff, user_context()),
    );
    assert_eq!(first.expect_err("no answer"), CallError::Timeout);
    assert_eq!(second.expect_err("no answer"), CallError::Timeout);
    assert_eq!(started.elapsed(), Duration::from_secs(10));
    host.shutdown().await;

    let err = core
        .call_service(&lamp_id("silent"), Command::TurnOff, user_context())
        .await
        .expect_err("stopped");
    assert!(matches!(err, CallError::NotRunning(_)), "{err}");
}

struct FromTheFuture;
impl Protocol for FromTheFuture {
    type Config = NoSettings;
    const MANIFEST: &'static str = r#"
        [extension]
        id = "future"
        name = "Future"
        version = "0.1.0"
        irori = ">=9.0.0"

        [[contributes.protocol]]
        iot_class = "local_push"
        entity_kinds = ["light"]
    "#;
    async fn run(_: NoSettings, _: ProtocolContext) -> Result<(), ProtocolError> {
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn incompatible_extensions_fail_without_retrying() {
    let core = Core::with_version(
        Arc::new(SystemClock),
        Version::try_from("0.1.0").expect("valid"),
    );
    let host = start(&core, builtin::<FromTheFuture>().expect("valid"));
    eventually("failed", || status(&core, "future").is_some()).await;
    assert_eq!(
        status(&core, "future"),
        Some(ExtensionStatus::Failed {
            reason: "requires Irori >=9.0.0, this is 0.1.0".into(),
            retry_at: None,
        })
    );
    host.shutdown().await;
}

// --- Lost reports are counted ------------------------------------------------------------

struct Confused;
impl Protocol for Confused {
    type Config = NoSettings;
    const MANIFEST: &'static str = r#"
        [extension]
        id = "confused"
        name = "Confused"
        version = "0.1.0"
        irori = ">=0.0.0"

        [[contributes.protocol]]
        iot_class = "local_push"
        entity_kinds = ["light"]
    "#;
    async fn run(_: NoSettings, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
        // Reports for an entity it never described.
        ctx.report_state(light(true, None, None));
        ctx.stopped().await;
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn rejected_reports_are_counted_per_extension() {
    let core = Core::new(Arc::new(SystemClock));
    let host = start(&core, builtin::<Confused>().expect("valid"));
    let id = ExtensionId::try_from("confused").expect("valid");
    eventually("the rejection is counted", || {
        core.extensions()
            .get(&id)
            .is_some_and(|overview| overview.rejected_reports == 1)
    })
    .await;
    let json = serde_json::to_value(&core.extensions()[&id]).expect("serializes");
    assert_eq!(json["state"], "running");
    assert_eq!(json["rejected_reports"], 1);
    assert_eq!(json["dropped_reports"], 0);
    host.shutdown().await;
}

#[tokio::test]
async fn unrepresentable_timings_are_refused() {
    let core = Core::new(Arc::new(SystemClock));
    let huge = Timing {
        max_retry: Duration::MAX,
        ..Timing::default()
    };
    let err = ExtensionHost::start(&core, vec![], huge).expect_err("too long");
    assert_eq!(err, "timing: max_retry must be at most 24 hours");
    let zero = Timing {
        first_retry: Duration::ZERO,
        ..Timing::default()
    };
    assert!(ExtensionHost::start(&core, vec![], zero).is_err());
}

/// A package left on disk from before an extension became a builtin (helpers, D45) must not
/// crash the next start: the id collision is between old state and new code, not a bug to
/// refuse to boot over.
#[tokio::test]
async fn a_stale_package_sharing_a_builtins_id_is_skipped_not_fatal() {
    let core = Core::new(Arc::new(SystemClock));
    let packages_dir = tempfile::tempdir().expect("temp dir");
    let stale = packages_dir.path().join("lamp");
    std::fs::create_dir_all(&stale).expect("made the stale package dir");
    std::fs::write(stale.join("irori-extension.toml"), LAMP_MANIFEST).expect("wrote the manifest");

    let host = ExtensionHost::start_with_packages(
        &core,
        vec![builtin::<Lamp>().expect("valid")],
        Timing::default(),
        packages_dir.path().to_path_buf(),
    )
    .expect("a stale package on disk must not fail startup");

    eventually(
        "the builtin runs despite the stale package sharing its id",
        || status(&core, "lamp") == Some(ExtensionStatus::Running),
    )
    .await;

    host.shutdown().await;
}

/// A package whose own manifest is broken — not sharing anyone's id — still needs to say so on
/// the Extensions page, rather than looking like it was never installed at all.
#[tokio::test]
async fn a_broken_packages_own_manifest_reports_failed_not_missing() {
    let core = Core::new(Arc::new(SystemClock));
    let packages_dir = tempfile::tempdir().expect("temp dir");
    let broken = packages_dir.path().join("broken");
    std::fs::create_dir_all(&broken).expect("made the package dir");
    std::fs::write(
        broken.join("irori-extension.toml"),
        "this is not valid toml{{{",
    )
    .expect("wrote a broken manifest");

    let host = ExtensionHost::start_with_packages(
        &core,
        vec![],
        Timing::default(),
        packages_dir.path().to_path_buf(),
    )
    .expect("a broken package on disk must not fail startup");

    let id = ExtensionId::try_from("broken").expect("valid");
    eventually(
        "the broken package is reported failed, not silently absent",
        || {
            matches!(
                status(&core, "broken"),
                Some(ExtensionStatus::Failed { .. })
            )
        },
    )
    .await;
    let overview = core.extensions().get(&id).cloned().expect("present");
    assert!(
        matches!(&overview.status, ExtensionStatus::Failed { reason, .. } if !reason.is_empty()),
        "{overview:?}"
    );

    host.shutdown().await;
}

/// A manifest that's fine on its own but names a `config_schema` that isn't there (or isn't
/// valid JSON) mustn't start with settings quietly unchecked and its form quietly hidden — it's
/// rejected the same way a manifest missing `run.command` already is. `run.command` here never
/// actually has to run: the schema is loaded, and this package rejected, before anything would
/// spawn it.
#[tokio::test]
async fn a_package_naming_a_missing_config_schema_reports_failed_naming_it() {
    let core = Core::new(Arc::new(SystemClock));
    let packages_dir = tempfile::tempdir().expect("temp dir");
    let broken = packages_dir.path().join("brokenschema");
    std::fs::create_dir_all(&broken).expect("made the package dir");
    std::fs::write(
        broken.join("irori-extension.toml"),
        r#"
            [extension]
            id = "brokenschema"
            name = "Broken Schema"
            version = "0.1.0"
            irori = ">=0.0.0"
            config_schema = "config.schema.json"

            [[contributes.protocol]]
            iot_class = "local_push"
            entity_kinds = ["light"]
            run = { command = "bin/never-actually-run" }
        "#,
    )
    .expect("wrote the manifest");
    // Deliberately not written: `load_config_schema` should fail before `run.command` matters.

    let host = ExtensionHost::start_with_packages(
        &core,
        vec![],
        Timing::default(),
        packages_dir.path().to_path_buf(),
    )
    .expect("a broken package on disk must not fail startup");

    eventually(
        "the missing schema is reported failed, naming the path",
        || {
            matches!(
                status(&core, "brokenschema"),
                Some(ExtensionStatus::Failed { reason, .. }) if reason.contains("config.schema.json")
            )
        },
    )
    .await;

    host.shutdown().await;
}

#[test]
fn duplicate_extension_ids_are_refused() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let core = Core::new(Arc::new(SystemClock));
        let err = ExtensionHost::start(
            &core,
            vec![
                builtin::<Lamp>().expect("valid"),
                builtin::<Lamp>().expect("valid"),
            ],
            Timing::default(),
        )
        .expect_err("same id twice");
        assert_eq!(err, "two extensions share the id `lamp`");
    });
}

// --- Settings, and what's waiting ----------------------------------------------------------

/// Records every set of settings it was started with, and says what it's waiting for.
struct Keyed;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct KeyedSettings {
    #[serde(default)]
    key: Option<String>,
}

static STARTED_WITH: std::sync::Mutex<Vec<Option<String>>> = std::sync::Mutex::new(Vec::new());

impl Protocol for Keyed {
    type Config = KeyedSettings;
    const MANIFEST: &'static str = KEYED_MANIFEST;
    async fn run(settings: KeyedSettings, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
        STARTED_WITH
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(settings.key.clone());
        if settings.key.is_none() {
            ctx.set_waiting(vec![irori_protocol::types::Waiting {
                unique_id: uid("locked"),
                name: Name::try_from("Locked box")?,
                reason: "it wants a key".into(),
                secret: Some(irori_protocol::types::SecretRequest {
                    path: vec!["key".into()],
                    label: "Key".into(),
                    hint: None,
                }),
            }])
            .await;
        }
        ctx.stopped().await;
        Ok(())
    }
}
const KEYED_MANIFEST: &str = r#"
    [extension]
    id = "keyed"
    name = "Keyed"
    version = "0.1.0"
    irori = ">=0.0.0"

    [[contributes.protocol]]
    iot_class = "local_push"
    entity_kinds = ["light"]
"#;

fn keyed_settings(
    extension: &str,
    table: serde_json::Value,
) -> irori_protocol::types::ExtensionSettings {
    let serde_json::Value::Object(table) = table else {
        panic!("a table");
    };
    irori_protocol::types::ExtensionSettings::new(
        [(ExtensionId::try_from(extension).expect("valid"), table)].into(),
    )
}

fn waiting(core: &Core) -> usize {
    core.extensions()
        .get(&ExtensionId::try_from("keyed").expect("valid"))
        .map_or(0, |overview| overview.waiting.len())
}

fn started_with() -> Vec<Option<String>> {
    STARTED_WITH
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The whole path a key takes: an extension says what it's waiting for, a key arrives in its
/// settings, and it's restarted with the key and stops waiting — without anyone restarting Irori.
/// A change to some other extension's settings doesn't disturb it.
///
/// One test rather than several because the recorder is shared, process-wide state.
#[tokio::test(start_paused = true)]
async fn new_settings_restart_only_their_own_extension_and_what_was_waiting_clears() {
    let core = Core::new(Arc::new(SystemClock));
    let host = start(&core, builtin::<Keyed>().expect("valid"));

    eventually(
        "it starts with no key and says what it's waiting for",
        || started_with() == [None] && waiting(&core) == 1,
    )
    .await;

    // Another extension's settings: no reason to restart this one.
    core.apply_extension_settings(keyed_settings("demo", serde_json::json!({"x": "y"})));
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        started_with(),
        [None],
        "restarted over somebody else's settings"
    );

    core.apply_extension_settings(keyed_settings("keyed", serde_json::json!({"key": "k"})));
    eventually("it's restarted with the key and stops waiting", || {
        started_with() == [None, Some("k".into())]
            && waiting(&core) == 0
            && status(&core, "keyed") == Some(ExtensionStatus::Running)
    })
    .await;

    // Settings it can't accept: failed, not retried in a loop — and not given up on either.
    core.apply_extension_settings(keyed_settings("keyed", serde_json::json!({"nope": 1})));
    eventually("bad settings fail it", || {
        matches!(
            status(&core, "keyed"),
            Some(ExtensionStatus::Failed { retry_at: None, .. })
        )
    })
    .await;
    core.apply_extension_settings(keyed_settings("keyed", serde_json::json!({"key": "k2"})));
    eventually("fixing them starts it again", || {
        started_with().last() == Some(&Some("k2".into()))
            && status(&core, "keyed") == Some(ExtensionStatus::Running)
    })
    .await;

    host.shutdown().await;
    assert_eq!(waiting(&core), 0);
}

/// Needs a setting to exist at all: its `Config` has a field with no default, so its own
/// deserialization would fail if it were ever started without one.
struct Needy;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct NeedySettings {
    #[allow(dead_code)]
    port: String,
}

static NEEDY_RUNS: AtomicUsize = AtomicUsize::new(0);

impl Protocol for Needy {
    type Config = NeedySettings;
    const MANIFEST: &'static str = NEEDY_MANIFEST;
    async fn run(_: NeedySettings, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
        NEEDY_RUNS.fetch_add(1, Ordering::SeqCst);
        ctx.stopped().await;
        Ok(())
    }
}
const NEEDY_MANIFEST: &str = r#"
    [extension]
    id = "needy"
    name = "Needy"
    version = "0.1.0"
    irori = ">=0.0.0"

    [[contributes.protocol]]
    iot_class = "local_push"
    entity_kinds = ["light"]
"#;

/// A required setting that isn't set is a person's job, not a crash: the extension is never
/// started, so it never fails and never retries — it waits, saying which setting it wants, and
/// starts as soon as that arrives.
#[tokio::test(start_paused = true)]
async fn an_extension_missing_a_required_setting_waits_for_it_instead_of_crash_looping() {
    let core = Core::new(Arc::new(SystemClock));
    let host = start(&core, builtin::<Needy>().expect("valid"));

    eventually("it says which setting it needs", || {
        status(&core, "needy")
            == Some(ExtensionStatus::NeedsSetup {
                missing: vec!["port".to_owned()],
            })
    })
    .await;
    // Long enough for several rounds of retry backoff, had it been retrying at all.
    tokio::time::sleep(Duration::from_secs(600)).await;
    assert_eq!(
        NEEDY_RUNS.load(Ordering::SeqCst),
        0,
        "started without the setting it can't run without"
    );

    core.apply_extension_settings(keyed_settings(
        "needy",
        serde_json::json!({"port": "/dev/ttyUSB0"}),
    ));
    eventually("the setting arriving starts it", || {
        status(&core, "needy") == Some(ExtensionStatus::Running)
    })
    .await;
    assert_eq!(NEEDY_RUNS.load(Ordering::SeqCst), 1);

    host.shutdown().await;
}

static RETRY_RUNS: AtomicUsize = AtomicUsize::new(0);

/// Crashes until it has a token, so a settings change during the retry wait can be seen.
struct CrashUntilKeyed;
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CrashUntilKeyedSettings {
    #[serde(default)]
    key: Option<String>,
}
impl Protocol for CrashUntilKeyed {
    type Config = CrashUntilKeyedSettings;
    const MANIFEST: &'static str = r#"
        [extension]
        id = "crash_until_keyed"
        name = "Crash until keyed"
        version = "0.1.0"
        irori = ">=0.0.0"

        [[contributes.protocol]]
        iot_class = "local_push"
        entity_kinds = ["light"]
    "#;
    async fn run(
        settings: CrashUntilKeyedSettings,
        mut ctx: ProtocolContext,
    ) -> Result<(), ProtocolError> {
        RETRY_RUNS.fetch_add(1, Ordering::SeqCst);
        if settings.key.is_none() {
            panic!("no key");
        }
        ctx.stopped().await;
        Ok(())
    }
}

/// Changing settings during the crash backoff must restart now, not after the current delay
/// (`docs/specs/protocols.md` §3 step 6).
#[tokio::test(start_paused = true)]
async fn a_settings_change_during_retry_restarts_without_waiting_out_the_delay() {
    let core = Core::new(Arc::new(SystemClock));
    let host = ExtensionHost::start(
        &core,
        vec![builtin::<CrashUntilKeyed>().expect("valid")],
        Timing {
            first_retry: Duration::from_secs(300),
            max_retry: Duration::from_secs(300),
            healthy_after: Duration::from_secs(600),
            stop_grace: Duration::from_secs(1),
        },
    )
    .expect("unique ids");

    eventually("crashed", || {
        RETRY_RUNS.load(Ordering::SeqCst) >= 1
            && matches!(
                status(&core, "crash_until_keyed"),
                Some(ExtensionStatus::Failed {
                    retry_at: Some(_),
                    ..
                })
            )
    })
    .await;
    let after_crash = RETRY_RUNS.load(Ordering::SeqCst);

    core.apply_extension_settings(keyed_settings(
        "crash_until_keyed",
        serde_json::json!({"key": "k"}),
    ));
    let started = tokio::time::Instant::now();
    eventually("restarted with the key", || {
        RETRY_RUNS.load(Ordering::SeqCst) > after_crash
            && status(&core, "crash_until_keyed") == Some(ExtensionStatus::Running)
    })
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "waited out the backoff: {:?}",
        started.elapsed()
    );

    host.shutdown().await;
}

/// Turning an extension off stops it and says so; turning it back on starts it again with its
/// devices. Neither counts as a failure, and turning off one leaves the others alone.
#[tokio::test(start_paused = true)]
async fn an_extension_can_be_turned_off_and_on_while_irori_runs() {
    let core = Core::new(Arc::new(SystemClock));
    let host = ExtensionHost::start(
        &core,
        vec![builtin::<Lamp>().expect("valid")],
        Timing::default(),
    )
    .expect("unique ids");
    eventually("the lamp is on", || core.state(&lamp_id("lamp")).is_some()).await;

    core.apply_disabled_extensions([ExtensionId::try_from("lamp").expect("valid")].into());
    eventually("turned off", || {
        status(&core, "lamp") == Some(ExtensionStatus::Disabled)
            && core
                .state(&lamp_id("lamp"))
                .is_some_and(|state| state.availability == Availability::Unavailable)
    })
    .await;

    core.apply_disabled_extensions(Default::default());
    eventually("turned back on, and its lamp with it", || {
        status(&core, "lamp") == Some(ExtensionStatus::Running)
            && core
                .state(&lamp_id("lamp"))
                .is_some_and(|state| state.availability == Availability::Available)
    })
    .await;
    host.shutdown().await;
}

/// An extension named in `irori.toml` never starts at all.
#[tokio::test(start_paused = true)]
async fn an_extension_turned_off_from_the_start_never_starts() {
    let core = Core::new(Arc::new(SystemClock));
    core.apply_disabled_extensions([ExtensionId::try_from("lamp").expect("valid")].into());
    let host = start(&core, builtin::<Lamp>().expect("valid"));
    eventually("disabled", || {
        status(&core, "lamp") == Some(ExtensionStatus::Disabled)
    })
    .await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(core.devices().is_empty(), "it described devices anyway");
    host.shutdown().await;
}

// --- Stored values ----------------------------------------------------------------------------

/// Counts its own starts in storage, and tries a value that's too big.
struct Counter;

static COUNTED: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());
static TOO_BIG: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

impl Protocol for Counter {
    type Config = NoSettings;
    const MANIFEST: &'static str = r#"
        [extension]
        id = "counter"
        name = "Counter"
        version = "0.1.0"
        irori = ">=0.0.0"

        [[contributes.protocol]]
        iot_class = "local_push"
        entity_kinds = ["switch"]
    "#;
    async fn run(_: NoSettings, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
        let starts = ctx
            .load("starts")
            .await?
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
            + 1;
        ctx.store("starts", serde_json::json!(starts)).await?;
        COUNTED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(starts);
        let huge = serde_json::json!("x".repeat(irori_protocol::MAX_STORED_VALUE));
        let refused = ctx.store("huge", huge).await.err().map(|e| e.to_string());
        *TOO_BIG
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = refused;
        ctx.stopped().await;
        Ok(())
    }
}

/// A stored value is there when the protocol starts again; one over the limit is refused
/// with a reason rather than silently cut.
#[tokio::test(start_paused = true)]
async fn stored_values_outlast_a_restart_and_have_a_size_limit() {
    let core = Core::new(Arc::new(SystemClock));
    let host = start(&core, builtin::<Counter>().expect("valid"));
    let counted = || {
        COUNTED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    };
    eventually("started once", || counted() == [1]).await;

    core.apply_disabled_extensions([ExtensionId::try_from("counter").expect("valid")].into());
    eventually("stopped", || {
        status(&core, "counter") == Some(ExtensionStatus::Disabled)
    })
    .await;
    core.apply_disabled_extensions(Default::default());
    eventually("started again, remembering", || counted() == [1, 2]).await;

    let refused = TOO_BIG
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .expect("the oversized value was refused");
    assert!(refused.contains("at most"), "{refused}");
    host.shutdown().await;
}
