//! A lock: Door Lock (0x0101). Its commands need a timed interaction, and a PIN when the lock
//! says remote operation needs one. Following Home Assistant's `lock.py`.

use base64::Engine as _;
use serde_json::json;

use crate::model::{Command, Node, cluster};
use irori_types::{LockCapabilities, LockCode, LockState, LockStatus, Service, State};

/// Door Lock's feature bits.
mod feature {
    /// PIN credentials.
    pub const PIN: u64 = 1;
    /// Can pull the latch (`unboltDoor`), not only unlock.
    pub const UNBOLT: u64 = 0x1000;
}

const LOCK_STATE: u32 = 0x0000;
const MAX_PIN_LENGTH: u32 = 0x0017;
const MIN_PIN_LENGTH: u32 = 0x0018;
const REQUIRE_PIN_FOR_REMOTE: u32 = 0x0033;

pub(super) fn capabilities(node: &Node, endpoint: u16) -> LockCapabilities {
    let features = node.features(endpoint, cluster::DOOR_LOCK);
    let requires_code = features & feature::PIN != 0
        && node
            .value(endpoint, cluster::DOOR_LOCK, REQUIRE_PIN_FOR_REMOTE)
            .and_then(serde_json::Value::as_bool)
            == Some(true);
    let length = |attribute| node.number(endpoint, cluster::DOOR_LOCK, attribute);
    let code_format = match (length(MIN_PIN_LENGTH), length(MAX_PIN_LENGTH)) {
        (Some(min), Some(max)) if requires_code && min <= max => {
            Some(format!("^\\d{{{min},{max}}}$"))
        }
        _ => None,
    };
    LockCapabilities {
        open: features & feature::UNBOLT != 0,
        requires_code,
        code_format,
    }
}

/// `LockState`: 1 locked, 2 unlocked, 3 unlatched (open). Not fully locked (0) is a bolt that
/// didn't go all the way: unlocked, as Home Assistant reads it.
pub(super) fn state(node: &Node, endpoint: u16) -> Option<State> {
    let state = match node
        .value(endpoint, cluster::DOOR_LOCK, LOCK_STATE)?
        .as_u64()?
    {
        1 => LockStatus::Locked,
        0 | 2 => LockStatus::Unlocked,
        3 => LockStatus::Open,
        _ => return None,
    };
    Some(State::Lock(LockState { state }))
}

pub(super) fn command(endpoint: u16, service: &Service) -> Result<Vec<Command>, String> {
    let (name, code) = match service {
        Service::LockLock(code) => ("lockDoor", code),
        Service::LockUnlock(code) => ("unlockDoor", code),
        Service::LockOpen(code) => ("unboltDoor", code),
        other => return Err(format!("a lock doesn't take {}", other.name())),
    };
    Ok(vec![Command::Invoke {
        endpoint,
        cluster: cluster::DOOR_LOCK,
        name,
        payload: payload(code),
        timed: true,
    }])
}

/// The PIN, base64 as the server takes an octet string, when one was given.
fn payload(code: &LockCode) -> serde_json::Value {
    match code.code.as_deref().filter(|code| !code.is_empty()) {
        Some(code) => json!({
            "pinCode": base64::engine::general_purpose::STANDARD.encode(code.as_bytes()),
        }),
        None => json!({}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pin_goes_as_base64_and_never_unasked() {
        let commands = command(
            1,
            &Service::LockUnlock(LockCode {
                code: Some("1234".to_owned()),
            }),
        )
        .expect("a lock unlocks");
        assert_eq!(
            commands,
            vec![Command::Invoke {
                endpoint: 1,
                cluster: cluster::DOOR_LOCK,
                name: "unlockDoor",
                payload: json!({"pinCode": "MTIzNA=="}),
                timed: true,
            }]
        );
        let Command::Invoke { payload, .. } =
            &command(1, &Service::LockLock(LockCode::default())).expect("a lock locks")[0]
        else {
            panic!("an invoke");
        };
        assert_eq!(payload, &json!({}));
    }
}
