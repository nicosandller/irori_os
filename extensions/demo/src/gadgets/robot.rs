//! A robot vacuum that cleans for a while, goes home by itself, and docks.

use tokio::time::{Duration, Instant};

use irori_protocol::types::{
    Capabilities, Service, State, VacuumActivity, VacuumCapabilities, VacuumState,
};

use super::{Device, Entity, Gadget, Room, cannot};

const ROBOT: &str = "robot-vacuum";
const ROBOT_VACUUM: &str = "robot-vacuum-vacuum";

/// How long a clean takes, and how long the way home.
const CLEANS_FOR: Duration = Duration::from_secs(40);
const HOME_IN: Duration = Duration::from_secs(6);

pub(super) struct RobotVacuum {
    activity: VacuumActivity,
    fan_speed: String,
    /// When what it's doing now ends by itself: a clean, or the way home.
    until: Option<Instant>,
}

impl RobotVacuum {
    pub(super) fn new() -> Self {
        Self {
            activity: VacuumActivity::Docked,
            fan_speed: "normal".to_owned(),
            until: None,
        }
    }

    fn go(&mut self, activity: VacuumActivity, until: Option<Instant>) {
        self.activity = activity;
        self.until = until;
    }
}

impl Gadget for RobotVacuum {
    fn device(&self) -> Device {
        Device {
            unique_id: ROBOT,
            name: "Demo robot vacuum",
            model: "Virtual robot vacuum",
            room: "Living room",
        }
    }

    fn entities(&self) -> Vec<Entity> {
        vec![Entity {
            unique_id: ROBOT_VACUUM,
            name: None,
            capabilities: Capabilities::Vacuum(VacuumCapabilities {
                start: true,
                pause: true,
                stop: true,
                return_home: true,
                locate: false,
                clean_spot: false,
                fan_speeds: vec!["quiet".into(), "normal".into(), "turbo".into()],
            }),
            suggested_object_id: None,
            category: None,
        }]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![(
            ROBOT_VACUUM,
            State::Vacuum(VacuumState {
                activity: self.activity,
                fan_speed: Some(self.fan_speed.clone()),
            }),
        )]
    }

    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        now: Instant,
    ) -> Result<&'static str, String> {
        match (unique_id, service) {
            (ROBOT_VACUUM, Service::VacuumStart) => {
                self.go(VacuumActivity::Cleaning, Some(now + CLEANS_FOR));
            }
            (ROBOT_VACUUM, Service::VacuumPause) => self.go(VacuumActivity::Paused, None),
            (ROBOT_VACUUM, Service::VacuumStop) => self.go(VacuumActivity::Idle, None),
            (ROBOT_VACUUM, Service::VacuumReturnToBase) => {
                self.go(VacuumActivity::Returning, Some(now + HOME_IN));
            }
            (ROBOT_VACUUM, Service::VacuumSetFanSpeed(data)) => {
                self.fan_speed.clone_from(&data.fan_speed);
            }
            _ => return Err(cannot(unique_id, service)),
        }
        Ok(ROBOT_VACUUM)
    }

    /// Done cleaning, it heads home; home, it docks.
    fn tick(&mut self, now: Instant, _: Room, _: u64) -> Vec<&'static str> {
        match (self.activity, self.until) {
            (VacuumActivity::Cleaning, Some(until)) if now >= until => {
                self.go(VacuumActivity::Returning, Some(now + HOME_IN));
            }
            (VacuumActivity::Returning, Some(until)) if now >= until => {
                self.go(VacuumActivity::Docked, None);
            }
            _ => return Vec::new(),
        }
        vec![ROBOT_VACUUM]
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
    fn the_robot_cleans_goes_home_and_docks_by_itself() {
        let now = Instant::now();
        let mut robot = RobotVacuum::new();
        robot
            .call(ROBOT_VACUUM, &Service::VacuumStart, now)
            .expect("starts");
        assert_eq!(robot.activity, VacuumActivity::Cleaning);
        assert!(
            robot
                .tick(now + Duration::from_secs(39), ROOM, 0)
                .is_empty()
        );
        let home = now + CLEANS_FOR;
        assert_eq!(robot.tick(home, ROOM, 0), vec![ROBOT_VACUUM]);
        assert_eq!(robot.activity, VacuumActivity::Returning);
        assert_eq!(robot.tick(home + HOME_IN, ROOM, 0), vec![ROBOT_VACUUM]);
        assert_eq!(robot.activity, VacuumActivity::Docked);

        // Paused, it stays where it is until told otherwise.
        robot
            .call(ROBOT_VACUUM, &Service::VacuumStart, now)
            .expect("starts");
        robot
            .call(ROBOT_VACUUM, &Service::VacuumPause, now)
            .expect("pauses");
        assert!(
            robot
                .tick(now + Duration::from_secs(600), ROOM, 0)
                .is_empty()
        );
        assert!(
            robot
                .call(ROBOT_VACUUM, &Service::VacuumLocate, now)
                .is_err()
        );
    }
}
