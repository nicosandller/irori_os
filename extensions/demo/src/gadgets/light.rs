//! A light: one on a relay that's only on or off, a dimmable one, or a lamp with colours. Lights
//! do as they're told and nothing on their own: turning them on at dusk is for a flow.

use tokio::time::Instant;

use irori_protocol::types::{
    Capabilities, ColorMode, ColorTempRange, LightCapabilities, LightState, LightTurnOn, Service,
    State,
};

use super::{Device, Entity, Gadget, cannot};
use crate::world::World;

/// What a light can do beside turning on and off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Shade {
    /// On a relay: no brightness.
    Relay,
    Dimmable,
    /// Dims, and takes a white or a colour.
    Colour,
}

pub(super) struct Light {
    device: Device,
    light: &'static str,
    shade: Shade,
    state: LightState,
}

impl Light {
    pub(super) fn new(device: Device, light: &'static str, shade: Shade) -> Self {
        let colour = shade == Shade::Colour;
        Self {
            device,
            light,
            shade,
            state: LightState {
                on: false,
                brightness: (shade != Shade::Relay).then_some(180),
                color_mode: colour.then_some(ColorMode::ColorTemp),
                color_temp_kelvin: colour.then_some(2700),
                rgb: None,
            },
        }
    }
}

/// Turns `light` on with `data`, or off.
fn apply(light: &mut LightState, shade: Shade, on: Option<&LightTurnOn>) {
    let Some(data) = on else {
        light.on = false;
        return;
    };
    light.on = true;
    if shade != Shade::Relay {
        light.brightness = data.brightness.or(light.brightness);
    }
    if shade != Shade::Colour {
        return;
    }
    // A lamp is one or the other: taking a colour puts its white away, and back.
    if let Some(kelvin) = data.color_temp_kelvin {
        light.color_temp_kelvin = Some(kelvin);
        light.rgb = None;
        light.color_mode = Some(ColorMode::ColorTemp);
    }
    if let Some(rgb) = data.rgb {
        light.rgb = Some(rgb);
        light.color_temp_kelvin = None;
        light.color_mode = Some(ColorMode::Rgb);
    }
}

impl Gadget for Light {
    fn device(&self) -> Device {
        self.device
    }

    fn entities(&self) -> Vec<Entity> {
        let colour = self.shade == Shade::Colour;
        vec![Entity::main(
            self.light,
            Capabilities::Light(LightCapabilities {
                brightness: self.shade != Shade::Relay,
                color_temp_kelvin: colour.then_some(ColorTempRange {
                    min: 2200,
                    max: 6500,
                }),
                rgb: colour,
            }),
        )]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![(self.light, State::Light(self.state.clone()))]
    }

    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        _: Instant,
    ) -> Result<&'static str, String> {
        match service {
            _ if unique_id != self.light => return Err(cannot(unique_id, service)),
            Service::LightTurnOn(data) => apply(&mut self.state, self.shade, Some(data)),
            Service::LightTurnOff => apply(&mut self.state, self.shade, None),
            _ => return Err(cannot(unique_id, service)),
        }
        Ok(self.light)
    }

    fn tick(&mut self, _: Instant, _: &World) -> Vec<&'static str> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light(shade: Shade) -> Light {
        Light::new(
            Device::new("l", "Light", "relay", "Garage"),
            "l-light",
            shade,
        )
    }

    #[test]
    fn a_relay_takes_no_brightness_and_a_lamp_takes_colours() {
        let now = Instant::now();
        let dim = Service::LightTurnOn(LightTurnOn {
            brightness: Some(64),
            ..LightTurnOn::default()
        });
        let mut relay = light(Shade::Relay);
        relay.call("l-light", &dim, now).expect("turns on");
        assert!(relay.state.on);
        assert_eq!(relay.state.brightness, None);
        super::super::assert_fits(&relay, &relay.states());

        let mut lamp = light(Shade::Colour);
        let red = Service::LightTurnOn(LightTurnOn {
            rgb: Some([255, 0, 0]),
            ..LightTurnOn::default()
        });
        lamp.call("l-light", &red, now).expect("takes a colour");
        assert_eq!(lamp.state.color_mode, Some(ColorMode::Rgb));
        assert_eq!(lamp.state.color_temp_kelvin, None);
        super::super::assert_fits(&lamp, &lamp.states());
        lamp.call("l-light", &Service::LightTurnOff, now)
            .expect("off");
        assert!(!lamp.state.on);
        assert!(lamp.call("l-light", &Service::SwitchTurnOn, now).is_err());
    }
}
