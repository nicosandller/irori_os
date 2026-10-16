//! Out in the garden: a plant sensor pushed into a bed, which reads the soil's moisture and
//! temperature and the air's humidity, and a light sensor on the fence.

use tokio::time::Instant;

use irori_protocol::types::{SensorClass, State};

use super::{Device, Entity, Gadget, number};
use crate::world::{World, daily, round1};

const SOIL: &str = "garden-soil";
const SOIL_MOISTURE: &str = "garden-soil-moisture";
const SOIL_TEMPERATURE: &str = "garden-soil-temperature";
const SOIL_HUMIDITY: &str = "garden-soil-humidity";
const SOIL_BATTERY: &str = "garden-soil-battery";

const LUX: &str = "garden-light";
const LUX_ILLUMINANCE: &str = "garden-light-illuminance";
const LUX_BATTERY: &str = "garden-light-battery";

pub(super) struct SoilSensor {
    world: World,
}

impl SoilSensor {
    pub(super) fn new() -> Self {
        Self {
            world: World::at(0),
        }
    }
}

/// The bed's moisture in %: watered at six in the morning, drying out through the day.
fn moisture(hour: f64) -> f64 {
    let watered = 46.0;
    let dried = |since: f64| (watered - 0.65 * since).max(20.0);
    round1(match hour {
        h if h < 6.0 => dried(h + 24.0 - 6.3),
        h if h < 6.3 => dried(23.7) + (watered - dried(23.7)) * (h - 6.0) / 0.3,
        h => dried(h - 6.3),
    })
}

/// The soil warms later and less than the air.
fn soil_temperature(hour: f64) -> f64 {
    round1(11.0 + 2.5 * daily(hour, 17.0))
}

impl Gadget for SoilSensor {
    fn device(&self) -> Device {
        Device::new(SOIL, "Soil sensor", "plant sensor", "Garden")
    }

    fn entities(&self) -> Vec<Entity> {
        vec![
            Entity::measured(SOIL_MOISTURE, "Soil moisture", SensorClass::Moisture, "%"),
            Entity::measured(
                SOIL_TEMPERATURE,
                "Soil temperature",
                SensorClass::Temperature,
                "°C",
            ),
            Entity::measured(SOIL_HUMIDITY, "Air humidity", SensorClass::Humidity, "%"),
            Entity::battery(SOIL_BATTERY),
        ]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![
            (SOIL_MOISTURE, number(moisture(self.world.hour))),
            (SOIL_TEMPERATURE, number(soil_temperature(self.world.hour))),
            (SOIL_HUMIDITY, number(self.world.outdoor_humidity)),
            (SOIL_BATTERY, number(self.world.battery)),
        ]
    }

    fn tick(&mut self, _: Instant, world: &World) -> Vec<&'static str> {
        self.world = *world;
        vec![SOIL_MOISTURE, SOIL_TEMPERATURE, SOIL_HUMIDITY, SOIL_BATTERY]
    }
}

pub(super) struct LightSensor {
    world: World,
}

impl LightSensor {
    pub(super) fn new() -> Self {
        Self {
            world: World::at(0),
        }
    }
}

impl Gadget for LightSensor {
    fn device(&self) -> Device {
        Device::new(LUX, "Garden light sensor", "light sensor", "Garden")
    }

    fn entities(&self) -> Vec<Entity> {
        vec![
            Entity::measured(
                LUX_ILLUMINANCE,
                "Illuminance",
                SensorClass::Illuminance,
                "lx",
            ),
            Entity::battery(LUX_BATTERY),
        ]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![
            (LUX_ILLUMINANCE, number(self.world.daylight)),
            (LUX_BATTERY, number(self.world.battery)),
        ]
    }

    fn tick(&mut self, _: Instant, world: &World) -> Vec<&'static str> {
        self.world = *world;
        vec![LUX_ILLUMINANCE, LUX_BATTERY]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bed_is_watered_in_the_morning_and_dries_through_the_day() {
        assert!(moisture(6.4) > moisture(18.0));
        assert!(moisture(5.9) < moisture(6.4));
        let day: Vec<f64> = (0..240)
            .map(|tenth| moisture(f64::from(tenth) / 10.0))
            .collect();
        assert!(day.iter().all(|m| (20.0..=46.0).contains(m)), "{day:?}");
        // No jump at midnight.
        assert!((moisture(23.99) - moisture(0.0)).abs() < 0.5);
    }
}
