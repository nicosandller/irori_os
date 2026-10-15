//! A smart wall switch on the approach, wired to the outdoor lights. It's a switch, not a light:
//! what it powers is for a person to say, with a "switch as" helper.

use tokio::time::Instant;

use irori_protocol::types::{
    Capabilities, Service, State, SwitchCapabilities, SwitchClass, SwitchState,
};

use super::{Device, Entity, Gadget, cannot};
use crate::world::World;

const LIGHTS: &str = "outdoor-lights";
const LIGHTS_SWITCH: &str = "outdoor-lights-switch";

pub(super) struct OutdoorLights {
    on: bool,
}

impl OutdoorLights {
    pub(super) fn new() -> Self {
        Self { on: false }
    }
}

impl Gadget for OutdoorLights {
    fn device(&self) -> Device {
        Device::new(LIGHTS, "Outdoor lights", "smart wall switch", "Approach")
    }

    fn entities(&self) -> Vec<Entity> {
        vec![Entity::main(
            LIGHTS_SWITCH,
            Capabilities::Switch(SwitchCapabilities {
                device_class: Some(SwitchClass::Switch),
            }),
        )]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![(LIGHTS_SWITCH, State::Switch(SwitchState { on: self.on }))]
    }

    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        _: Instant,
    ) -> Result<&'static str, String> {
        match (unique_id, service) {
            (LIGHTS_SWITCH, Service::SwitchTurnOn) => self.on = true,
            (LIGHTS_SWITCH, Service::SwitchTurnOff) => self.on = false,
            _ => return Err(cannot(unique_id, service)),
        }
        Ok(LIGHTS_SWITCH)
    }

    fn tick(&mut self, _: Instant, _: &World) -> Vec<&'static str> {
        Vec::new()
    }
}
