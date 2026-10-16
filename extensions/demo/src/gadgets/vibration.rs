//! A vibration sensor, on a battery: on the dining table, it feels the plates go down; in the
//! mailbox, the post arriving.

use tokio::time::Instant;

use irori_protocol::types::{BinarySensorClass, State};

use super::{Device, Entity, Gadget, flag, number};
use crate::world::{World, during};

pub(super) struct Vibration {
    device: Device,
    vibration: &'static str,
    battery: &'static str,
    shaken: fn(&World) -> bool,
    world: World,
}

impl Vibration {
    pub(super) fn new(
        device: Device,
        vibration: &'static str,
        battery: &'static str,
        shaken: fn(&World) -> bool,
    ) -> Self {
        Self {
            device,
            vibration,
            battery,
            shaken,
            world: World::at(0),
        }
    }
}

/// The table, as each meal is served.
pub(super) fn the_table(world: &World) -> bool {
    during(world.hour, &[(7.0, 7.3), (12.5, 12.8), (19.0, 19.4)])
}

/// The mailbox: the post mid-morning, and a parcel in the afternoon.
pub(super) fn the_post(world: &World) -> bool {
    during(world.hour, &[(10.4, 10.7), (15.2, 15.4)])
}

impl Gadget for Vibration {
    fn device(&self) -> Device {
        self.device
    }

    fn entities(&self) -> Vec<Entity> {
        vec![
            Entity::flag(self.vibration, "Vibration", BinarySensorClass::Vibration),
            Entity::battery(self.battery),
        ]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![
            (self.vibration, flag((self.shaken)(&self.world))),
            (self.battery, number(self.world.battery)),
        ]
    }

    fn tick(&mut self, _: Instant, world: &World) -> Vec<&'static str> {
        self.world = *world;
        vec![self.vibration, self.battery]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_post_comes_once_or_twice_a_day() {
        let shaken = (0..60).filter(|secs| the_post(&World::at(*secs))).count();
        assert!(shaken > 0 && shaken < 6, "{shaken}");
    }
}
