//! Home Assistant's MQTT lawn mower: a topic saying what it's doing, and a topic for each of
//! the three things it can be told, each sent that thing's name.

use irori_types::{
    Capabilities, LawnMowerCapabilities, LawnMowerState, MowerActivity, Service, State,
};

use crate::discovery::{EntityTopics, str_field};
use crate::state::{Message, Publish, decode_text, text_publish};
use crate::template::{CommandTemplate, ValueTemplate};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LawnMowerTopics {
    pub activity_state_topic: Option<String>,
    pub activity_value_template: ValueTemplate,
    /// Where each command goes, with its template: start mowing, pause, dock. `None` for one
    /// it can't be told.
    pub start_mowing: Option<(String, CommandTemplate)>,
    pub pause: Option<(String, CommandTemplate)>,
    pub dock: Option<(String, CommandTemplate)>,
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let command = |what: &str| -> Result<Option<(String, CommandTemplate)>, String> {
        let Some(topic) = str_field(root, &format!("{what}_command_topic")) else {
            return Ok(None);
        };
        let key = format!("{what}_command_template");
        let template = CommandTemplate::parse(str_field(root, &key), "value")
            .map_err(|why| format!("`{key}`: {why}"))?;
        Ok(Some((topic.to_owned(), template)))
    };
    let topics = LawnMowerTopics {
        activity_state_topic: str_field(root, "activity_state_topic").map(str::to_owned),
        activity_value_template: ValueTemplate::parse(str_field(root, "activity_value_template")),
        start_mowing: command("start_mowing")?,
        pause: command("pause")?,
        dock: command("dock")?,
    };
    let capabilities = LawnMowerCapabilities {
        start_mowing: topics.start_mowing.is_some(),
        pause: topics.pause.is_some(),
        dock: topics.dock.is_some(),
    };
    Ok((
        Capabilities::LawnMower(capabilities),
        EntityTopics::LawnMower(Box::new(topics)),
    ))
}

pub(crate) fn decode(mower: &LawnMowerTopics, message: Message) -> Option<Result<State, String>> {
    message.on(mower.activity_state_topic.as_deref()).then(|| {
        decode_text(message.payload, &mower.activity_value_template).and_then(|said| {
            MowerActivity::parse(said.trim())
                .map(|activity| State::LawnMower(LawnMowerState { activity }))
                .ok_or_else(|| format!("{said:?} isn't something a mower does"))
        })
    })
}

pub(crate) fn encode(mower: &LawnMowerTopics, service: &Service) -> Result<Vec<Publish>, String> {
    // Home Assistant sends each command its own name.
    let (command, payload) = match service {
        Service::LawnMowerStartMowing => (&mower.start_mowing, "start_mowing"),
        Service::LawnMowerPause => (&mower.pause, "pause"),
        Service::LawnMowerDock => (&mower.dock, "dock"),
        service => return Err(super::no_service("a lawn mower", service)),
    };
    let (topic, template) = command
        .as_ref()
        .ok_or_else(|| format!("this mower has nowhere to send `{payload}`"))?;
    Ok(vec![text_publish(topic, &template.render(payload))])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;

    #[test]
    fn a_mower_says_what_it_does_and_is_told_on_a_topic_each() {
        let payload = br#"{"unique_id": "mower", "name": "Mower",
            "activity_state_topic": "mower/state",
            "activity_value_template": "{{ value_json.activity }}",
            "start_mowing_command_topic": "mower/start", "dock_command_topic": "mower/dock",
            "dock_command_template": "{\"do\": \"{{ value }}\"}"}"#;
        let parsed = parse(Component::LawnMower, payload).expect("valid");
        assert_eq!(
            parsed.capabilities,
            Capabilities::LawnMower(LawnMowerCapabilities {
                start_mowing: true,
                pause: false,
                dock: true,
            })
        );
        assert_eq!(
            crate::state::decode(
                &parsed.topics,
                "mower/state",
                br#"{"activity": "mowing"}"#,
                None
            ),
            Some(Ok(State::LawnMower(LawnMowerState {
                activity: MowerActivity::Mowing
            })))
        );
        let sent =
            crate::state::encode(&parsed.topics, &Service::LawnMowerStartMowing).expect("starts");
        assert_eq!(
            (sent[0].topic.as_str(), sent[0].payload.as_slice()),
            ("mower/start", &b"start_mowing"[..])
        );
        let sent = crate::state::encode(&parsed.topics, &Service::LawnMowerDock).expect("docks");
        assert_eq!(
            String::from_utf8_lossy(&sent[0].payload),
            r#"{"do": "dock"}"#
        );
        assert!(crate::state::encode(&parsed.topics, &Service::LawnMowerPause).is_err());
    }
}
