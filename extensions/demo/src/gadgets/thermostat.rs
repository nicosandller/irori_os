//! The living room's thermostat: heats, or follows its schedule, in °C, and reads the room.

use tokio::time::Instant;

use irori_protocol::types::{
    Capabilities, ClimateCapabilities, ClimateState, HvacAction, HvacMode, Service, State,
};

use super::{Device, Entity, Gadget, Room, cannot};

const THERMOSTAT: &str = "thermostat";
const THERMOSTAT_CLIMATE: &str = "thermostat-climate";

pub(super) struct Thermostat {
    state: ClimateState,
    /// The mode `turn_on` goes back to.
    on_mode: HvacMode,
}

fn capabilities() -> ClimateCapabilities {
    ClimateCapabilities {
        hvac_modes: vec![HvacMode::Off, HvacMode::Heat, HvacMode::Auto],
        min_temp: 7.0,
        max_temp: 30.0,
        temp_step: 0.5,
        target_temperature: true,
        target_temperature_range: false,
        target_humidity: None,
        fan_modes: Vec::new(),
        swing_modes: Vec::new(),
        preset_modes: vec!["comfort".into(), "eco".into(), "away".into()],
    }
}

/// What a thermostat is doing in a room at `room` °C: heating while it's below the target.
fn heating(thermostat: &ClimateState, room: f64) -> HvacAction {
    match (thermostat.hvac_mode, thermostat.target_temperature) {
        (HvacMode::Off, _) => HvacAction::Off,
        (_, Some(target)) if room < target => HvacAction::Heating,
        _ => HvacAction::Idle,
    }
}

impl Thermostat {
    pub(super) fn new() -> Self {
        Self {
            state: ClimateState {
                hvac_action: Some(HvacAction::Idle),
                current_temperature: Some(21.0),
                target_temperature: Some(21.0),
                preset_mode: Some("comfort".into()),
                ..ClimateState::in_mode(HvacMode::Heat)
            },
            on_mode: HvacMode::Heat,
        }
    }

    fn set_mode(&mut self, mode: HvacMode) {
        self.state.hvac_mode = mode;
        if mode != HvacMode::Off {
            self.on_mode = mode;
        }
        self.reconsider();
    }

    /// Whether it heats, in the room as it last read it.
    fn reconsider(&mut self) {
        let room = self.state.current_temperature.unwrap_or(21.0);
        self.state.hvac_action = Some(heating(&self.state, room));
    }
}

impl Gadget for Thermostat {
    fn device(&self) -> Device {
        Device {
            unique_id: THERMOSTAT,
            name: "Demo thermostat",
            model: "Virtual thermostat",
            room: "Living room",
        }
    }

    fn entities(&self) -> Vec<Entity> {
        vec![Entity {
            unique_id: THERMOSTAT_CLIMATE,
            name: None,
            capabilities: Capabilities::Climate(capabilities()),
            suggested_object_id: None,
            category: None,
        }]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![(THERMOSTAT_CLIMATE, State::Climate(self.state.clone()))]
    }

    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        _: Instant,
    ) -> Result<&'static str, String> {
        match (unique_id, service) {
            (THERMOSTAT_CLIMATE, Service::ClimateSetHvacMode(data)) => {
                self.set_mode(data.hvac_mode)
            }
            (THERMOSTAT_CLIMATE, Service::ClimateTurnOn) => self.set_mode(self.on_mode),
            (THERMOSTAT_CLIMATE, Service::ClimateTurnOff) => self.set_mode(HvacMode::Off),
            (THERMOSTAT_CLIMATE, Service::ClimateSetTemperature(data)) => {
                if let Some(mode) = data.hvac_mode {
                    self.set_mode(mode);
                }
                if data.temperature.is_some() {
                    self.state.target_temperature = data.temperature;
                    // A target set by hand is no preset's.
                    self.state.preset_mode = None;
                }
                self.reconsider();
            }
            // Each preset is a target: comfortable, saving, or nobody home.
            (THERMOSTAT_CLIMATE, Service::ClimateSetPresetMode(data)) => {
                self.state.target_temperature = Some(match data.preset_mode.as_str() {
                    "comfort" => 21.0,
                    "eco" => 18.5,
                    _ => 15.0,
                });
                self.state.preset_mode = Some(data.preset_mode.clone());
            }
            _ => return Err(cannot(unique_id, service)),
        }
        Ok(THERMOSTAT_CLIMATE)
    }

    /// Reads the room, and heats while it's below the target.
    fn tick(&mut self, _: Instant, room: Room, _: u64) -> Vec<&'static str> {
        let before = self.state.clone();
        self.state.current_temperature = Some(room.temperature);
        self.state.hvac_action = Some(heating(&self.state, room.temperature));
        if self.state == before {
            Vec::new()
        } else {
            vec![THERMOSTAT_CLIMATE]
        }
    }
}

#[cfg(test)]
mod tests {
    use irori_protocol::types::{ClimateHvacMode, ClimatePresetMode};

    use super::*;

    fn room(temperature: f64) -> Room {
        Room {
            temperature,
            humidity: 50.0,
        }
    }

    #[test]
    fn the_thermostat_heats_a_cold_room_and_comes_back_on_as_it_was() {
        let now = Instant::now();
        let mut thermostat = Thermostat::new();
        thermostat.tick(now, room(19.0), 0);
        super::super::assert_fits(&thermostat, &thermostat.states());
        assert_eq!(thermostat.state.hvac_action, Some(HvacAction::Heating));
        thermostat.tick(now, room(22.0), 0);
        assert_eq!(thermostat.state.hvac_action, Some(HvacAction::Idle));
        assert!(
            thermostat.tick(now, room(22.0), 0).is_empty(),
            "nothing new to say"
        );

        let auto = Service::ClimateSetHvacMode(ClimateHvacMode {
            hvac_mode: HvacMode::Auto,
        });
        thermostat
            .call(THERMOSTAT_CLIMATE, &auto, now)
            .expect("takes it");
        thermostat
            .call(THERMOSTAT_CLIMATE, &Service::ClimateTurnOff, now)
            .expect("turns off");
        assert_eq!(thermostat.state.hvac_action, Some(HvacAction::Off));
        thermostat
            .call(THERMOSTAT_CLIMATE, &Service::ClimateTurnOn, now)
            .expect("turns on");
        super::super::assert_fits(&thermostat, &thermostat.states());
        assert_eq!(thermostat.state.hvac_mode, HvacMode::Auto);

        let away = Service::ClimateSetPresetMode(ClimatePresetMode {
            preset_mode: "away".into(),
        });
        thermostat
            .call(THERMOSTAT_CLIMATE, &away, now)
            .expect("takes it");
        assert_eq!(thermostat.state.target_temperature, Some(15.0));
    }
}
