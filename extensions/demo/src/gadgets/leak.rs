//! A water leak sensor on the kitchen floor, on a battery. Once in a while the dishwasher's
//! door drips: wet for a few seconds, then dry again.

use tokio::time::Instant;

use irori_protocol::types::{BinarySensorClass, State};

use super::{Device, Entity, Gadget, flag, number};
use crate::world::World;

const LEAK: &str = "kitchen-leak";
const LEAK_MOISTURE: &str = "kitchen-leak-moisture";
const LEAK_BATTERY: &str = "kitchen-leak-battery";

/// A drip every fifteen minutes, wet for twenty seconds: rarer than anything else here.
const DRIP_EVERY: u64 = 900;
const DRIP_AT: u64 = 610;
const DRIP_FOR: u64 = 20;

pub(super) struct LeakSensor {
    world: World,
}

impl LeakSensor {
    pub(super) fn new() -> Self {
        Self {
            world: World::at(0),
        }
    }
}

fn wet(secs: u64) -> bool {
    (DRIP_AT..DRIP_AT + DRIP_FOR).contains(&(secs % DRIP_EVERY))
}

impl Gadget for LeakSensor {
    fn device(&self) -> Device {
        Device::new(LEAK, "Water leak sensor", "water leak sensor", "Kitchen")
    }

    fn entities(&self) -> Vec<Entity> {
        vec![
            Entity::flag(LEAK_MOISTURE, "Leak", BinarySensorClass::Moisture),
            Entity::battery(LEAK_BATTERY),
        ]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![
            (LEAK_MOISTURE, flag(wet(self.world.secs))),
            (LEAK_BATTERY, number(self.world.battery)),
        ]
    }

    fn tick(&mut self, _: Instant, world: &World) -> Vec<&'static str> {
        self.world = *world;
        vec![LEAK_MOISTURE, LEAK_BATTERY]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_drips_now_and_then_and_dries_again() {
        assert!(!wet(0) && !wet(609) && wet(610) && wet(629) && !wet(630));
        assert!(wet(DRIP_EVERY + DRIP_AT));
    }
}
