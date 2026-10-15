//! The demo house's devices, room by room. Each kind of device is a [`Gadget`] in its own
//! module, and the house is a list of them ([`Gadgets::new`]): what's in the dining room, the
//! garage, the foyer, the approach, the living room, the kitchen, the garden and the basement.
//!
//! The foyer lock and the thermostat also have [`firmware`] to update. The cameras draw what
//! they see ([`picture`]) when a page asks.

mod boiler;
mod camera;
mod climate_sensor;
mod contact;
mod doorbell;
mod firmware;
mod garage_door;
mod leak;
mod light;
mod lock;
mod picture;
mod plug;
mod presence;
mod smoke;
mod soil;
mod switch;
mod thermostat;
pub(super) mod tv;
mod vibration;

use tokio::time::Instant;

use irori_protocol::types::{
    BinarySensorCapabilities, BinarySensorClass, BinarySensorState, Capabilities, EntityCategory,
    EntityDescription, Name, ObjectId, SensorCapabilities, SensorClass, SensorState, SensorValue,
    SensorValueType, Service, State, StateClass,
};
use irori_protocol::{Picture, ProtocolContext, ProtocolError};

use crate::world::World;
use crate::{device, id};

use self::camera::{Camera, Scene};
use self::contact::Contact;
use self::light::{Light, Shade};
use self::plug::{Appliance, Plug};
use self::presence::{MmWave, Movement};
use self::vibration::Vibration;

pub(crate) use self::doorbell::rings;

/// One of the demo's devices: what it is, what it says, what it does when it's told something,
/// and what it does on its own.
trait Gadget: Send {
    /// The device as a real one would describe itself.
    fn device(&self) -> Device;

    /// Its entities, as it describes them.
    fn entities(&self) -> Vec<Entity>;

    /// Every state worth reporting at the start.
    fn states(&self) -> Vec<(&'static str, State)>;

    /// Carries out `service` on its entity `unique_id`: the entity whose state that changed.
    /// `Err` for a service it can't do.
    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        _now: Instant,
    ) -> Result<&'static str, String> {
        Err(cannot(unique_id, service))
    }

    /// The entities that changed on their own by `now`, in `world`.
    fn tick(&mut self, now: Instant, world: &World) -> Vec<&'static str>;

    /// What its camera `unique_id` sees now, for a camera.
    fn picture(&self, _unique_id: &str) -> Option<Picture> {
        None
    }
}

/// A device, as `crate::device` takes it.
#[derive(Debug, Clone, Copy)]
struct Device {
    unique_id: &'static str,
    name: &'static str,
    model: &'static str,
    room: &'static str,
}

/// One of a device's entities.
struct Entity {
    unique_id: &'static str,
    /// `None` when it's the device's main feature and takes the device's name.
    name: Option<&'static str>,
    capabilities: Capabilities,
    category: Option<EntityCategory>,
}

impl Entity {
    /// The device's main feature, named for the device: `light.garage_ceiling_light`.
    fn main(unique_id: &'static str, capabilities: Capabilities) -> Self {
        Self {
            unique_id,
            name: None,
            capabilities,
            category: None,
        }
    }

    fn named(unique_id: &'static str, name: &'static str, capabilities: Capabilities) -> Self {
        Self {
            unique_id,
            name: Some(name),
            capabilities,
            category: None,
        }
    }

    fn config(mut self) -> Self {
        self.category = Some(EntityCategory::Config);
        self
    }

    fn diagnostic(mut self) -> Self {
        self.category = Some(EntityCategory::Diagnostic);
        self
    }

    /// A reading with a number.
    fn measured(
        unique_id: &'static str,
        name: &'static str,
        class: SensorClass,
        unit: &str,
    ) -> Self {
        Self::named(
            unique_id,
            name,
            Capabilities::Sensor(SensorCapabilities {
                value_type: SensorValueType::Number,
                device_class: Some(class),
                unit: Some(unit.into()),
                state_class: Some(StateClass::Measurement),
                options: Vec::new(),
            }),
        )
    }

    /// Something that's on or off.
    fn flag(unique_id: &'static str, name: &'static str, class: BinarySensorClass) -> Self {
        Self::named(
            unique_id,
            name,
            Capabilities::BinarySensor(BinarySensorCapabilities {
                device_class: Some(class),
            }),
        )
    }

    /// A battery's charge, in %.
    fn battery(unique_id: &'static str) -> Self {
        Self::measured(unique_id, "Battery", SensorClass::Battery, "%").diagnostic()
    }
}

/// A reading's state.
fn number(value: f64) -> State {
    State::Sensor(SensorState {
        value: SensorValue::Number(value),
    })
}

/// A binary sensor's state.
fn flag(on: bool) -> State {
    State::BinarySensor(BinarySensorState { on })
}

/// The handles of the devices the demo used to have. They're removed when it starts, so a
/// home that added them doesn't keep them alongside this house. None is used again.
const RETIRED: &[&str] = &[
    "lamp",
    "plug",
    "hallway-sensor",
    "hall-light",
    "movement",
    "luminosity",
    "mmwave",
    "air-monitor",
    "tv-area-mmwave-sensor",
    "tv-area-lights",
    "living-room-light",
    "dining-light",
    "front-door",
    "back-door",
    "window",
    "front-door-lock",
    "house-alarm",
    "robot-vacuum",
    "doorbell",
    "blind",
    "ceiling-fan",
    "tv",
    "thermostat",
    "dehumidifier",
    "hot-water-tank",
    "water-shutoff",
];

/// What the devices are doing.
pub(crate) struct Gadgets {
    all: Vec<Box<dyn Gadget>>,
}

impl Gadgets {
    /// The house, room by room.
    pub(crate) fn new() -> Self {
        Self {
            all: vec![
                // Dining room
                Box::new(Contact::new(
                    Device::new(
                        "dining-room-door-1",
                        "Dining room door 1",
                        "contact",
                        "Dining room",
                    ),
                    "dining-room-door-1-contact",
                    "dining-room-door-1-battery",
                    contact::to_the_patio,
                )),
                Box::new(Contact::new(
                    Device::new(
                        "dining-room-door-2",
                        "Dining room door 2",
                        "contact",
                        "Dining room",
                    ),
                    "dining-room-door-2-contact",
                    "dining-room-door-2-battery",
                    contact::to_the_kitchen,
                )),
                Box::new(MmWave::new(
                    Device::new(
                        "dining-room-presence",
                        "Dining room presence sensor",
                        "mmwave",
                        "Dining room",
                    ),
                    "dining-room-presence-occupancy",
                    "dining-room-presence-distance",
                    presence::at_the_table,
                )),
                Box::new(Vibration::new(
                    Device::new(
                        "dining-room-vibration",
                        "Dining room vibration sensor",
                        "vibration",
                        "Dining room",
                    ),
                    "dining-room-vibration-vibration",
                    "dining-room-vibration-battery",
                    vibration::the_table,
                )),
                // Garage
                Box::new(garage_door::GarageDoor::new()),
                Box::new(Light::new(
                    Device::new(
                        "garage-ceiling-light",
                        "Garage ceiling light",
                        "relay",
                        "Garage",
                    ),
                    "garage-ceiling-light-light",
                    Shade::Relay,
                )),
                Box::new(Camera::new(
                    Device::new("garage-camera", "Garage camera", "camera", "Garage"),
                    "garage-camera-camera",
                    "garage-camera-motion",
                    Scene::Garage,
                    camera::in_the_garage,
                )),
                // Foyer
                Box::new(lock::FoyerLock::new()),
                Box::new(Light::new(
                    Device::new(
                        "foyer-ceiling-light",
                        "Foyer ceiling light",
                        "dimmable",
                        "Foyer",
                    ),
                    "foyer-ceiling-light-light",
                    Shade::Dimmable,
                )),
                Box::new(Movement::new(
                    Device::new("foyer-movement", "Foyer movement sensor", "pir", "Foyer"),
                    "foyer-movement-motion",
                    "foyer-movement-battery",
                    presence::through_the_foyer,
                )),
                // Approach
                Box::new(doorbell::Doorbell::new()),
                Box::new(switch::OutdoorLights::new()),
                Box::new(Vibration::new(
                    Device::new(
                        "mailbox-vibration",
                        "Mailbox sensor",
                        "vibration",
                        "Approach",
                    ),
                    "mailbox-vibration-vibration",
                    "mailbox-vibration-battery",
                    vibration::the_post,
                )),
                // Living room
                Box::new(Contact::new(
                    Device::new(
                        "living-room-door",
                        "Living room door",
                        "contact",
                        "Living room",
                    ),
                    "living-room-door-contact",
                    "living-room-door-battery",
                    contact::to_the_garden,
                )),
                Box::new(Light::new(
                    Device::new(
                        "living-room-wall-light",
                        "Living room wall light",
                        "dimmable",
                        "Living room",
                    ),
                    "living-room-wall-light-light",
                    Shade::Dimmable,
                )),
                Box::new(Light::new(
                    Device::new(
                        "living-room-ceiling-light",
                        "Living room ceiling light",
                        "relay",
                        "Living room",
                    ),
                    "living-room-ceiling-light-light",
                    Shade::Relay,
                )),
                Box::new(Light::new(
                    Device::new(
                        "living-room-lamp",
                        "Living room lamp",
                        "colour",
                        "Living room",
                    ),
                    "living-room-lamp-light",
                    Shade::Colour,
                )),
                Box::new(tv::Tv::new()),
                Box::new(Camera::new(
                    Device::new(
                        "living-room-camera",
                        "Living room camera",
                        "camera",
                        "Living room",
                    ),
                    "living-room-camera-camera",
                    "living-room-camera-motion",
                    Scene::LivingRoom,
                    camera::in_the_living_room,
                )),
                Box::new(climate_sensor::ClimateSensor::new(
                    Device::new(
                        "living-room-climate",
                        "Living room climate sensor",
                        "climate",
                        "Living room",
                    ),
                    [
                        "living-room-climate-temperature",
                        "living-room-climate-humidity",
                        "living-room-climate-battery",
                    ],
                    false,
                )),
                Box::new(MmWave::new(
                    Device::new(
                        "living-room-presence",
                        "Living room presence sensor",
                        "mmwave",
                        "Living room",
                    ),
                    "living-room-presence-occupancy",
                    "living-room-presence-distance",
                    presence::on_the_sofa,
                )),
                // Kitchen
                Box::new(Plug::new(
                    Device::new("fridge-plug", "Fridge plug", "plug", "Kitchen"),
                    [
                        "fridge-plug-switch",
                        "fridge-plug-power",
                        "fridge-plug-energy",
                        "fridge-plug-voltage",
                        "fridge-plug-current",
                    ],
                    Appliance::Fridge,
                )),
                Box::new(Plug::new(
                    Device::new("dishwasher-plug", "Dishwasher plug", "plug", "Kitchen"),
                    [
                        "dishwasher-plug-switch",
                        "dishwasher-plug-power",
                        "dishwasher-plug-energy",
                        "dishwasher-plug-voltage",
                        "dishwasher-plug-current",
                    ],
                    Appliance::Dishwasher,
                )),
                Box::new(Light::new(
                    Device::new(
                        "kitchen-ceiling-light",
                        "Kitchen ceiling light",
                        "dimmable",
                        "Kitchen",
                    ),
                    "kitchen-ceiling-light-light",
                    Shade::Dimmable,
                )),
                Box::new(thermostat::Thermostat::new()),
                Box::new(smoke::SmokeDetector::new()),
                Box::new(leak::LeakSensor::new()),
                // Garden
                Box::new(climate_sensor::ClimateSensor::new(
                    Device::new(
                        "garden-climate",
                        "Garden climate sensor",
                        "climate",
                        "Garden",
                    ),
                    [
                        "garden-climate-temperature",
                        "garden-climate-humidity",
                        "garden-climate-battery",
                    ],
                    true,
                )),
                Box::new(soil::SoilSensor::new()),
                Box::new(soil::LightSensor::new()),
                Box::new(Camera::new(
                    Device::new("garden-camera-1", "Garden camera 1", "camera", "Garden"),
                    "garden-camera-1-camera",
                    "garden-camera-1-motion",
                    Scene::Lawn,
                    camera::on_the_lawn,
                )),
                Box::new(Camera::new(
                    Device::new("garden-camera-2", "Garden camera 2", "camera", "Garden"),
                    "garden-camera-2-camera",
                    "garden-camera-2-motion",
                    Scene::Shed,
                    camera::by_the_shed,
                )),
                // Basement
                Box::new(boiler::WaterBoiler::new()),
            ],
        }
    }

    /// Every state worth reporting at the start.
    pub(crate) fn states(&self) -> Vec<(&'static str, State)> {
        self.all.iter().flat_map(|gadget| gadget.states()).collect()
    }

    fn owner(&self, unique_id: &str) -> Option<&dyn Gadget> {
        self.all
            .iter()
            .find(|gadget| {
                gadget
                    .entities()
                    .iter()
                    .any(|entity| entity.unique_id == unique_id)
            })
            .map(AsRef::as_ref)
    }

    /// Carries out a call: the entity whose state it changed, and that state.
    pub(crate) fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        now: Instant,
    ) -> Result<(&'static str, State), String> {
        let gadget = self
            .all
            .iter_mut()
            .find(|gadget| {
                gadget
                    .entities()
                    .iter()
                    .any(|entity| entity.unique_id == unique_id)
            })
            .ok_or_else(|| format!("the demo has no `{unique_id}`"))?;
        gadget
            .call(unique_id, service, now)
            .map(|changed| state_of(gadget.as_ref(), changed))
    }

    /// What changed on its own by `now`, in `world`.
    pub(crate) fn tick(&mut self, now: Instant, world: &World) -> Vec<(&'static str, State)> {
        let mut changed = Vec::new();
        for gadget in &mut self.all {
            for entity in gadget.tick(now, world) {
                changed.push(state_of(gadget.as_ref(), entity));
            }
        }
        changed
    }

    /// What the camera `unique_id` sees now.
    pub(crate) fn picture(&self, unique_id: &str) -> Result<Picture, String> {
        self.owner(unique_id)
            .and_then(|gadget| gadget.picture(unique_id))
            .ok_or_else(|| format!("the demo has no camera `{unique_id}`"))
    }
}

impl Device {
    /// A device of the demo house. `model` says what it is, after "Virtual".
    const fn new(
        unique_id: &'static str,
        name: &'static str,
        model: &'static str,
        room: &'static str,
    ) -> Self {
        Self {
            unique_id,
            name,
            model,
            room,
        }
    }

    /// What a person reads as its model.
    fn model_words(&self) -> String {
        let what = match self.model {
            "contact" => "contact sensor",
            "mmwave" => "mmWave presence sensor",
            "pir" => "PIR movement sensor",
            "vibration" => "vibration sensor",
            "relay" => "light on a relay",
            "dimmable" => "dimmable light",
            "colour" => "colour lamp",
            "camera" => "security camera",
            "climate" => "temperature and humidity sensor",
            "plug" => "smart plug with power meter",
            other => other,
        };
        format!("Virtual {what}")
    }
}

/// `entity`'s state, as `gadget` reports it.
fn state_of(gadget: &dyn Gadget, entity: &'static str) -> (&'static str, State) {
    gadget
        .states()
        .into_iter()
        .find(|(unique_id, _)| *unique_id == entity)
        .expect("every entity that takes a call or changes has a state")
}

/// The error for a service one of these entities doesn't take.
fn cannot(unique_id: &str, service: &Service) -> String {
    format!("the demo's `{unique_id}` can't {}", service.name())
}

/// The id an entity asks for, from its device's name and its own: `light.living_room_lamp`,
/// `sensor.garden_climate_sensor_temperature`. Without it the device's id would lead, and that
/// starts with the protocol: `light.demo_living_room_lamp`.
fn object_id(device: &str, entity: Option<&str>) -> String {
    let words = match entity {
        Some(entity) => format!("{device} {entity}"),
        None => device.to_owned(),
    };
    words
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

/// Takes away the devices the demo used to have, then describes the house.
pub(crate) async fn describe(ctx: &ProtocolContext) -> Result<(), ProtocolError> {
    for handle in RETIRED {
        // Missing on a home that never had it, or once it's gone: either is fine.
        let _ = ctx.remove_device(id(handle)?).await;
    }
    for gadget in Gadgets::new().all {
        let described = gadget.device();
        ctx.describe_device(device(
            described.unique_id,
            described.name,
            &described.model_words(),
            described.room,
        )?)
        .await?;
        for entity in gadget.entities() {
            ctx.describe_entity(EntityDescription {
                unique_id: id(entity.unique_id)?,
                name: entity.name.map(Name::try_from).transpose()?,
                device_unique_id: Some(id(described.unique_id)?),
                suggested_object_id: Some(ObjectId::try_from(object_id(
                    described.name,
                    entity.name,
                ))?),
                capabilities: entity.capabilities,
                entity_category: entity.category,
            })
            .await?;
        }
    }
    Ok(())
}

/// Irori refuses a report that doesn't fit what the entity said it is; here that would only be a
/// line in the log, so every state is checked against its entity.
#[cfg(test)]
fn assert_fits(gadget: &dyn Gadget, states: &[(&'static str, State)]) {
    let entities = gadget.entities();
    for (unique_id, state) in states {
        let entity = entities
            .iter()
            .find(|entity| entity.unique_id == *unique_id)
            .expect("described");
        assert_eq!(entity.capabilities.fits(state), Ok(()), "{unique_id}");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tokio::time::Duration;

    use super::*;

    #[test]
    fn every_state_fits_its_entity_through_a_few_days() {
        let now = Instant::now();
        let mut gadgets = Gadgets::new();
        let fits = |gadgets: &Gadgets, states: &[(&'static str, State)]| {
            for gadget in &gadgets.all {
                let ids: BTreeSet<_> = gadget.entities().iter().map(|e| e.unique_id).collect();
                let own: Vec<_> = states
                    .iter()
                    .filter(|(id, _)| ids.contains(id))
                    .cloned()
                    .collect();
                assert_fits(gadget.as_ref(), &own);
            }
        };
        fits(&gadgets, &gadgets.states());
        for second in (0..400).step_by(2) {
            let ticked = gadgets.tick(now + Duration::from_secs(second), &World::at(second));
            fits(&gadgets, &ticked);
        }
        fits(&gadgets, &rings(0, 1000));
    }

    #[test]
    fn every_entity_belongs_to_one_device_and_no_retired_handle_comes_back() {
        let gadgets = Gadgets::new();
        let mut seen = BTreeSet::new();
        for gadget in &gadgets.all {
            let device = gadget.device();
            assert!(seen.insert(device.unique_id), "{}", device.unique_id);
            assert!(!RETIRED.contains(&device.unique_id), "{}", device.unique_id);
            assert!(!device.name.contains("Demo"), "{}", device.name);
            for entity in gadget.entities() {
                assert!(seen.insert(entity.unique_id), "{}", entity.unique_id);
                assert!(
                    entity.unique_id.starts_with(device.unique_id),
                    "{} isn't named for its device",
                    entity.unique_id
                );
            }
        }
    }

    #[test]
    fn entities_are_named_for_their_device_without_the_protocol() {
        assert_eq!(object_id("Living room lamp", None), "living_room_lamp");
        assert_eq!(
            object_id("Garden climate sensor", Some("Temperature")),
            "garden_climate_sensor_temperature"
        );
        assert_eq!(
            object_id("Dining room door 1", Some("Contact")),
            "dining_room_door_1_contact"
        );
        let gadgets = Gadgets::new();
        let mut ids = BTreeSet::new();
        for gadget in &gadgets.all {
            for entity in gadget.entities() {
                let id = object_id(gadget.device().name, entity.name);
                assert!(ObjectId::try_from(id.as_str()).is_ok(), "{id}");
                assert!(ids.insert(id.clone()), "{id} twice");
            }
        }
    }

    #[test]
    fn every_room_the_house_asks_for_has_devices() {
        let gadgets = Gadgets::new();
        let rooms: BTreeSet<&str> = gadgets.all.iter().map(|g| g.device().room).collect();
        assert_eq!(
            rooms,
            BTreeSet::from([
                "Approach",
                "Basement",
                "Dining room",
                "Foyer",
                "Garage",
                "Garden",
                "Kitchen",
                "Living room",
            ])
        );
    }

    #[test]
    fn a_call_reaches_its_device_and_a_picture_only_a_camera() {
        let mut gadgets = Gadgets::new();
        assert!(
            gadgets
                .call(
                    "garage-ceiling-light-light",
                    &Service::LightTurnOff,
                    Instant::now()
                )
                .is_ok()
        );
        assert!(
            gadgets
                .call("nothing", &Service::LightTurnOff, Instant::now())
                .is_err()
        );
        let seen = gadgets
            .picture("garden-camera-1-camera")
            .expect("a picture");
        assert_eq!(seen.content_type, "image/svg+xml");
        assert_eq!(seen.check(), Ok(()));
        assert!(gadgets.picture("garage-ceiling-light-light").is_err());
    }
}
