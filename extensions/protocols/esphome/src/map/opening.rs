//! What ESPHome's cover and valve share. Both say where they are as a position, 0.0 closed to
//! 1.0 open (also one that can only open and close), and whether they're moving
//! (`current_operation`: 1 opening, 2 closing). Both are told a position to go to, or to stop.

use irori_protocol::types::{OpenState, OpeningAbilities, OpeningCommand, OpeningState};

/// Where it is, trimmed to what it said it can do.
pub fn state(current_operation: i32, position: f32, known: OpeningAbilities) -> OpeningState {
    OpeningState {
        state: match current_operation {
            1 => OpenState::Opening,
            2 => OpenState::Closing,
            _ if position > 0.0 => OpenState::Open,
            _ => OpenState::Closed,
        },
        position: known.position.then(|| to_percent(position)),
    }
}

/// What a command asks of it, the way ESPHome takes it: open and close are positions 1.0 and
/// 0.0, as Home Assistant sends them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    Position(f32),
    Stop,
}

impl From<OpeningCommand> for Target {
    fn from(command: OpeningCommand) -> Self {
        match command {
            OpeningCommand::Open => Self::Position(1.0),
            OpeningCommand::Close => Self::Position(0.0),
            OpeningCommand::Stop => Self::Stop,
            OpeningCommand::SetPosition(position) => Self::Position(from_percent(position)),
        }
    }
}

/// 0.0-1.0 to 0-100.
pub fn to_percent(fraction: f32) -> u8 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        (fraction.clamp(0.0, 1.0) * 100.0).round() as u8
    }
}

/// 0-100 to 0.0-1.0.
pub fn from_percent(percent: u8) -> f32 {
    f32::from(percent) / 100.0
}
