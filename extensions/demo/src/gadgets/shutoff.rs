//! A main water shut-off with a leak alarm that sounds for as long as it's told.

use tokio::time::{Duration, Instant};

use irori_protocol::types::{
    Capabilities, OpenState, Service, SirenCapabilities, SirenState, State, ValveCapabilities,
    ValveClass, ValveState,
};

use super::{Device, Entity, Gadget, Room, cannot};

const SHUTOFF: &str = "water-shutoff";
const SHUTOFF_VALVE: &str = "water-shutoff-valve";
const SHUTOFF_ALARM: &str = "water-shutoff-alarm";

pub(super) struct WaterShutoff {
    valve: OpenState,
    alarm: bool,
    alarm_until: Option<Instant>,
}

impl WaterShutoff {
    pub(super) fn new() -> Self {
        Self {
            valve: OpenState::Open,
            alarm: false,
            alarm_until: None,
        }
    }
}

impl Gadget for WaterShutoff {
    fn device(&self) -> Device {
        Device {
            unique_id: SHUTOFF,
            name: "Demo water shut-off",
            model: "Virtual water valve with leak alarm",
            room: "Kitchen",
        }
    }

    fn entities(&self) -> Vec<Entity> {
        vec![
            Entity {
                unique_id: SHUTOFF_VALVE,
                name: Some("Main water"),
                capabilities: Capabilities::Valve(ValveCapabilities {
                    device_class: Some(ValveClass::Water),
                    position: false,
                    stop: false,
                }),
                suggested_object_id: None,
                category: None,
            },
            Entity {
                unique_id: SHUTOFF_ALARM,
                name: Some("Leak alarm"),
                capabilities: Capabilities::Siren(SirenCapabilities {
                    tones: vec!["beep".into(), "alarm".into()],
                    volume: true,
                    duration: true,
                }),
                suggested_object_id: None,
                category: None,
            },
        ]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![
            (
                SHUTOFF_VALVE,
                State::Valve(ValveState {
                    state: self.valve,
                    position: None,
                }),
            ),
            (SHUTOFF_ALARM, State::Siren(SirenState { on: self.alarm })),
        ]
    }

    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        now: Instant,
    ) -> Result<&'static str, String> {
        match (unique_id, service) {
            (SHUTOFF_VALVE, Service::ValveOpen) => {
                self.valve = OpenState::Open;
                Ok(SHUTOFF_VALVE)
            }
            (SHUTOFF_VALVE, Service::ValveClose) => {
                self.valve = OpenState::Closed;
                Ok(SHUTOFF_VALVE)
            }
            (SHUTOFF_ALARM, Service::SirenTurnOn(data)) => {
                self.alarm = true;
                self.alarm_until = data
                    .duration
                    .map(|secs| now + Duration::from_secs(secs.into()));
                Ok(SHUTOFF_ALARM)
            }
            (SHUTOFF_ALARM, Service::SirenTurnOff) => {
                self.alarm = false;
                self.alarm_until = None;
                Ok(SHUTOFF_ALARM)
            }
            _ => Err(cannot(unique_id, service)),
        }
    }

    /// The alarm stops when its time is up.
    fn tick(&mut self, now: Instant, _: Room, _: u64) -> Vec<&'static str> {
        if self.alarm_until.is_some_and(|until| now >= until) {
            self.alarm = false;
            self.alarm_until = None;
            vec![SHUTOFF_ALARM]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use irori_protocol::types::SirenTurnOn;

    use super::*;

    const ROOM: Room = Room {
        temperature: 21.0,
        humidity: 50.0,
    };

    #[test]
    fn the_alarm_stops_when_its_time_is_up() {
        let now = Instant::now();
        let mut shutoff = WaterShutoff::new();
        let sound = Service::SirenTurnOn(SirenTurnOn {
            tone: Some("beep".into()),
            volume_level: None,
            duration: Some(5),
        });
        shutoff.call(SHUTOFF_ALARM, &sound, now).expect("sounds");
        assert!(shutoff.alarm);
        super::super::assert_fits(&shutoff, &shutoff.states());
        assert!(
            shutoff
                .tick(now + Duration::from_secs(4), ROOM, 0)
                .is_empty()
        );
        assert_eq!(
            shutoff.tick(now + Duration::from_secs(5), ROOM, 0),
            vec![SHUTOFF_ALARM]
        );
        assert!(!shutoff.alarm);
    }
}
