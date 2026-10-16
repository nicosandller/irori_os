//! A door's contact sensor, on a battery. When each door opens is its own habit.

use tokio::time::Instant;

use irori_protocol::types::{BinarySensorClass, State};

use super::{Device, Entity, Gadget, flag, number};
use crate::world::{World, during};

pub(super) struct Contact {
    device: Device,
    contact: &'static str,
    battery: &'static str,
    /// Whether the door is open in this world.
    opens: fn(&World) -> bool,
    open: bool,
    charge: f64,
}

impl Contact {
    pub(super) fn new(
        device: Device,
        contact: &'static str,
        battery: &'static str,
        opens: fn(&World) -> bool,
    ) -> Self {
        Self {
            device,
            contact,
            battery,
            opens,
            open: false,
            charge: 100.0,
        }
    }
}

/// The dining room's door to the patio: out after lunch, and after dinner.
pub(super) fn to_the_patio(world: &World) -> bool {
    during(world.hour, &[(13.3, 13.8), (20.6, 21.0)])
}

/// The dining room's door to the kitchen: back and forth as meals are brought in.
pub(super) fn to_the_kitchen(world: &World) -> bool {
    during(
        world.hour,
        &[(6.9, 7.2), (12.4, 12.7), (18.9, 19.3), (20.4, 20.6)],
    )
}

/// The living room's door to the garden: some air at midday, and the cat let out at night.
pub(super) fn to_the_garden(world: &World) -> bool {
    during(world.hour, &[(12.0, 13.0), (22.0, 22.3)])
}

impl Gadget for Contact {
    fn device(&self) -> Device {
        self.device
    }

    fn entities(&self) -> Vec<Entity> {
        vec![
            Entity::flag(self.contact, "Contact", BinarySensorClass::Door),
            Entity::battery(self.battery),
        ]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![
            (self.contact, flag(self.open)),
            (self.battery, number(self.charge)),
        ]
    }

    fn tick(&mut self, _: Instant, world: &World) -> Vec<&'static str> {
        self.open = (self.opens)(world);
        self.charge = world.battery;
        vec![self.contact, self.battery]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::DAY_SECS;

    #[test]
    fn every_door_opens_now_and_then_but_is_mostly_shut() {
        for opens in [to_the_patio, to_the_kitchen, to_the_garden] {
            let day: Vec<World> = (0..DAY_SECS / 2).map(|tick| World::at(tick * 2)).collect();
            let open = day.iter().filter(|world| opens(world)).count();
            assert!(open > 0 && open < day.len() / 4, "{open}");
        }
    }
}
