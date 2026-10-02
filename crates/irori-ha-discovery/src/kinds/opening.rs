//! What Home Assistant's MQTT cover and valve share: told to open, close, stop or go to a
//! position, and saying where they are on a state topic, a position topic, or both. A cover adds
//! its tilt ([`super::cover`]); a valve is only this ([`super::valve`]).

use irori_types::{EntityKind, OpenState, OpeningCommand, OpeningState};

use crate::state::{Message, Publish, text_publish};
use crate::template::{CommandTemplate, ValueTemplate};

/// How it's told what to do and says where it is, all as Home Assistant lets it vary. A state
/// can come in on any of its topics, which are often the same one.
#[derive(Debug, Clone, PartialEq)]
pub struct OpeningTopics {
    pub command_topic: Option<String>,
    pub payload_open: String,
    pub payload_close: String,
    /// `None` when it can't be stopped.
    pub payload_stop: Option<String>,
    pub state_topic: Option<String>,
    /// `None` when its template needs Jinja: the state is then worked out from the position.
    pub value_template: Option<ValueTemplate>,
    pub state_open: String,
    pub state_opening: String,
    pub state_closed: String,
    pub state_closing: String,
    /// Stopped somewhere: open, unless the position says it's at the closed end.
    pub state_stopped: String,
    pub position_topic: Option<String>,
    pub position_template: ValueTemplate,
    /// The device's own numbers for fully open and fully closed; Irori's are 100 and 0.
    pub position_open: f64,
    pub position_closed: f64,
    pub set_position_topic: Option<String>,
    pub set_position_template: CommandTemplate,
}

impl OpeningTopics {
    pub(crate) fn listens(&self) -> Vec<&str> {
        [&self.state_topic, &self.position_topic]
            .into_iter()
            .filter_map(Option::as_deref)
            .collect()
    }

    /// Where it is, from a message on any of its topics (or one of `kind`'s own, such as a
    /// cover's tilt), merged with where it last said: the position and the state often arrive on
    /// different topics, or in one body.
    pub(crate) fn read(
        &self,
        kind: EntityKind,
        message: Message,
        previous: Option<OpeningState>,
    ) -> Result<OpeningState, String> {
        let for_state = message.on(self.state_topic.as_deref());
        let for_position = message.on(self.position_topic.as_deref());
        let mut position = previous.and_then(|old| old.position);
        if for_position && let Some(raw) = number(message, &self.position_template)? {
            position = percent(raw, self.position_closed, self.position_open);
        }
        let from_position = || OpenState::at(position.unwrap_or(100));
        let said = match (&self.value_template, for_state) {
            (Some(template), true) => match template.extract(message.payload)? {
                serde_json::Value::String(text) => Some(text.trim().to_owned()),
                serde_json::Value::Null => None,
                other => Some(other.to_string()),
            },
            _ => None,
        };
        let state = match said {
            Some(text) if text == self.state_open => OpenState::Open,
            Some(text) if text == self.state_opening => OpenState::Opening,
            Some(text) if text == self.state_closed => OpenState::Closed,
            Some(text) if text == self.state_closing => OpenState::Closing,
            Some(text) if text == self.state_stopped => from_position(),
            Some(text) => return Err(format!("{text:?} isn't a state this {kind} says")),
            // Nothing said about where it is: its position says, else what it said last.
            None => match (position, previous) {
                (Some(_), _) => from_position(),
                (None, Some(old)) => old.state,
                (None, None) => return Err(format!("the {kind} hasn't said where it is")),
            },
        };
        Ok(OpeningState { state, position })
    }

    /// The message that does `command`. `kind` names it in an error ("this valve can't be
    /// stopped").
    pub(crate) fn encode(
        &self,
        kind: EntityKind,
        command: OpeningCommand,
    ) -> Result<Vec<Publish>, String> {
        let send = |payload: &str| match &self.command_topic {
            Some(topic) => Ok(vec![text_publish(topic, payload)]),
            None => Err(format!("this {kind} takes no commands")),
        };
        match command {
            OpeningCommand::Open => send(&self.payload_open),
            OpeningCommand::Close => send(&self.payload_close),
            OpeningCommand::Stop => match &self.payload_stop {
                Some(stop) => send(stop),
                None => Err(format!("this {kind} can't be stopped")),
            },
            OpeningCommand::SetPosition(position) => match &self.set_position_topic {
                Some(topic) => Ok(vec![text_publish(
                    topic,
                    &self.set_position_template.render(&scale(
                        position,
                        self.position_closed,
                        self.position_open,
                    )),
                )]),
                None => Err(format!("this {kind} can't go to a position")),
            },
        }
    }
}

/// A number a message says through `template`, if it says one.
pub(crate) fn number(message: Message, template: &ValueTemplate) -> Result<Option<f64>, String> {
    Ok(match template.extract(message.payload)? {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    })
}

/// A device's `raw`, on its own scale from `closed` to `open`, as 0-100.
pub(crate) fn percent(raw: f64, closed: f64, open: f64) -> Option<u8> {
    let span = open - closed;
    if span == 0.0 {
        return None;
    }
    let share = ((raw - closed) / span * 100.0).round().clamp(0.0, 100.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(share as u8)
}

/// 0-100 on a device's own scale from `closed` to `open`, as it's written.
pub(crate) fn scale(percent: u8, closed: f64, open: f64) -> String {
    let raw = closed + f64::from(percent) / 100.0 * (open - closed);
    let raw = raw.round().to_string();
    raw.strip_suffix(".0").map_or(raw.clone(), str::to_owned)
}
