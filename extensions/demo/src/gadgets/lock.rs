//! The foyer's smart lock, with a sensor in the frame that knows whether the door is open. It
//! locks itself again a while after it's unlocked, runs on a battery, and has firmware to update.
//! Someone lets themselves out in the morning and in in the evening.

use tokio::time::{Duration, Instant};

use irori_protocol::types::{
    BinarySensorClass, Capabilities, LockCapabilities, LockState, LockStatus, NumberCapabilities,
    NumberMode, NumberState, SensorClass, Service, State,
};

use super::firmware::Firmware;
use super::{Device, Entity, Gadget, cannot, flag, number};
use crate::world::{World, during};

const LOCK: &str = "foyer-lock";
const LOCK_LOCK: &str = "foyer-lock-lock";
const LOCK_DOOR: &str = "foyer-lock-door";
const LOCK_AUTO: &str = "foyer-lock-auto-lock";
const LOCK_BATTERY: &str = "foyer-lock-battery";
const LOCK_FIRMWARE: &str = "foyer-lock-firmware";

pub(super) struct FoyerLock {
    status: LockStatus,
    /// Seconds after unlocking before it locks again; 0 never.
    auto_lock: f64,
    unlocked_at: Option<Instant>,
    door_open: bool,
    /// Whether someone was coming or going at the last reading, to unlock once as they do.
    passing: bool,
    world: World,
    firmware: Firmware,
}

impl FoyerLock {
    pub(super) fn new() -> Self {
        Self {
            status: LockStatus::Locked,
            auto_lock: 30.0,
            unlocked_at: None,
            door_open: false,
            passing: false,
            world: World::at(0),
            firmware: Firmware::new(LOCK_FIRMWARE, "Foyer door lock firmware", 7),
        }
    }

    fn unlock(&mut self, now: Instant) {
        self.status = LockStatus::Unlocked;
        self.unlocked_at = Some(now);
    }
}

/// Someone heading out in the morning, and home in the evening.
fn passing_through(world: &World) -> bool {
    during(world.hour, &[(8.0, 8.2), (17.4, 17.6)])
}

impl Gadget for FoyerLock {
    fn device(&self) -> Device {
        Device::new(
            LOCK,
            "Foyer door lock",
            "smart lock with door sensor",
            "Foyer",
        )
    }

    fn entities(&self) -> Vec<Entity> {
        vec![
            Entity::main(LOCK_LOCK, Capabilities::Lock(LockCapabilities::default())),
            Entity::flag(LOCK_DOOR, "Door", BinarySensorClass::Door),
            Entity::named(
                LOCK_AUTO,
                "Lock again after",
                Capabilities::Number(NumberCapabilities {
                    min: 0.0,
                    max: 300.0,
                    step: 10.0,
                    unit: Some("s".into()),
                    device_class: Some(SensorClass::Duration),
                    mode: NumberMode::Box,
                }),
            )
            .config(),
            Entity::battery(LOCK_BATTERY),
            self.firmware.entity(),
        ]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![
            (LOCK_LOCK, State::Lock(LockState { state: self.status })),
            (LOCK_DOOR, flag(self.door_open)),
            (
                LOCK_AUTO,
                State::Number(NumberState {
                    value: self.auto_lock,
                }),
            ),
            (LOCK_BATTERY, number(self.world.battery)),
            self.firmware.state(),
        ]
    }

    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        now: Instant,
    ) -> Result<&'static str, String> {
        if self.firmware.is(unique_id) {
            return self.firmware.call(service, now);
        }
        match (unique_id, service) {
            (LOCK_LOCK, Service::LockLock(_)) if self.door_open => {
                Err("the door is open: close it first".to_owned())
            }
            (LOCK_LOCK, Service::LockLock(_)) => {
                self.status = LockStatus::Locked;
                self.unlocked_at = None;
                Ok(LOCK_LOCK)
            }
            (LOCK_LOCK, Service::LockUnlock(_)) => {
                self.unlock(now);
                Ok(LOCK_LOCK)
            }
            (LOCK_AUTO, Service::NumberSetValue(data)) => {
                self.auto_lock = data.value;
                Ok(LOCK_AUTO)
            }
            _ => Err(cannot(unique_id, service)),
        }
    }

    /// Unlocks for whoever's coming or going, and locks itself again once its time is up and
    /// the door is shut.
    fn tick(&mut self, now: Instant, world: &World) -> Vec<&'static str> {
        self.world = *world;
        let mut changed = vec![LOCK_DOOR, LOCK_BATTERY];
        let passing = passing_through(world);
        if passing && !self.passing {
            self.unlock(now);
            changed.push(LOCK_LOCK);
        }
        self.passing = passing;
        // The door is open for the first part of each coming and going.
        self.door_open = passing && self.status == LockStatus::Unlocked;
        if let Some(since) = self.unlocked_at
            && !self.door_open
            && self.auto_lock > 0.0
            && now >= since + Duration::from_secs_f64(self.auto_lock)
        {
            self.status = LockStatus::Locked;
            self.unlocked_at = None;
            changed.push(LOCK_LOCK);
        }
        if self.firmware.tick(now) {
            changed.push(LOCK_FIRMWARE);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use irori_protocol::types::{LockCode, NumberSetValue};

    use super::*;

    #[test]
    fn the_lock_lets_people_through_and_locks_itself_again_unless_told_not_to() {
        let now = Instant::now();
        let mut lock = FoyerLock::new();
        let leaving = World::at(20); // 08:00
        assert!(passing_through(&leaving));
        assert!(lock.tick(now, &leaving).contains(&LOCK_LOCK));
        assert!(lock.door_open);
        super::super::assert_fits(&lock, &lock.states());
        assert!(
            lock.call(LOCK_LOCK, &Service::LockLock(LockCode::default()), now)
                .is_err()
        );

        let later = World::at(25);
        assert!(
            !lock
                .tick(now + Duration::from_secs(29), &later)
                .contains(&LOCK_LOCK)
        );
        assert!(
            lock.tick(now + Duration::from_secs(30), &later)
                .contains(&LOCK_LOCK)
        );
        assert_eq!(lock.status, LockStatus::Locked);

        let never = Service::NumberSetValue(NumberSetValue { value: 0.0 });
        lock.call(LOCK_AUTO, &never, now).expect("takes it");
        lock.call(LOCK_LOCK, &Service::LockUnlock(LockCode::default()), now)
            .expect("unlocks");
        lock.tick(now + Duration::from_secs(3600), &later);
        assert_eq!(lock.status, LockStatus::Unlocked);
        assert!(
            lock.call(LOCK_FIRMWARE, &Service::UpdateInstall, now)
                .is_ok()
        );
    }
}
