//! A protocol whose devices take a while to answer (a car that has to wake first) says so in its
//! manifest, and the core waits that long for its calls instead of the usual 10 seconds. Time is
//! paused, so the waits run instantly.

use std::sync::Arc;
use std::time::Duration;

use irori_core::{CallError, Command, Core, ExtensionHost, SystemClock, Timing};
use irori_protocol::types::{
    Capabilities, Context, ContextId, DeviceDescription, EntityDescription, EntityId, Name, Origin,
    State, StateReport, SwitchCapabilities, SwitchState, UniqueId, UserId,
};
use irori_protocol::{NoSettings, Protocol, ProtocolContext, ProtocolError, builtin};

/// How long either protocol takes to answer a call.
const WAKING: Duration = Duration::from_secs(30);

fn uid(s: &str) -> UniqueId {
    UniqueId::try_from(s).expect("valid")
}

/// One switch that answers every call after [`WAKING`].
async fn sleepy(mut ctx: ProtocolContext, device: &str) -> Result<(), ProtocolError> {
    ctx.describe_device(DeviceDescription {
        unique_id: uid(device),
        name: Name::try_from(device)?,
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
        unique_id: uid(device),
        name: None,
        device_unique_id: Some(uid(device)),
        suggested_object_id: None,
        capabilities: Capabilities::Switch(SwitchCapabilities { device_class: None }),
        entity_category: None,
    })
    .await?;
    ctx.report_state(StateReport {
        unique_id: uid(device),
        state: Some(State::Switch(SwitchState { on: false })),
        attributes: Default::default(),
        caused_by: None,
        replayed: false,
    });
    while let Some(incoming) = ctx.next_call().await {
        tokio::time::sleep(WAKING).await;
        incoming.reply(Ok(()));
    }
    Ok(())
}

/// Says its calls may take a minute.
struct Car;
impl Protocol for Car {
    type Config = NoSettings;
    const MANIFEST: &'static str = r#"
        [extension]
        id = "car"
        name = "Car"
        version = "0.1.0"
        irori = ">=0.0.0"

        [[contributes.protocol]]
        iot_class = "cloud_polling"
        entity_kinds = ["switch"]
        answer_within_seconds = 60
    "#;
    async fn run(_: NoSettings, ctx: ProtocolContext) -> Result<(), ProtocolError> {
        sleepy(ctx, "climate").await
    }
}

/// Says nothing, so it gets the usual wait.
struct Plug;
impl Protocol for Plug {
    type Config = NoSettings;
    const MANIFEST: &'static str = r#"
        [extension]
        id = "plug"
        name = "Plug"
        version = "0.1.0"
        irori = ">=0.0.0"

        [[contributes.protocol]]
        iot_class = "local_push"
        entity_kinds = ["switch"]
    "#;
    async fn run(_: NoSettings, ctx: ProtocolContext) -> Result<(), ProtocolError> {
        sleepy(ctx, "kettle").await
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

async fn entity(core: &Core, device: &str) -> EntityId {
    for _ in 0..10_000 {
        if let Some(entity) = core
            .entities()
            .into_iter()
            .find(|entity| entity.unique_id == uid(device))
        {
            return entity.id;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{device} was never described");
}

#[tokio::test(start_paused = true)]
async fn a_protocol_that_says_its_calls_are_slow_is_waited_for() {
    let core = Core::new(Arc::new(SystemClock));
    let host = ExtensionHost::start(
        &core,
        vec![
            builtin::<Car>().expect("valid"),
            builtin::<Plug>().expect("valid"),
        ],
        Timing::default(),
    )
    .expect("unique ids");
    let climate = entity(&core, "climate").await;
    let kettle = entity(&core, "kettle").await;

    core.call_service(&climate, Command::new("turn_on"), user_context())
        .await
        .expect("the car's answer is waited for");
    assert!(
        matches!(
            core.call_service(&kettle, Command::new("turn_on"), user_context())
                .await,
            Err(CallError::Timeout)
        ),
        "a protocol that said nothing still gets 10 seconds"
    );
    host.shutdown().await;
}
