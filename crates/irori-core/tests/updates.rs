//! Updates end to end: what the core answers itself (skipping), and installs taking turns on a
//! protocol. Time is paused, so an install that takes minutes runs instantly.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use irori_core::{CallError, Command, Core, Event, ExtensionHost, SystemClock, Timing};
use irori_protocol::types::{
    Capabilities, Context, ContextId, DeviceDescription, EntityDescription, EntityId, Name, Origin,
    Service, State, StateReport, UniqueId, UpdateCapabilities, UpdateClass, UpdateState, UserId,
};
use irori_protocol::{
    MemoryStorage, NoSettings, Protocol, ProtocolContext, ProtocolError, Storage, builtin,
};

/// How long a plug takes to flash.
const FLASHING: Duration = Duration::from_secs(300);

/// The most plugs that were ever flashing at once.
static AT_ONCE: AtomicUsize = AtomicUsize::new(0);

fn uid(s: &str) -> UniqueId {
    UniqueId::try_from(s).expect("valid")
}

fn update(installed: &str, flashing: bool) -> StateReport {
    StateReport {
        unique_id: uid("unset"),
        state: Some(State::Update(UpdateState {
            available: installed != "2",
            installed_version: Some(installed.into()),
            latest_version: Some("2".into()),
            in_progress: flashing,
            progress: flashing.then_some(50.0),
            ..UpdateState::default()
        })),
        attributes: BTreeMap::new(),
        caused_by: None,
        replayed: false,
    }
}

fn report(ctx: &ProtocolContext, plug: &UniqueId, installed: &str, flashing: bool) {
    ctx.report_state(StateReport {
        unique_id: plug.clone(),
        ..update(installed, flashing)
    });
}

/// Three plugs on version 1 with version 2 on offer. `a` and `b` take five minutes to flash;
/// `deaf` says yes to an install and then does nothing.
struct Plugs;
impl Protocol for Plugs {
    type Config = NoSettings;
    const MANIFEST: &'static str = r#"
        [extension]
        id = "plugs"
        name = "Plugs"
        version = "0.1.0"
        irori = ">=0.0.0"

        [[contributes.protocol]]
        iot_class = "local_push"
        entity_kinds = ["update"]
    "#;

    async fn run(_: NoSettings, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
        for plug in ["a", "b", "deaf"] {
            ctx.describe_device(DeviceDescription {
                unique_id: uid(plug),
                name: Name::try_from(plug)?,
                manufacturer: None,
                model: None,
                sw_version: None,
                hw_version: None,
                suggested_area: None,
                via_device_unique_id: None,
            })
            .await?;
            ctx.describe_entity(EntityDescription {
                unique_id: uid(plug),
                name: None,
                device_unique_id: Some(uid(plug)),
                suggested_object_id: None,
                capabilities: Capabilities::Update(UpdateCapabilities {
                    device_class: Some(UpdateClass::Firmware),
                    install: true,
                    check: false,
                }),
                entity_category: None,
            })
            .await?;
            report(&ctx, &uid(plug), "1", false);
        }
        let mut flashing: Vec<(UniqueId, tokio::time::Instant)> = Vec::new();
        loop {
            let done_at = flashing.iter().map(|(_, at)| *at).min();
            tokio::select! {
                incoming = ctx.next_call() => {
                    let Some(incoming) = incoming else { break };
                    let plug = incoming.call.unique_id.clone();
                    let install = matches!(incoming.call.service, Service::UpdateInstall);
                    incoming.reply(Ok(()));
                    if install && plug != uid("deaf") {
                        flashing.push((plug.clone(), tokio::time::Instant::now() + FLASHING));
                        AT_ONCE.fetch_max(flashing.len(), Ordering::SeqCst);
                        report(&ctx, &plug, "1", true);
                    }
                }
                () = async { tokio::time::sleep_until(done_at.expect("one is flashing")).await },
                    if done_at.is_some() =>
                {
                    let now = tokio::time::Instant::now();
                    for (plug, _) in flashing.iter().filter(|(_, at)| *at <= now) {
                        report(&ctx, plug, "2", false);
                    }
                    flashing.retain(|(_, at)| *at > now);
                }
            }
        }
        Ok(())
    }
}

fn user_context() -> Context {
    Context {
        id: ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9").expect("valid"),
        parent_id: None,
        origin: Origin::User {
            user_id: UserId::try_from("nico").expect("valid"),
        },
    }
}

async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..100_000 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for: {what}");
}

fn start(core: &Core) -> ExtensionHost {
    ExtensionHost::start(
        core,
        vec![builtin::<Plugs>().expect("valid")],
        Timing::default(),
    )
    .expect("unique ids")
}

/// A plug's update entity, once its protocol has described it.
fn entity(core: &Core, plug: &str) -> Option<EntityId> {
    core.entities()
        .into_iter()
        .find(|entity| entity.unique_id == uid(plug))
        .map(|entity| entity.id)
}

fn state(core: &Core, id: &EntityId) -> Option<UpdateState> {
    match core.state(id)?.state? {
        State::Update(update) => Some(update),
        _ => None,
    }
}

async fn plugs(core: &Core) -> (EntityId, EntityId, EntityId) {
    eventually("the plugs report", || {
        ["a", "b", "deaf"]
            .iter()
            .all(|plug| entity(core, plug).is_some_and(|id| state(core, &id).is_some()))
    })
    .await;
    let id = |plug| entity(core, plug).expect("described");
    (id("a"), id("b"), id("deaf"))
}

async fn install(core: &Core, id: &EntityId) -> Result<(), CallError> {
    core.call_service(id, Command::new("install"), user_context())
        .await
}

#[tokio::test(start_paused = true)]
async fn installs_on_one_protocol_take_turns() {
    let core = Core::new(Arc::new(SystemClock));
    let host = start(&core);
    let (a, b, deaf) = plugs(&core).await;
    let mut events = core.subscribe();

    install(&core, &a).await.expect("sent");
    install(&core, &b).await.expect("waits, which is fine");
    install(&core, &deaf).await.expect("waits too");
    let waiting: Vec<(EntityId, usize)> = core
        .queued_installs()
        .into_iter()
        .map(|queued| (queued.entity_id, queued.ahead))
        .collect();
    assert!(waiting.contains(&(b.clone(), 1)), "{waiting:?}");
    assert!(waiting.contains(&(deaf.clone(), 2)), "{waiting:?}");
    eventually("the first plug says it's installing", || {
        state(&core, &a).is_some_and(|plug| plug.in_progress)
    })
    .await;
    assert!(
        matches!(install(&core, &a).await, Err(CallError::NotSupported(why)) if why.contains("already")),
        "an install that's running isn't started again"
    );

    eventually("both plugs are on version 2", || {
        [&a, &b].iter().all(|id| {
            state(&core, id).is_some_and(|plug| plug.installed_version.as_deref() == Some("2"))
        })
    })
    .await;
    assert_eq!(AT_ONCE.load(Ordering::SeqCst), 1, "never two at once");

    // The deaf plug took its turn last, said nothing, and was given up on.
    eventually("the deaf plug is given up on", || {
        state(&core, &deaf).is_some_and(|plug| plug.last_error.is_some())
    })
    .await;
    let deaf_state = state(&core, &deaf).expect("it reported");
    assert!(
        deaf_state.available,
        "the update is still there to try again"
    );
    assert!(core.queued_installs().is_empty());

    let mut said_the_line_changed = false;
    while let Ok(event) = events.try_recv() {
        said_the_line_changed |= event == Event::InstallsChanged;
    }
    assert!(said_the_line_changed);
    host.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_skipped_version_stays_skipped_across_a_restart() {
    let storage: Arc<dyn Storage> = Arc::new(MemoryStorage::default());
    let core = Core::new(Arc::new(SystemClock));
    core.use_storage(Arc::clone(&storage));
    let host = start(&core);
    let (a, _, _) = plugs(&core).await;

    core.call_service(&a, Command::new("skip"), user_context())
        .await
        .expect("skipped");
    let skipped = state(&core, &a).expect("it reported");
    assert!(!skipped.available);
    assert_eq!(skipped.skipped_version.as_deref(), Some("2"));
    let who = core.state(&a).expect("there").context.origin;
    assert!(matches!(who, Origin::User { .. }), "{who:?}");
    assert!(
        matches!(
            core.call_service(&a, Command::new("skip"), user_context())
                .await,
            Err(CallError::NotSupported(_))
        ),
        "nothing is left to skip"
    );
    host.shutdown().await;

    let again = Core::new(Arc::new(SystemClock));
    again.use_storage(storage);
    let host = start(&again);
    let (a, b, _) = plugs(&again).await;
    assert!(!state(&again, &a).expect("it reported").available);
    assert!(state(&again, &b).expect("it reported").available);

    again
        .call_service(&a, Command::new("clear_skipped"), user_context())
        .await
        .expect("taken back");
    let back = state(&again, &a).expect("it reported");
    assert!(back.available);
    assert_eq!(back.skipped_version, None);
    host.shutdown().await;
}
