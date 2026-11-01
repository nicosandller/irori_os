//! A tracker that says where it is (a car's GPS) is home by where the home is: the core works it
//! out from the home's location, so every protocol with a position gets it right without being
//! told where the home is. With no location set, the protocol's own answer stands.

use std::sync::Arc;
use std::time::Duration;

use irori_core::{Core, ExtensionHost, SystemClock, Timing};
use irori_protocol::types::{
    Capabilities, DeviceDescription, EntityDescription, EntityId, HomeSettings, Location, Name,
    State, StateReport, TrackerCapabilities, TrackerSource, TrackerState, UniqueId,
};
use irori_protocol::{NoSettings, Protocol, ProtocolContext, ProtocolError, builtin};

fn uid(s: &str) -> UniqueId {
    UniqueId::try_from(s).expect("valid")
}

/// Where the car is: about 55 m from the home below, so home.
const PARKED: (f64, f64) = (51.0005, 4.0);
/// Across town.
const AWAY: (f64, f64) = (51.05, 4.0);

fn position((latitude, longitude): (f64, f64)) -> StateReport {
    StateReport {
        unique_id: uid("car-location"),
        state: Some(State::DeviceTracker(TrackerState {
            // The protocol doesn't know where the home is.
            home: false,
            latitude: Some(latitude),
            longitude: Some(longitude),
            accuracy: Some(10.0),
        })),
        attributes: Default::default(),
        caused_by: None,
        replayed: false,
    }
}

/// A car that reports where it's parked, then drives off when asked anything.
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
        entity_kinds = ["device_tracker"]
    "#;
    async fn run(_: NoSettings, mut ctx: ProtocolContext) -> Result<(), ProtocolError> {
        ctx.describe_device(DeviceDescription {
            unique_id: uid("car"),
            name: Name::try_from("Car")?,
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
            unique_id: uid("car-location"),
            name: None,
            device_unique_id: Some(uid("car")),
            suggested_object_id: None,
            capabilities: Capabilities::DeviceTracker(TrackerCapabilities {
                source_type: Some(TrackerSource::Gps),
            }),
            entity_category: None,
        })
        .await?;
        ctx.report_state(position(PARKED));
        ctx.stopped().await;
        Ok(())
    }
}

async fn tracker(core: &Core) -> (EntityId, TrackerState) {
    for _ in 0..500 {
        if let Some(entity) = core
            .entities()
            .into_iter()
            .find(|entity| entity.unique_id == uid("car-location"))
            && let Some(State::DeviceTracker(state)) =
                core.state(&entity.id).and_then(|state| state.state)
        {
            return (entity.id, state);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the car never reported");
}

#[tokio::test]
async fn a_car_parked_by_the_home_is_home() {
    let core = Core::new(Arc::new(SystemClock));
    core.apply_home(HomeSettings {
        location: Some(
            serde_json::from_value::<Location>(
                serde_json::json!({ "latitude": 51.0, "longitude": 4.0 }),
            )
            .expect("valid"),
        ),
        ..HomeSettings::default()
    });
    let host = ExtensionHost::start(
        &core,
        vec![builtin::<Car>().expect("valid")],
        Timing::default(),
    )
    .expect("unique ids");
    let (_, parked) = tracker(&core).await;
    assert!(parked.home, "{parked:?}");
    assert_eq!(parked.latitude, Some(PARKED.0), "the position is kept");
    host.shutdown().await;
}

#[tokio::test]
async fn with_no_home_location_the_protocols_answer_stands() {
    let core = Core::new(Arc::new(SystemClock));
    let host = ExtensionHost::start(
        &core,
        vec![builtin::<Car>().expect("valid")],
        Timing::default(),
    )
    .expect("unique ids");
    let (_, parked) = tracker(&core).await;
    assert!(!parked.home, "nothing to say otherwise: {parked:?}");
    host.shutdown().await;
}

#[test]
fn across_town_is_not_home() {
    let State::DeviceTracker(away) = position(AWAY).state.expect("a state") else {
        unreachable!()
    };
    assert_eq!(away.is_at((51.0, 4.0)), Some(false));
}
