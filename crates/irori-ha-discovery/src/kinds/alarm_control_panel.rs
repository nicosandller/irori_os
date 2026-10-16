//! Home Assistant's MQTT alarm control panel: a payload for each way of arming, for disarming
//! and for setting it off, and Home Assistant's own words for its states.
//!
//! **Its code.** The config's `code` says who checks it:
//!
//! - none: the panel takes no code.
//! - `REMOTE_CODE` (digits) or `REMOTE_CODE_TEXT`: the device checks it, and the code travels
//!   in the command through `command_template`'s `{{ code }}`.
//! - anything else is the code itself, which Home Assistant checks before sending anything.
//!   Irori does the same, and never sends it.

use irori_types::{
    AlarmCapabilities, AlarmState, AlarmStatus, ArmMode, Capabilities, CodeFormat, LockCode,
    Service, State,
};

use crate::discovery::{EntityTopics, owned_str, str_field, string_list};
use crate::state::{Message, Publish, decode_text, text_publish};
use crate::template::ValueTemplate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlarmTopics {
    pub state_topic: String,
    pub value_template: ValueTemplate,
    /// `None` for a panel that is only watched.
    pub command_topic: Option<String>,
    pub command_template: ActionTemplate,
    /// What to send to arm each way, in [`ArmMode::ALL`]'s order.
    pub payload_arm: [String; 5],
    pub payload_disarm: String,
    pub payload_trigger: String,
    /// The code to check before sending, when it's the config's to check and not the device's.
    pub code: Option<Secret>,
}

/// A panel's code as its config gave it. Its `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<hidden>")
    }
}

/// A panel's `command_template`: fixed text with `{{ action }}` and `{{ code }}` put into it.
/// Home Assistant's default is `{{ action }}` alone. Anything more is Jinja, which Irori
/// doesn't run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionTemplate(Vec<Piece>);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Text(String),
    Action,
    Code,
}

impl ActionTemplate {
    fn parse(template: Option<&str>) -> Result<Self, String> {
        let Some(template) = template else {
            return Ok(Self(vec![Piece::Action]));
        };
        let refuse = || {
            format!(
                "its template {template:?} is more than the action and the code, which Irori \
                 can't render"
            )
        };
        if template.contains("{%") {
            return Err(refuse());
        }
        let mut pieces = Vec::new();
        let mut rest = template;
        while let Some((before, after)) = rest.split_once("{{") {
            if !before.is_empty() {
                pieces.push(Piece::Text(before.to_owned()));
            }
            let (inside, after) = after.split_once("}}").ok_or_else(refuse)?;
            pieces.push(match inside.trim() {
                "action" => Piece::Action,
                "code" => Piece::Code,
                _ => return Err(refuse()),
            });
            rest = after;
        }
        if !rest.is_empty() {
            pieces.push(Piece::Text(rest.to_owned()));
        }
        Ok(Self(pieces))
    }

    fn takes_code(&self) -> bool {
        self.0.contains(&Piece::Code)
    }

    fn render(&self, action: &str, code: &str) -> String {
        self.0
            .iter()
            .map(|piece| match piece {
                Piece::Text(text) => text.as_str(),
                Piece::Action => action,
                Piece::Code => code,
            })
            .collect()
    }
}

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let state_topic = str_field(root, "state_topic")
        .ok_or("an alarm panel needs a `state_topic`")?
        .to_owned();
    let command_topic = str_field(root, "command_topic").map(str::to_owned);
    let command_template = ActionTemplate::parse(str_field(root, "command_template"))
        .map_err(|why| format!("`command_template`: {why}"))?;
    let word = |key: &str, default: &str| owned_str(root, key, default);
    // Home Assistant's default is everything; a panel lists what it has when that's less.
    let features = string_list(root, "supported_features");
    let has = |feature: &str| {
        command_topic.is_some()
            && features
                .as_ref()
                .is_none_or(|list| list.iter().any(|f| f == feature))
    };
    let arm_modes = ArmMode::ALL
        .into_iter()
        .filter(|mode| has(&format!("arm_{}", mode_word(*mode))))
        .collect();
    let (code_format, code) = match str_field(root, "code") {
        None => (None, None),
        Some("REMOTE_CODE") => (Some(CodeFormat::Number), None),
        Some("REMOTE_CODE_TEXT") => (Some(CodeFormat::Text), None),
        Some(code) => (
            Some(if code.chars().all(|c| c.is_ascii_digit()) {
                CodeFormat::Number
            } else {
                CodeFormat::Text
            }),
            Some(Secret(code.to_owned())),
        ),
    };
    if code_format.is_some() && code.is_none() && !command_template.takes_code() {
        return Err(
            "its code is the device's to check, but its `command_template` never sends one"
                .to_owned(),
        );
    }
    let flag = |key: &str| {
        root.get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true)
    };
    let capabilities = AlarmCapabilities {
        arm_modes,
        trigger: has("trigger"),
        code_format,
        code_arm_required: flag("code_arm_required"),
        code_disarm_required: flag("code_disarm_required"),
    };
    let topics = AlarmTopics {
        state_topic,
        value_template: ValueTemplate::parse(str_field(root, "value_template")),
        command_topic,
        command_template,
        payload_arm: [
            word("payload_arm_home", "ARM_HOME"),
            word("payload_arm_away", "ARM_AWAY"),
            word("payload_arm_night", "ARM_NIGHT"),
            word("payload_arm_vacation", "ARM_VACATION"),
            word("payload_arm_custom_bypass", "ARM_CUSTOM_BYPASS"),
        ],
        payload_disarm: word("payload_disarm", "DISARM"),
        payload_trigger: word("payload_trigger", "TRIGGER"),
        code,
    };
    Ok((
        Capabilities::AlarmControlPanel(capabilities),
        EntityTopics::AlarmControlPanel(Box::new(topics)),
    ))
}

/// A mode as Home Assistant's feature list and Irori both spell it: `custom_bypass`.
fn mode_word(mode: ArmMode) -> &'static str {
    match mode {
        ArmMode::Home => "home",
        ArmMode::Away => "away",
        ArmMode::Night => "night",
        ArmMode::Vacation => "vacation",
        ArmMode::CustomBypass => "custom_bypass",
    }
}

pub(crate) fn decode(panel: &AlarmTopics, message: Message) -> Option<Result<State, String>> {
    message.on(Some(&panel.state_topic)).then(|| {
        decode_text(message.payload, &panel.value_template).and_then(|said| {
            AlarmStatus::parse(said.trim())
                .map(|state| State::AlarmControlPanel(AlarmState { state }))
                .ok_or_else(|| format!("{said:?} isn't a state an alarm panel says"))
        })
    })
}

pub(crate) fn encode(panel: &AlarmTopics, service: &Service) -> Result<Vec<Publish>, String> {
    let (payload, given, arming): (&String, &LockCode, Option<bool>) = match service {
        Service::AlarmArmHome(code) => (&panel.payload_arm[0], code, Some(true)),
        Service::AlarmArmAway(code) => (&panel.payload_arm[1], code, Some(true)),
        Service::AlarmArmNight(code) => (&panel.payload_arm[2], code, Some(true)),
        Service::AlarmArmVacation(code) => (&panel.payload_arm[3], code, Some(true)),
        Service::AlarmArmCustomBypass(code) => (&panel.payload_arm[4], code, Some(true)),
        Service::AlarmDisarm(code) => (&panel.payload_disarm, code, Some(false)),
        Service::AlarmTrigger(code) => (&panel.payload_trigger, code, None),
        service => return Err(super::no_service("an alarm panel", service)),
    };
    let topic = panel
        .command_topic
        .as_ref()
        .ok_or("this panel is only watched: it has nowhere to send a command")?;
    let given = given.code.as_deref().unwrap_or_default();
    // A code that's the config's to check is checked here, where one was given: the core has
    // already refused a call that needed one and had none. It is never sent.
    if let Some(Secret(code)) = &panel.code
        && arming.is_some()
        && !given.is_empty()
        && given != code
    {
        return Err("that isn't its code".to_owned());
    }
    let sent = if panel.code.is_some() { "" } else { given };
    Ok(vec![text_publish(
        topic,
        &panel.command_template.render(payload, sent),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::topic::Component;

    fn code(text: &str) -> LockCode {
        LockCode {
            code: Some(text.to_owned()),
        }
    }

    #[test]
    fn a_panel_with_home_assistants_defaults_arms_every_way_and_says_its_state() {
        let payload = br#"{"unique_id": "house_alarm", "name": "House alarm",
            "state_topic": "alarm/state", "command_topic": "alarm/set"}"#;
        let parsed = parse(Component::AlarmControlPanel, payload).expect("valid");
        assert_eq!(
            parsed.capabilities,
            Capabilities::AlarmControlPanel(AlarmCapabilities {
                arm_modes: ArmMode::ALL.to_vec(),
                trigger: true,
                code_format: None,
                code_arm_required: true,
                code_disarm_required: true,
            })
        );
        assert_eq!(
            crate::state::decode(&parsed.topics, "alarm/state", b"armed_night", None),
            Some(Ok(State::AlarmControlPanel(AlarmState {
                state: AlarmStatus::ArmedNight
            })))
        );
        assert!(
            crate::state::decode(&parsed.topics, "alarm/state", b"asleep", None)
                .expect("its topic")
                .is_err()
        );
        let sent =
            crate::state::encode(&parsed.topics, &Service::AlarmArmAway(LockCode::default()))
                .expect("arms");
        assert_eq!(
            (sent[0].topic.as_str(), sent[0].payload.as_slice()),
            ("alarm/set", &b"ARM_AWAY"[..])
        );
    }

    #[test]
    fn a_code_the_device_checks_travels_in_the_command() {
        let payload = br#"{"unique_id": "house_alarm", "name": "House alarm",
            "state_topic": "alarm/state", "command_topic": "alarm/set",
            "supported_features": ["arm_home", "arm_away"],
            "code": "REMOTE_CODE", "code_arm_required": false,
            "command_template": "{\"action\": \"{{ action }}\", \"code\": \"{{ code }}\"}"}"#;
        let parsed = parse(Component::AlarmControlPanel, payload).expect("valid");
        let Capabilities::AlarmControlPanel(caps) = &parsed.capabilities else {
            panic!("a panel");
        };
        assert_eq!(caps.arm_modes, [ArmMode::Home, ArmMode::Away]);
        assert!(!caps.trigger);
        assert_eq!(caps.code_format, Some(CodeFormat::Number));
        assert!(!caps.code_arm_required && caps.code_disarm_required);
        let sent = crate::state::encode(&parsed.topics, &Service::AlarmDisarm(code("1234")))
            .expect("disarms");
        assert_eq!(
            String::from_utf8_lossy(&sent[0].payload),
            r#"{"action": "DISARM", "code": "1234"}"#
        );

        // Its to check, and no way to send it: said when it's described, not at the door.
        let no_way = br#"{"unique_id": "x", "name": "X", "state_topic": "s",
            "command_topic": "c", "code": "REMOTE_CODE"}"#;
        let error = parse(Component::AlarmControlPanel, no_way).expect_err("refused");
        assert!(error.contains("never sends one"), "{error}");
    }

    #[test]
    fn a_code_the_config_holds_is_checked_here_and_never_sent() {
        let payload = br#"{"unique_id": "house_alarm", "name": "House alarm",
            "state_topic": "alarm/state", "command_topic": "alarm/set", "code": "4321"}"#;
        let parsed = parse(Component::AlarmControlPanel, payload).expect("valid");
        let Capabilities::AlarmControlPanel(caps) = &parsed.capabilities else {
            panic!("a panel");
        };
        assert_eq!(caps.code_format, Some(CodeFormat::Number));
        assert_eq!(
            crate::state::encode(&parsed.topics, &Service::AlarmDisarm(code("1111"))),
            Err("that isn't its code".to_owned())
        );
        let sent = crate::state::encode(&parsed.topics, &Service::AlarmDisarm(code("4321")))
            .expect("disarms");
        assert_eq!(sent[0].payload, b"DISARM");
        assert!(
            !format!("{:?}", parsed.topics).contains("4321"),
            "kept out of logs"
        );
    }

    #[test]
    fn a_panel_with_no_command_topic_is_only_watched() {
        let payload = br#"{"unique_id": "house_alarm", "name": "House alarm",
            "state_topic": "alarm/state"}"#;
        let parsed = parse(Component::AlarmControlPanel, payload).expect("valid");
        let Capabilities::AlarmControlPanel(caps) = &parsed.capabilities else {
            panic!("a panel");
        };
        assert!(caps.arm_modes.is_empty() && !caps.trigger);
        assert!(
            crate::state::encode(&parsed.topics, &Service::AlarmDisarm(LockCode::default()))
                .is_err()
        );
    }
}
