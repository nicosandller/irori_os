//! Home Assistant's MQTT lock: lock, unlock and sometimes open, each with its own payload, and a
//! word for each state.

use irori_types::{Capabilities, LockCapabilities, LockState, LockStatus, Service, State};

use crate::discovery::{EntityTopics, owned_str, plain_command, str_field};
use crate::state::{Message, Publish, decode_text, text_publish};
use crate::template::{CommandTemplate, ValueTemplate};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockTopics {
    pub command_topic: String,
    pub command_template: CommandTemplate,
    pub payload_lock: String,
    pub payload_unlock: String,
    /// `None` when it can't open the door.
    pub payload_open: Option<String>,
    pub state_topic: Option<String>,
    pub value_template: ValueTemplate,
    /// What it says for each state, in Irori's order: locked, unlocked, locking, unlocking,
    /// jammed, open, opening.
    pub said: [(String, LockStatus); 7],
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let (command_topic, command_template) = plain_command(root, "a lock")?;
    let word = |key: &str, default: &str| owned_str(root, key, default);
    let payload_open = str_field(root, "payload_open").map(str::to_owned);
    let topics = LockTopics {
        command_topic,
        command_template,
        payload_lock: word("payload_lock", "LOCK"),
        payload_unlock: word("payload_unlock", "UNLOCK"),
        payload_open: payload_open.clone(),
        state_topic: str_field(root, "state_topic").map(str::to_owned),
        value_template: ValueTemplate::parse(str_field(root, "value_template")),
        said: [
            (word("state_locked", "LOCKED"), LockStatus::Locked),
            (word("state_unlocked", "UNLOCKED"), LockStatus::Unlocked),
            (word("state_locking", "LOCKING"), LockStatus::Locking),
            (word("state_unlocking", "UNLOCKING"), LockStatus::Unlocking),
            (word("state_jammed", "JAMMED"), LockStatus::Jammed),
            (word("state_open", "OPEN"), LockStatus::Open),
            (word("state_opening", "OPENING"), LockStatus::Opening),
        ],
    };
    // Home Assistant checks an MQTT lock's `code_format` itself and never sends the code to the
    // device. Irori has no regular expressions to check it with, so it's kept for pages to show
    // and not demanded; the page asks before unlocking either way.
    let capabilities = LockCapabilities {
        open: payload_open.is_some(),
        requires_code: false,
        code_format: str_field(root, "code_format").map(str::to_owned),
    };
    Ok((
        Capabilities::Lock(capabilities),
        EntityTopics::Lock(Box::new(topics)),
    ))
}

pub(crate) fn decode(lock: &LockTopics, message: Message) -> Option<Result<State, String>> {
    message.on(lock.state_topic.as_deref()).then(|| {
        decode_text(message.payload, &lock.value_template).and_then(|said| {
            lock.said
                .iter()
                .find(|(word, _)| *word == said.trim())
                .map(|(_, state)| State::Lock(LockState { state: *state }))
                .ok_or_else(|| format!("{said:?} isn't a state this lock says"))
        })
    })
}

pub(crate) fn encode(lock: &LockTopics, service: &Service) -> Result<Vec<Publish>, String> {
    let payload = match service {
        Service::LockLock(_) => &lock.payload_lock,
        Service::LockUnlock(_) => &lock.payload_unlock,
        Service::LockOpen(_) => lock
            .payload_open
            .as_ref()
            .ok_or("this lock can't open the door")?,
        service => return Err(super::no_service("a lock", service)),
    };
    Ok(vec![text_publish(
        &lock.command_topic,
        &lock.command_template.render(payload),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;

    #[test]
    fn a_z2m_lock_says_lock_and_unlock() {
        // Zigbee2MQTT's `case "lock"`: its own words for the two states.
        let payload = br#"{"unique_id": "0x02_lock", "name": null,
            "device": {"identifiers": ["zigbee2mqtt_0x02"], "name": "Front door"},
            "command_topic": "zigbee2mqtt/Front door/set", "state_topic": "zigbee2mqtt/Front door",
            "value_template": "{{ value_json[\"state\"] }}",
            "state_locked": "LOCK", "state_unlocked": "UNLOCK"}"#;
        let parsed = parse(Component::Lock, payload).expect("valid");
        assert_eq!(
            parsed.capabilities,
            Capabilities::Lock(LockCapabilities::default()),
            "no open, no code"
        );
        assert_eq!(
            crate::state::decode(
                &parsed.topics,
                "zigbee2mqtt/Front door",
                br#"{"state": "UNLOCK", "battery": 80}"#,
                None
            ),
            Some(Ok(irori_types::State::Lock(irori_types::LockState {
                state: LockStatus::Unlocked
            })))
        );
        let sent = crate::state::encode(
            &parsed.topics,
            &irori_types::Service::LockLock(irori_types::LockCode::default()),
        )
        .expect("lock");
        assert_eq!(sent[0].topic, "zigbee2mqtt/Front door/set");
        assert_eq!(sent[0].payload, b"LOCK");
        assert!(
            crate::state::encode(
                &parsed.topics,
                &irori_types::Service::LockOpen(irori_types::LockCode::default())
            )
            .is_err()
        );
    }
}
