//! The garage door opener: a motor that takes a few seconds to run the door up or down, and the
//! safety beam across the opening that stops it closing on something. It opens as the car
//! leaves in the morning and comes back in the evening, and does as it's told in between.

use tokio::time::Instant;

use irori_protocol::types::{
    BinarySensorClass, Capabilities, CoverCapabilities, CoverClass, CoverState, OpenState, Service,
    State,
};

use super::{Device, Entity, Gadget, cannot, flag};
use crate::world::{World, during};

const DOOR: &str = "garage-door";
const DOOR_COVER: &str = "garage-door-cover";
const DOOR_OBSTRUCTION: &str = "garage-door-obstruction";
/// How far the door moves between readings, in percent.
const STEP: u8 = 50;

pub(super) struct GarageDoor {
    position: u8,
    target: u8,
    /// Whether the car was coming or going at the last reading, to act once when that changes.
    car_moving: bool,
    /// Something in the opening: the car, on its way through.
    blocked: bool,
    /// Opened for the car: it closes again once it's all the way up and the car is through.
    for_the_car: bool,
}

impl GarageDoor {
    pub(super) fn new() -> Self {
        Self {
            position: 0,
            target: 0,
            car_moving: false,
            blocked: false,
            for_the_car: false,
        }
    }

    fn state(&self) -> CoverState {
        let state = match self.target.cmp(&self.position) {
            std::cmp::Ordering::Greater => OpenState::Opening,
            std::cmp::Ordering::Less => OpenState::Closing,
            std::cmp::Ordering::Equal if self.position == 0 => OpenState::Closed,
            std::cmp::Ordering::Equal => OpenState::Open,
        };
        CoverState {
            state,
            position: Some(self.position),
            tilt: None,
        }
    }
}

/// The car leaving and coming back: the door is up while it does.
fn car_moving(world: &World) -> bool {
    during(world.hour, &[(7.95, 8.35), (17.45, 17.85)])
}

impl Gadget for GarageDoor {
    fn device(&self) -> Device {
        Device::new(DOOR, "Garage door", "garage door opener", "Garage")
    }

    fn entities(&self) -> Vec<Entity> {
        vec![
            Entity::main(
                DOOR_COVER,
                Capabilities::Cover(CoverCapabilities {
                    device_class: Some(CoverClass::Garage),
                    position: true,
                    tilt: false,
                    stop: true,
                }),
            ),
            Entity::flag(DOOR_OBSTRUCTION, "Obstruction", BinarySensorClass::Problem),
        ]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![
            (DOOR_COVER, State::Cover(self.state())),
            (DOOR_OBSTRUCTION, flag(self.blocked)),
        ]
    }

    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        _: Instant,
    ) -> Result<&'static str, String> {
        match (unique_id, service) {
            (DOOR_COVER, Service::CoverOpen) => {
                self.target = 100;
                self.for_the_car = false;
            }
            (DOOR_COVER, Service::CoverClose) if self.blocked => {
                return Err("something is in the way of the door".to_owned());
            }
            (DOOR_COVER, Service::CoverClose) => self.target = 0,
            (DOOR_COVER, Service::CoverStop) => self.target = self.position,
            (DOOR_COVER, Service::CoverSetPosition(data)) => self.target = data.position,
            _ => return Err(cannot(unique_id, service)),
        }
        Ok(DOOR_COVER)
    }

    fn tick(&mut self, _: Instant, world: &World) -> Vec<&'static str> {
        let before = self.states();
        let moving = car_moving(world);
        if moving && !self.car_moving {
            self.target = 100;
            self.for_the_car = true;
        }
        self.car_moving = moving;
        if self.for_the_car && !moving && self.position == 100 {
            self.target = 0;
            self.for_the_car = false;
        }
        // The car is in the opening while the door is fully up and it's coming or going.
        self.blocked = moving && self.position == 100;
        if self.blocked && self.target < self.position {
            self.target = self.position;
        }
        self.position = if self.target > self.position {
            self.position.saturating_add(STEP).min(self.target)
        } else {
            self.position.saturating_sub(STEP).max(self.target)
        };
        if self.states() == before {
            Vec::new()
        } else {
            vec![DOOR_COVER, DOOR_OBSTRUCTION]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_door_goes_up_for_the_car_and_down_after_and_wont_close_on_it() {
        let now = Instant::now();
        let mut door = GarageDoor::new();
        let mut seen = Vec::new();
        for secs in 0..60 {
            door.tick(now, &World::at(secs));
            seen.push(door.state().state);
            super::super::assert_fits(&door, &door.states());
        }
        for state in [
            OpenState::Opening,
            OpenState::Open,
            OpenState::Closing,
            OpenState::Closed,
        ] {
            assert!(seen.contains(&state), "{state:?} in {seen:?}");
        }
        door.position = 100;
        door.target = 100;
        door.blocked = true;
        assert!(door.call(DOOR_COVER, &Service::CoverClose, now).is_err());
    }
}
