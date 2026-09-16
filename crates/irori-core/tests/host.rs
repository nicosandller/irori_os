//! The extension host end to end: integrations started, fed into the core, called, supervised.
//! Time is paused, so waits of seconds or minutes run instantly.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use irori_core::{
    CallError, Command, Core, Event, ExtensionHost, ExtensionStatus, SystemClock, Timing,
};
use irori_integration::types::{
    Availability, Capabilities, Context, ContextId, DeviceDescription, EntityDescription, EntityId,
    ExtensionId, LightCapabilities, LightState, LightTurnOn, Name, Origin, Service, State,
    StateReport, UniqueId, UserId, Version,
};
use irori_integration::{
    Health, Integration, IntegrationContext, IntegrationError, NoSettings, builtin,
};

fn uid(s: &str) -> UniqueId {
    UniqueId::try_from(s).expect("valid")
}

fn lamp_id() -> EntityId {
    EntityId::try_from("light.lamp").expect("valid")
}

async fn describe_lamp(ctx: &IntegrationContext) -> Result<(), IntegrationError> {
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

fn start(core: &Core, builtin: irori_integration::Builtin) -> ExtensionHost {
    ExtensionHost::start(core, vec![builtin], Timing::default()).expect("unique ids")
}

// --- A well-behaved lamp ------------------------------------------------------------------

struct Lamp;
impl Integration for Lamp {
    type Config = NoSettings;
    const MANIFEST: &'static str = LAMP_MANIFEST;
    async fn run(_: NoSettings, mut ctx: IntegrationContext) -> Result<(), IntegrationError> {
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
    irori = ">=0.0.0, <0.1.0"

    [[contributes.integration]]
    iot_class = "local_push"
    entity_kinds = ["light"]
"#;

#[tokio::test(start_paused = true)]
async fn devices_appear_and_commands_round_trip_with_their_context() {
    let core = Core::new(Arc::new(SystemClock));
    let mut events = core.subscribe();
    let host = start(&core, builtin::<Lamp>().expect("valid"));

    eventually("the lamp reports it's on", || {
        core.state(&lamp_id())
            .and_then(|s| s.state)
            .is_some_and(|s| matches!(s, State::Light(LightState { on: true, .. })))
    })
    .await;
    assert_eq!(status(&core, "lamp"), Some(ExtensionStatus::Running));
    assert_eq!(core.devices().len(), 1);

    let context = user_context();
    core.call_service(&lamp_id(), Command::Toggle, context.clone())
        .await
        .expect("toggle works");
    eventually("the lamp reports it's off", || {
        core.state(&lamp_id())
            .and_then(|s| s.state)
            .is_some_and(|s| matches!(s, State::Light(LightState { on: false, .. })))
    })
    .await;
    // The change points back to the person who asked.
    let state = core.state(&lamp_id()).expect("state");
    assert_eq!(state.context.parent_id, Some(context.id));

    let mut saw_call = false;
    while let Ok(event) = events.try_recv() {
        saw_call |= matches!(event, Event::ServiceCalled { .. });
    }
    assert!(saw_call, "a ServiceCalled event was published");

    let err = core
        .call_service(
            &lamp_id(),
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
    let state = core.state(&lamp_id()).expect("kept");
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
impl Integration for SlowLamp {
    type Config = NoSettings;
    const MANIFEST: &'static str = r#"
        [extension]
        id = "slow_lamp"
        name = "Slow lamp"
        version = "0.1.0"
        irori = ">=0.0.0"

        [[contributes.integration]]
        iot_class = "local_push"
        entity_kinds = ["light"]
    "#;
    async fn run(_: NoSettings, mut ctx: IntegrationContext) -> Result<(), IntegrationError> {
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
        core.state(&lamp_id())
            .and_then(|s| s.state)
            .is_some_and(|s| matches!(s, State::Light(LightState { on: true, .. })))
    };
    eventually("the lamp is on", on).await;

    // Two people press toggle at the same moment, before the lamp has confirmed the first:
    // off, then on again.
    let lamp = lamp_id();
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
impl Integration for BrokenLamp {
    type Config = NoSettings;
    const MANIFEST: &'static str = r#"
        [extension]
        id = "broken_lamp"
        name = "Broken lamp"
        version = "0.1.0"
        irori = ">=0.0.0"

        [[contributes.integration]]
        iot_class = "local_push"
        entity_kinds = ["light"]
    "#;
    async fn run(_: NoSettings, mut ctx: IntegrationContext) -> Result<(), IntegrationError> {
        describe_lamp(&ctx).await?;
        ctx.report_state(light(true, None, None));
        while let Some(incoming) = ctx.next_call().await {
            if matches!(incoming.call.service, Service::LightTurnOff) {
                TURN_OFFS.fetch_add(1, Ordering::SeqCst);
            }
            incoming.reply(Err(irori_integration::ServiceError::unavailable(
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
        core.state(&lamp_id())
            .and_then(|s| s.state)
            .is_some_and(|s| matches!(s, State::Light(LightState { on: true, .. })))
    })
    .await;

    // Both toggles see a lamp that's still on, so both try to turn it off.
    for _ in 0..2 {
        let err = core
            .call_service(&lamp_id(), Command::Toggle, user_context())
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
impl Integration for Crashy {
    type Config = NoSettings;
    const MANIFEST: &'static str = CRASHY_MANIFEST;
    async fn run(_: NoSettings, ctx: IntegrationContext) -> Result<(), IntegrationError> {
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

    [[contributes.integration]]
    iot_class = "local_push"
    entity_kinds = ["light"]
"#;

#[tokio::test(start_paused = true)]
async fn a_crashing_integration_is_restarted_with_growing_delays() {
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
        core.state(&lamp_id()).expect("kept").availability,
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
        core.state(&lamp_id())
            .is_some_and(|s| s.availability == Availability::Available)
    })
    .await;

    host.shutdown().await;
}

static EAGER_STARTS: AtomicUsize = AtomicUsize::new(0);

/// Panics before it even returns its future.
struct PanicsOnStart;
impl Integration for PanicsOnStart {
    type Config = NoSettings;
    const MANIFEST: &'static str = r#"
        [extension]
        id = "eager"
        name = "Eager"
        version = "0.1.0"
        irori = ">=0.0.0"

        [[contributes.integration]]
        iot_class = "local_push"
        entity_kinds = ["light"]
    "#;
    #[allow(clippy::manual_async_fn)]
    fn run(
        _: NoSettings,
        _: IntegrationContext,
    ) -> impl std::future::Future<Output = Result<(), IntegrationError>> + Send {
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
impl Integration for Stubborn {
    type Config = NoSettings;
    const MANIFEST: &'static str = STUBBORN_MANIFEST;
    async fn run(_: NoSettings, ctx: IntegrationContext) -> Result<(), IntegrationError> {
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

    [[contributes.integration]]
    iot_class = "local_push"
    entity_kinds = ["light"]
"#;

#[tokio::test(start_paused = true)]
async fn an_integration_that_ignores_stop_is_cancelled_after_the_grace_period() {
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
impl Integration for Silent {
    type Config = NoSettings;
    const MANIFEST: &'static str = SILENT_MANIFEST;
    async fn run(_: NoSettings, mut ctx: IntegrationContext) -> Result<(), IntegrationError> {
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

    [[contributes.integration]]
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
    let lamp = lamp_id();
    let (first, second) = tokio::join!(
        core.call_service(&lamp, Command::TurnOff, user_context()),
        core.call_service(&lamp, Command::TurnOff, user_context()),
    );
    assert_eq!(first.expect_err("no answer"), CallError::Timeout);
    assert_eq!(second.expect_err("no answer"), CallError::Timeout);
    assert_eq!(started.elapsed(), Duration::from_secs(10));
    host.shutdown().await;

    let err = core
        .call_service(&lamp_id(), Command::TurnOff, user_context())
        .await
        .expect_err("stopped");
    assert!(matches!(err, CallError::NotRunning(_)), "{err}");
}

struct FromTheFuture;
impl Integration for FromTheFuture {
    type Config = NoSettings;
    const MANIFEST: &'static str = r#"
        [extension]
        id = "future"
        name = "Future"
        version = "0.1.0"
        irori = ">=9.0.0"

        [[contributes.integration]]
        iot_class = "local_push"
        entity_kinds = ["light"]
    "#;
    async fn run(_: NoSettings, _: IntegrationContext) -> Result<(), IntegrationError> {
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
impl Integration for Confused {
    type Config = NoSettings;
    const MANIFEST: &'static str = r#"
        [extension]
        id = "confused"
        name = "Confused"
        version = "0.1.0"
        irori = ">=0.0.0"

        [[contributes.integration]]
        iot_class = "local_push"
        entity_kinds = ["light"]
    "#;
    async fn run(_: NoSettings, mut ctx: IntegrationContext) -> Result<(), IntegrationError> {
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

impl Integration for Keyed {
    type Config = KeyedSettings;
    const MANIFEST: &'static str = KEYED_MANIFEST;
    async fn run(
        settings: KeyedSettings,
        mut ctx: IntegrationContext,
    ) -> Result<(), IntegrationError> {
        STARTED_WITH
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(settings.key.clone());
        if settings.key.is_none() {
            ctx.set_waiting(vec![irori_integration::types::Waiting {
                unique_id: uid("locked"),
                name: Name::try_from("Locked box")?,
                reason: "it wants a key".into(),
                secret: Some(irori_integration::types::SecretRequest {
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
    irori = ">=0.0.0, <0.1.0"

    [[contributes.integration]]
    iot_class = "local_push"
    entity_kinds = ["light"]
"#;

fn keyed_settings(
    extension: &str,
    table: serde_json::Value,
) -> irori_integration::types::ExtensionSettings {
    let serde_json::Value::Object(table) = table else {
        panic!("a table");
    };
    irori_integration::types::ExtensionSettings::new(
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
