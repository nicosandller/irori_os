//! A temperature and humidity sensor on a battery: one in the living room, one out in the
//! garden.

use tokio::time::Instant;

use irori_protocol::types::{SensorClass, State};

use super::{Device, Entity, Gadget, number};
use crate::world::World;

pub(super) struct ClimateSensor {
    device: Device,
    /// Its temperature, humidity and battery.
    ids: [&'static str; 3],
    outdoors: bool,
    world: World,
}

impl ClimateSensor {
    pub(super) fn new(device: Device, ids: [&'static str; 3], outdoors: bool) -> Self {
        Self {
            device,
            ids,
            outdoors,
            world: World::at(0),
        }
    }
}

impl Gadget for ClimateSensor {
    fn device(&self) -> Device {
        self.device
    }

    fn entities(&self) -> Vec<Entity> {
        let [temperature, humidity, battery] = self.ids;
        vec![
            Entity::measured(temperature, "Temperature", SensorClass::Temperature, "°C"),
            Entity::measured(humidity, "Humidity", SensorClass::Humidity, "%"),
            Entity::battery(battery),
        ]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        let [temperature, humidity, battery] = self.ids;
        let (degrees, damp) = if self.outdoors {
            (self.world.outdoors, self.world.outdoor_humidity)
        } else {
            (self.world.indoors, self.world.indoor_humidity)
        };
        vec![
            (temperature, number(degrees)),
            (humidity, number(damp)),
            (battery, number(self.world.battery)),
        ]
    }

    fn tick(&mut self, _: Instant, world: &World) -> Vec<&'static str> {
        self.world = *world;
        self.ids.to_vec()
    }
}
