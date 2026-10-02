//! A ceiling fan with three speeds, a breeze mode, and a winter direction.

use tokio::time::Instant;

use irori_protocol::types::{
    Capabilities, FanCapabilities, FanDirection, FanState, Service, State, percentage_to_speed,
    speed_to_percentage,
};

use super::{Device, Entity, Gadget, Room, cannot};

const FAN: &str = "ceiling-fan";
const FAN_FAN: &str = "ceiling-fan-fan";
const FAN_SPEEDS: u16 = 3;

pub(super) struct CeilingFan {
    fan: FanState,
}

impl CeilingFan {
    pub(super) fn new() -> Self {
        Self {
            fan: FanState {
                on: false,
                percentage: Some(speed_to_percentage(1, FAN_SPEEDS)),
                oscillating: None,
                direction: Some(FanDirection::Forward),
                preset_mode: None,
            },
        }
    }

    /// A speed set by percentage lands on one of its three, as a real fan's would, and leaves
    /// any preset mode.
    fn speed(&mut self, percentage: u8) {
        let speed = percentage_to_speed(percentage, FAN_SPEEDS).max(1);
        self.fan.percentage = Some(speed_to_percentage(speed, FAN_SPEEDS));
        self.fan.preset_mode = None;
    }
}

impl Gadget for CeilingFan {
    fn device(&self) -> Device {
        Device {
            unique_id: FAN,
            name: "Demo ceiling fan",
            model: "Virtual ceiling fan",
            room: "Living room",
        }
    }

    fn entities(&self) -> Vec<Entity> {
        vec![Entity {
            unique_id: FAN_FAN,
            name: None,
            capabilities: Capabilities::Fan(FanCapabilities {
                speed_count: FAN_SPEEDS,
                oscillate: false,
                direction: true,
                preset_modes: vec!["breeze".into()],
            }),
            suggested_object_id: None,
            category: None,
        }]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![(FAN_FAN, State::Fan(self.fan.clone()))]
    }

    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        _: Instant,
    ) -> Result<&'static str, String> {
        match (unique_id, service) {
            (FAN_FAN, Service::FanTurnOn(data)) => {
                self.fan.on = true;
                if let Some(percentage) = data.percentage {
                    self.speed(percentage);
                }
                if data.preset_mode.is_some() {
                    self.fan.preset_mode.clone_from(&data.preset_mode);
                }
            }
            (FAN_FAN, Service::FanTurnOff) => self.fan.on = false,
            (FAN_FAN, Service::FanSetPercentage(data)) => {
                if data.percentage == 0 {
                    self.fan.on = false;
                } else {
                    self.fan.on = true;
                    self.speed(data.percentage);
                }
            }
            (FAN_FAN, Service::FanSetDirection(data)) => self.fan.direction = Some(data.direction),
            (FAN_FAN, Service::FanSetPresetMode(data)) => {
                self.fan.on = true;
                self.fan.preset_mode = Some(data.preset_mode.clone());
            }
            _ => return Err(cannot(unique_id, service)),
        }
        Ok(FAN_FAN)
    }

    fn tick(&mut self, _: Instant, _: Room, _: u64) -> Vec<&'static str> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use irori_protocol::types::FanPercentage;

    use super::*;

    #[test]
    fn the_fan_lands_on_one_of_its_speeds() {
        let now = Instant::now();
        let mut fan = CeilingFan::new();
        let set = |percentage| Service::FanSetPercentage(FanPercentage { percentage });
        fan.call(FAN_FAN, &set(50), now).expect("takes it");
        assert!(fan.fan.on);
        assert_eq!(fan.fan.percentage, Some(66));
        super::super::assert_fits(&fan, &fan.states());
        fan.call(FAN_FAN, &set(0), now).expect("takes it");
        assert!(!fan.fan.on);
        assert_eq!(fan.fan.percentage, Some(66), "comes back on at it");
    }
}
