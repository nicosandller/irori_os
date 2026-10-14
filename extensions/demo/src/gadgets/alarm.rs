//! A house alarm that counts down before it arms, so there's time to leave.

use tokio::time::{Duration, Instant};

use irori_protocol::types::{
    AlarmCapabilities, AlarmState, AlarmStatus, ArmMode, Capabilities, Service, State,
};

use super::{Device, Entity, Gadget, Room, cannot};

const ALARM: &str = "house-alarm";
const ALARM_PANEL: &str = "house-alarm-panel";

/// How long it says `arming` before it's armed.
const EXIT_DELAY: Duration = Duration::from_secs(5);

pub(super) struct HouseAlarm {
    status: AlarmStatus,
    /// What it's counting down to, and from when.
    arming: Option<(AlarmStatus, Instant)>,
}

impl HouseAlarm {
    pub(super) fn new() -> Self {
        Self {
            status: AlarmStatus::Disarmed,
            arming: None,
        }
    }

    fn arm(&mut self, mode: ArmMode, now: Instant) {
        self.status = AlarmStatus::Arming;
        self.arming = Some((mode.armed(), now));
    }
}

impl Gadget for HouseAlarm {
    fn device(&self) -> Device {
        Device {
            unique_id: ALARM,
            name: "Demo house alarm",
            model: "Virtual alarm panel",
            room: "Hallway",
        }
    }

    fn entities(&self) -> Vec<Entity> {
        vec![Entity {
            unique_id: ALARM_PANEL,
            name: None,
            // No code, so a flow can arm it as the last person leaves and disarm it as the
            // first comes home.
            capabilities: Capabilities::AlarmControlPanel(AlarmCapabilities {
                arm_modes: vec![ArmMode::Home, ArmMode::Away, ArmMode::Night],
                trigger: true,
                code_format: None,
                code_arm_required: false,
                code_disarm_required: false,
            }),
            suggested_object_id: None,
            category: None,
        }]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![(
            ALARM_PANEL,
            State::AlarmControlPanel(AlarmState { state: self.status }),
        )]
    }

    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        now: Instant,
    ) -> Result<&'static str, String> {
        match (unique_id, service) {
            (ALARM_PANEL, Service::AlarmArmHome(_)) => self.arm(ArmMode::Home, now),
            (ALARM_PANEL, Service::AlarmArmAway(_)) => self.arm(ArmMode::Away, now),
            (ALARM_PANEL, Service::AlarmArmNight(_)) => self.arm(ArmMode::Night, now),
            (ALARM_PANEL, Service::AlarmDisarm(_)) => {
                self.status = AlarmStatus::Disarmed;
                self.arming = None;
            }
            (ALARM_PANEL, Service::AlarmTrigger(_)) => {
                self.status = AlarmStatus::Triggered;
                self.arming = None;
            }
            _ => return Err(cannot(unique_id, service)),
        }
        Ok(ALARM_PANEL)
    }

    /// Armed once the time to leave is up.
    fn tick(&mut self, now: Instant, _: Room, _: u64) -> Vec<&'static str> {
        match self.arming {
            Some((armed, since)) if now >= since + EXIT_DELAY => {
                self.status = armed;
                self.arming = None;
                vec![ALARM_PANEL]
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use irori_protocol::types::LockCode;

    use super::*;

    const ROOM: Room = Room {
        temperature: 21.0,
        humidity: 50.0,
    };

    #[test]
    fn the_alarm_counts_down_before_it_is_armed_and_disarming_stops_the_count() {
        let now = Instant::now();
        let mut alarm = HouseAlarm::new();
        let away = Service::AlarmArmAway(LockCode::default());
        alarm.call(ALARM_PANEL, &away, now).expect("arms");
        assert_eq!(alarm.status, AlarmStatus::Arming);
        assert!(alarm.tick(now + Duration::from_secs(4), ROOM, 0).is_empty());
        assert_eq!(
            alarm.tick(now + Duration::from_secs(5), ROOM, 0),
            vec![ALARM_PANEL]
        );
        assert_eq!(alarm.status, AlarmStatus::ArmedAway);

        alarm.call(ALARM_PANEL, &away, now).expect("arms");
        let disarm = Service::AlarmDisarm(LockCode::default());
        alarm.call(ALARM_PANEL, &disarm, now).expect("disarms");
        assert!(
            alarm
                .tick(now + Duration::from_secs(60), ROOM, 0)
                .is_empty()
        );
        assert_eq!(alarm.status, AlarmStatus::Disarmed);

        let vacation = Service::AlarmArmVacation(LockCode::default());
        assert!(alarm.call(ALARM_PANEL, &vacation, now).is_err());
    }
}
