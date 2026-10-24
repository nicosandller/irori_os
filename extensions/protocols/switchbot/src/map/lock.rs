//! The Smart Lock family: the bolt, the door it's on, and whether it's calibrated.

use irori_protocol::ServiceError;
use irori_types::{
    BinarySensorClass, Capabilities, LockCapabilities, LockState, LockStatus, Service, State,
};

use super::{Command, Entity, sensors, unsupported};
use crate::model::{Bolt, Family, Reading};

pub fn entities(family: Family) -> Vec<Entity> {
    let Family::Lock { door, deadbolt } = family else {
        return vec![];
    };
    let mut list = vec![Entity::main(
        "main",
        Capabilities::Lock(LockCapabilities {
            // "Open" pulls the latch (SwitchBot's `deadbolt` command).
            open: deadbolt,
            requires_code: false,
            code_format: None,
        }),
    )];
    if door {
        list.push(sensors::binary("door", "Door", BinarySensorClass::Door));
    }
    list.push(sensors::calibration());
    list.push(sensors::battery());
    list
}

pub fn state(key: &str, reading: &Reading) -> Option<State> {
    match key {
        "main" => reading.bolt.map(|bolt| {
            State::Lock(LockState {
                state: match bolt {
                    Bolt::Locked => LockStatus::Locked,
                    Bolt::Unlocked => LockStatus::Unlocked,
                    Bolt::Locking => LockStatus::Locking,
                    Bolt::Unlocking => LockStatus::Unlocking,
                    Bolt::Jammed => LockStatus::Jammed,
                },
            })
        }),
        _ => None,
    }
}

pub fn command(deadbolt: bool, key: &str, service: &Service) -> Result<Vec<Command>, ServiceError> {
    if key != "main" {
        return Err(unsupported());
    }
    Ok(vec![match service {
        Service::LockLock(_) => Command::plain("lock"),
        Service::LockUnlock(_) => Command::plain("unlock"),
        Service::LockOpen(_) if deadbolt => Command::plain("deadbolt"),
        _ => return Err(unsupported()),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::LockCode;

    #[test]
    fn open_pulls_the_latch_only_where_the_lock_can() {
        assert_eq!(
            command(true, "main", &Service::LockOpen(LockCode::default())).expect("supported"),
            [Command::plain("deadbolt")]
        );
        assert!(command(false, "main", &Service::LockOpen(LockCode::default())).is_err());
        assert_eq!(
            command(false, "main", &Service::LockUnlock(LockCode::default())).expect("supported"),
            [Command::plain("unlock")]
        );
    }

    #[test]
    fn a_lock_lite_has_no_door() {
        let lite = Family::of("Lock Lite").expect("known");
        assert!(entities(lite).iter().all(|entity| entity.key != "door"));
        let pro = Family::of("Smart Lock Pro").expect("known");
        assert!(entities(pro).iter().any(|entity| entity.key == "door"));
    }
}
