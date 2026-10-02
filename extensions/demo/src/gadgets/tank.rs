//! A heat pump hot water tank with its own switch: it heats towards its target while it's on,
//! and cools a little while it isn't.

use tokio::time::Instant;

use irori_protocol::types::{
    Capabilities, Service, State, WaterHeaterCapabilities, WaterHeaterMode, WaterHeaterState,
};

use super::{Device, Entity, Gadget, Room, cannot};

const TANK: &str = "hot-water-tank";
const TANK_HEATER: &str = "hot-water-tank-heater";

pub(super) struct HotWaterTank {
    state: WaterHeaterState,
    /// The mode it's in when its switch is on.
    on_mode: WaterHeaterMode,
}

fn capabilities() -> WaterHeaterCapabilities {
    WaterHeaterCapabilities {
        operation_modes: vec![
            WaterHeaterMode::Eco,
            WaterHeaterMode::HeatPump,
            WaterHeaterMode::Performance,
        ],
        min_temp: 40.0,
        max_temp: 65.0,
        temp_step: 1.0,
        target_temperature: true,
        on_off: true,
    }
}

impl HotWaterTank {
    pub(super) fn new() -> Self {
        Self {
            state: WaterHeaterState {
                operation_mode: WaterHeaterMode::HeatPump,
                current_temperature: Some(50.0),
                target_temperature: Some(55.0),
            },
            on_mode: WaterHeaterMode::HeatPump,
        }
    }

    fn set_mode(&mut self, mode: WaterHeaterMode) {
        self.on_mode = mode;
        self.state.operation_mode = mode;
    }
}

impl Gadget for HotWaterTank {
    fn device(&self) -> Device {
        Device {
            unique_id: TANK,
            name: "Demo hot water tank",
            model: "Virtual heat pump water heater",
            room: "Kitchen",
        }
    }

    fn entities(&self) -> Vec<Entity> {
        vec![Entity {
            unique_id: TANK_HEATER,
            name: None,
            capabilities: Capabilities::WaterHeater(capabilities()),
            category: None,
        }]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![(TANK_HEATER, State::WaterHeater(self.state.clone()))]
    }

    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        _: Instant,
    ) -> Result<&'static str, String> {
        match (unique_id, service) {
            (TANK_HEATER, Service::WaterHeaterTurnOff) => {
                self.state.operation_mode = WaterHeaterMode::Off;
            }
            (TANK_HEATER, Service::WaterHeaterTurnOn) => self.state.operation_mode = self.on_mode,
            (TANK_HEATER, Service::WaterHeaterSetOperationMode(data)) => {
                self.set_mode(data.operation_mode);
            }
            (TANK_HEATER, Service::WaterHeaterSetTemperature(data)) => {
                if let Some(mode) = data.operation_mode {
                    self.set_mode(mode);
                }
                self.state.target_temperature = Some(data.temperature);
            }
            _ => return Err(cannot(unique_id, service)),
        }
        Ok(TANK_HEATER)
    }

    /// A degree a reading towards its target while it's on; a little lost to the room while it
    /// isn't.
    fn tick(&mut self, _: Instant, _: Room) -> Vec<&'static str> {
        let water = self.state.current_temperature.unwrap_or(50.0);
        let heated = match (self.state.operation_mode, self.state.target_temperature) {
            (WaterHeaterMode::Off, _) => (water - 0.2).max(20.0),
            (_, Some(target)) if water < target => (water + 1.0).min(target),
            _ => water,
        };
        if (heated - water).abs() > f64::EPSILON {
            self.state.current_temperature = Some((heated * 10.0).round() / 10.0);
            vec![TANK_HEATER]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOM: Room = Room {
        temperature: 21.0,
        humidity: 50.0,
    };

    #[test]
    fn the_tank_heats_while_on_and_cools_while_off() {
        let now = Instant::now();
        let mut tank = HotWaterTank::new();
        tank.tick(now, ROOM);
        assert_eq!(tank.state.current_temperature, Some(51.0));
        tank.call(TANK_HEATER, &Service::WaterHeaterTurnOff, now)
            .expect("switches off");
        super::super::assert_fits(&tank, &tank.states());
        tank.tick(now, ROOM);
        super::super::assert_fits(&tank, &tank.states());
        assert_eq!(tank.state.current_temperature, Some(50.8));
        tank.call(TANK_HEATER, &Service::WaterHeaterTurnOn, now)
            .expect("switches on");
        assert_eq!(tank.state.operation_mode, WaterHeaterMode::HeatPump);
    }
}
