//! A front door lock that locks itself again a while after it's unlocked.

use tokio::time::{Duration, Instant};

use irori_protocol::types::{
    Capabilities, EntityCategory, LockCapabilities, LockState, LockStatus, NumberCapabilities,
    NumberMode, NumberState, SensorClass, Service, State,
};

use super::{Device, Entity, Gadget, Room, cannot};

const LOCK: &str = "front-door-lock";
const LOCK_LOCK: &str = "front-door-lock-lock";
const LOCK_AUTO: &str = "front-door-lock-auto-lock";

pub(super) struct FrontDoorLock {
    status: LockStatus,
    /// Seconds after unlocking before it locks again; 0 never.
    auto_lock: f64,
    unlocked_at: Option<Instant>,
}

impl FrontDoorLock {
    pub(super) fn new() -> Self {
        Self {
            status: LockStatus::Locked,
            auto_lock: 30.0,
            unlocked_at: None,
        }
    }
}

impl Gadget for FrontDoorLock {
    fn device(&self) -> Device {
        Device {
            unique_id: LOCK,
            name: "Demo front door lock",
            model: "Virtual smart lock",
            room: "Hallway",
        }
    }

    fn entities(&self) -> Vec<Entity> {
        vec![
            Entity {
                unique_id: LOCK_LOCK,
                name: None,
                capabilities: Capabilities::Lock(LockCapabilities::default()),
                suggested_object_id: None,
                category: None,
            },
            Entity {
                unique_id: LOCK_AUTO,
                name: Some("Lock again after"),
                capabilities: Capabilities::Number(NumberCapabilities {
                    min: 0.0,
                    max: 300.0,
                    step: 10.0,
                    unit: Some("s".into()),
                    device_class: Some(SensorClass::Duration),
                    mode: NumberMode::Box,
                }),
                suggested_object_id: None,
                category: Some(EntityCategory::Config),
            },
        ]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![
            (LOCK_LOCK, State::Lock(LockState { state: self.status })),
            (
                LOCK_AUTO,
                State::Number(NumberState {
                    value: self.auto_lock,
                }),
            ),
        ]
    }

    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        now: Instant,
    ) -> Result<&'static str, String> {
        match (unique_id, service) {
            (LOCK_LOCK, Service::LockLock(_)) => {
                self.status = LockStatus::Locked;
                self.unlocked_at = None;
                Ok(LOCK_LOCK)
            }
            (LOCK_LOCK, Service::LockUnlock(_)) => {
                self.status = LockStatus::Unlocked;
                self.unlocked_at = Some(now);
                Ok(LOCK_LOCK)
            }
            (LOCK_AUTO, Service::NumberSetValue(data)) => {
                self.auto_lock = data.value;
                Ok(LOCK_AUTO)
            }
            _ => Err(cannot(unique_id, service)),
        }
    }

    /// Locks itself again once its time is up.
    fn tick(&mut self, now: Instant, _: Room, _: u64) -> Vec<&'static str> {
        match self.unlocked_at {
            Some(since)
                if self.auto_lock > 0.0
                    && now >= since + Duration::from_secs_f64(self.auto_lock) =>
            {
                self.status = LockStatus::Locked;
                self.unlocked_at = None;
                vec![LOCK_LOCK]
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use irori_protocol::types::{LockCode, NumberSetValue};

    use super::*;

    const ROOM: Room = Room {
        temperature: 21.0,
        humidity: 50.0,
    };

    #[test]
    fn the_lock_locks_itself_again_unless_told_not_to() {
        let now = Instant::now();
        let mut lock = FrontDoorLock::new();
        let unlock = Service::LockUnlock(LockCode::default());
        lock.call(LOCK_LOCK, &unlock, now).expect("unlocks");
        assert!(lock.tick(now + Duration::from_secs(29), ROOM, 0).is_empty());
        assert_eq!(
            lock.tick(now + Duration::from_secs(30), ROOM, 0),
            vec![LOCK_LOCK]
        );
        assert_eq!(lock.status, LockStatus::Locked);

        let never = Service::NumberSetValue(NumberSetValue { value: 0.0 });
        lock.call(LOCK_AUTO, &never, now).expect("takes it");
        lock.call(LOCK_LOCK, &unlock, now).expect("unlocks");
        assert!(
            lock.tick(now + Duration::from_secs(3600), ROOM, 0)
                .is_empty()
        );
        assert_eq!(lock.status, LockStatus::Unlocked);
    }
}
