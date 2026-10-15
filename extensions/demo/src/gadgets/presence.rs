//! Knowing someone's there: an mmWave sensor, which sees people sitting still and says how far
//! away they are, and a PIR movement sensor, which only sees them move.

use tokio::time::Instant;

use irori_protocol::types::{BinarySensorClass, SensorClass, State};

use super::{Device, Entity, Gadget, flag, number};
use crate::world::{World, during};

pub(super) struct MmWave {
    device: Device,
    occupancy: &'static str,
    distance: &'static str,
    present: fn(&World) -> bool,
    world: World,
}

impl MmWave {
    pub(super) fn new(
        device: Device,
        occupancy: &'static str,
        distance: &'static str,
        present: fn(&World) -> bool,
    ) -> Self {
        Self {
            device,
            occupancy,
            distance,
            present,
            world: World::at(0),
        }
    }
}

/// Distance to the nearest person while someone's there, 0.6–3.0 m, wandering as they shift.
fn target_distance(world: &World) -> f64 {
    let wave = (world.hour * std::f64::consts::TAU / 1.5).sin();
    crate::world::round1(1.8 + 1.2 * wave)
}

/// The dining table at mealtimes.
pub(super) fn at_the_table(world: &World) -> bool {
    during(world.hour, &[(7.0, 7.8), (12.5, 13.3), (19.0, 20.5)])
}

/// Someone on the sofa, then not, three and a half minutes each: slower than the day, so a
/// flow waiting for the sofa to be empty for three minutes gets to fire.
pub(super) fn on_the_sofa(world: &World) -> bool {
    (world.secs / SOFA_SECS).is_multiple_of(2)
}

/// How long the sofa stays taken, and then empty.
const SOFA_SECS: u64 = 210;

/// Up in the night, heading out in the morning, home in the evening, off to bed.
pub(super) fn through_the_foyer(world: &World) -> bool {
    during(
        world.hour,
        &[(2.5, 2.8), (7.8, 8.2), (17.3, 17.7), (22.6, 22.9)],
    )
}

impl Gadget for MmWave {
    fn device(&self) -> Device {
        self.device
    }

    fn entities(&self) -> Vec<Entity> {
        vec![
            Entity::flag(self.occupancy, "Occupancy", BinarySensorClass::Occupancy),
            Entity::measured(self.distance, "Target distance", SensorClass::Distance, "m"),
        ]
    }

    /// Distance is unknown while nobody's there.
    fn states(&self) -> Vec<(&'static str, State)> {
        let present = (self.present)(&self.world);
        let mut states = vec![(self.occupancy, flag(present))];
        if present {
            states.push((self.distance, number(target_distance(&self.world))));
        }
        states
    }

    fn tick(&mut self, _: Instant, world: &World) -> Vec<&'static str> {
        self.world = *world;
        if (self.present)(world) {
            vec![self.occupancy, self.distance]
        } else {
            vec![self.occupancy]
        }
    }
}

pub(super) struct Movement {
    device: Device,
    motion: &'static str,
    battery: &'static str,
    moving: fn(&World) -> bool,
    world: World,
}

impl Movement {
    pub(super) fn new(
        device: Device,
        motion: &'static str,
        battery: &'static str,
        moving: fn(&World) -> bool,
    ) -> Self {
        Self {
            device,
            motion,
            battery,
            moving,
            world: World::at(0),
        }
    }
}

impl Gadget for Movement {
    fn device(&self) -> Device {
        self.device
    }

    fn entities(&self) -> Vec<Entity> {
        vec![
            Entity::flag(self.motion, "Motion", BinarySensorClass::Motion),
            Entity::battery(self.battery),
        ]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![
            (self.motion, flag((self.moving)(&self.world))),
            (self.battery, number(self.world.battery)),
        ]
    }

    fn tick(&mut self, _: Instant, world: &World) -> Vec<&'static str> {
        self.world = *world;
        vec![self.motion, self.battery]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sofa_takes_its_time_and_the_table_is_for_meals() {
        let sofa = |secs| on_the_sofa(&World::at(secs));
        assert!(sofa(0) && sofa(209) && !sofa(210) && sofa(420));
        let meals = (0..30)
            .filter(|tick| at_the_table(&World::at(tick * 2)))
            .count();
        assert!(meals > 2 && meals < 15, "{meals}");
        let mut table = MmWave::new(
            Device::new("t", "Table", "mmwave", "Dining room"),
            "t-occupancy",
            "t-distance",
            at_the_table,
        );
        for tick in 0..30 {
            let changed = table.tick(Instant::now(), &World::at(tick * 2));
            let states = table.states();
            assert_eq!(changed.len(), states.len());
            super::super::assert_fits(&table, &states);
            if let Some((_, State::Sensor(distance))) = states.get(1) {
                assert!(
                    matches!(distance.value, irori_protocol::types::SensorValue::Number(m) if (0.6..=3.0).contains(&m))
                );
            }
        }
    }
}
