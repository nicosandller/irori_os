//! A living room dehumidifier that dries the room while it's damper than its target.

use tokio::time::Instant;

use irori_protocol::types::{
    Capabilities, HumidifierAction, HumidifierCapabilities, HumidifierClass, HumidifierState,
    HumidityRange, Service, State,
};

use super::{Device, Entity, Gadget, Room, cannot};

const DRYER: &str = "dehumidifier";
const DRYER_HUMIDIFIER: &str = "dehumidifier-humidifier";

pub(super) struct Dehumidifier {
    state: HumidifierState,
}

fn capabilities() -> HumidifierCapabilities {
    HumidifierCapabilities {
        device_class: Some(HumidifierClass::Dehumidifier),
        humidity: HumidityRange {
            min: 35.0,
            max: 80.0,
        },
        modes: vec!["normal".into(), "sleep".into()],
    }
}

/// What a dehumidifier is doing in a room at `room` %: drying while it's above the target.
fn drying(dryer: &HumidifierState, room: f64) -> HumidifierAction {
    match dryer.target_humidity {
        _ if !dryer.on => HumidifierAction::Off,
        Some(target) if room > target => HumidifierAction::Drying,
        _ => HumidifierAction::Idle,
    }
}

impl Dehumidifier {
    pub(super) fn new() -> Self {
        Self {
            state: HumidifierState {
                on: true,
                target_humidity: Some(50.0),
                current_humidity: Some(50.0),
                mode: Some("normal".into()),
                action: Some(HumidifierAction::Idle),
            },
        }
    }

    /// Whether it dries, in the room as it last read it.
    fn reconsider(&mut self) {
        let room = self.state.current_humidity.unwrap_or(50.0);
        self.state.action = Some(drying(&self.state, room));
    }
}

impl Gadget for Dehumidifier {
    fn device(&self) -> Device {
        Device {
            unique_id: DRYER,
            name: "Demo dehumidifier",
            model: "Virtual dehumidifier",
            room: "Living room",
        }
    }

    fn entities(&self) -> Vec<Entity> {
        vec![Entity {
            unique_id: DRYER_HUMIDIFIER,
            name: None,
            capabilities: Capabilities::Humidifier(capabilities()),
            suggested_object_id: None,
            category: None,
        }]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![(DRYER_HUMIDIFIER, State::Humidifier(self.state.clone()))]
    }

    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        _: Instant,
    ) -> Result<&'static str, String> {
        match (unique_id, service) {
            (DRYER_HUMIDIFIER, Service::HumidifierTurnOn | Service::HumidifierTurnOff) => {
                self.state.on = matches!(service, Service::HumidifierTurnOn);
                self.reconsider();
            }
            (DRYER_HUMIDIFIER, Service::HumidifierSetHumidity(data)) => {
                self.state.target_humidity = Some(data.humidity);
                self.reconsider();
            }
            (DRYER_HUMIDIFIER, Service::HumidifierSetMode(data)) => {
                self.state.mode = Some(data.mode.clone());
            }
            _ => return Err(cannot(unique_id, service)),
        }
        Ok(DRYER_HUMIDIFIER)
    }

    /// Reads the room, and dries it while it's above the target.
    fn tick(&mut self, _: Instant, room: Room, _: u64) -> Vec<&'static str> {
        let before = self.state.clone();
        self.state.current_humidity = Some(room.humidity);
        self.reconsider();
        if self.state == before {
            Vec::new()
        } else {
            vec![DRYER_HUMIDIFIER]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(humidity: f64) -> Room {
        Room {
            temperature: 21.0,
            humidity,
        }
    }

    #[test]
    fn the_dehumidifier_dries_a_damp_room() {
        let now = Instant::now();
        let mut dryer = Dehumidifier::new();
        dryer.tick(now, room(64.0), 0);
        super::super::assert_fits(&dryer, &dryer.states());
        assert_eq!(dryer.state.action, Some(HumidifierAction::Drying));
        dryer.tick(now, room(45.0), 0);
        assert_eq!(dryer.state.action, Some(HumidifierAction::Idle));
        dryer
            .call(DRYER_HUMIDIFIER, &Service::HumidifierTurnOff, now)
            .expect("turns off");
        super::super::assert_fits(&dryer, &dryer.states());
        assert_eq!(dryer.state.action, Some(HumidifierAction::Off));
    }
}
