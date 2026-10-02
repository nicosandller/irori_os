//! What a cover and a valve share: they open and close, some part of the way, some can be stopped
//! on the way. Each kind keeps its own wire types (a cover also tilts); this is the one model both
//! are read, checked and commanded through, so protocols and pages handle opening once.

use std::sync::LazyLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Typed;
use crate::{EntityKind, InvariantError, Service, ServiceName};

/// Where something that opens and closes is: a cover's or a valve's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpenState {
    Open,
    Opening,
    Closed,
    Closing,
}

impl OpenState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Opening => "opening",
            Self::Closed => "closed",
            Self::Closing => "closing",
        }
    }

    /// Whether it is open, or on its way there: what a toggle closes.
    pub fn is_open_or_opening(self) -> bool {
        matches!(self, Self::Open | Self::Opening)
    }

    /// Read back from [`OpenState::as_str`], e.g. a remembered command.
    pub fn parse(text: &str) -> Option<Self> {
        super::from_ha(text)
    }

    /// Where something at `position` (0 closed, 100 open) has stopped.
    pub fn at(position: u8) -> Self {
        if position == 0 {
            Self::Closed
        } else {
            Self::Open
        }
    }
}

/// Every text a cover's or valve's primary value can be, for rules to check against.
pub(crate) static OPEN_STATES: LazyLock<Vec<String>> = LazyLock::new(|| {
    [
        OpenState::Open,
        OpenState::Opening,
        OpenState::Closed,
        OpenState::Closing,
    ]
    .iter()
    .map(|state| state.as_str().to_owned())
    .collect()
});

/// Data for `cover.set_position` and `valve.set_position`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetPosition {
    /// 0 (closed) to 100 (open).
    #[schemars(range(max = 100))]
    pub position: u8,
}

/// What it can do besides open and close.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpeningAbilities {
    /// Goes to a position between open and closed, and says where it is.
    pub position: bool,
    /// Can be stopped while it moves.
    pub stop: bool,
}

/// Where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpeningState {
    pub state: OpenState,
    /// 0 (closed) to 100 (open), when it can say.
    pub position: Option<u8>,
}

impl OpeningState {
    /// Checked when a state is read: `kind` names it in the message ("a valve's position").
    pub(crate) fn validate(self, kind: EntityKind) -> Result<(), InvariantError> {
        match self.position {
            Some(position) if position > 100 => Err(InvariantError(format!(
                "a {kind}'s position is 0-100, not {position}"
            ))),
            _ => Ok(()),
        }
    }

    /// The same, now in `state`.
    pub fn in_state(self, state: OpenState) -> Self {
        Self { state, ..self }
    }
}

/// What a cover's or a valve's service asks of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpeningCommand {
    Open,
    Close,
    Stop,
    /// 0 (closed) to 100 (open).
    SetPosition(u8),
}

impl OpeningCommand {
    /// What `service` asks of an entity of `kind`, when it's one of that kind's opening services.
    /// `None` for anything else, a cover's tilt included.
    pub fn of(kind: EntityKind, service: &Service) -> Option<Self> {
        let command = match service {
            Service::CoverOpen | Service::ValveOpen => Self::Open,
            Service::CoverClose | Service::ValveClose => Self::Close,
            Service::CoverStop | Service::ValveStop => Self::Stop,
            Service::CoverSetPosition(data) | Service::ValveSetPosition(data) => {
                Self::SetPosition(data.position)
            }
            _ => return None,
        };
        (service.name().kind() == kind).then_some(command)
    }

    /// The service of `kind` that asks for it. `None` unless `kind` opens and closes.
    pub fn service(self, kind: EntityKind) -> Option<Service> {
        let position = |position| SetPosition { position };
        Some(match (kind, self) {
            (EntityKind::Cover, Self::Open) => Service::CoverOpen,
            (EntityKind::Cover, Self::Close) => Service::CoverClose,
            (EntityKind::Cover, Self::Stop) => Service::CoverStop,
            (EntityKind::Cover, Self::SetPosition(p)) => Service::CoverSetPosition(position(p)),
            (EntityKind::Valve, Self::Open) => Service::ValveOpen,
            (EntityKind::Valve, Self::Close) => Service::ValveClose,
            (EntityKind::Valve, Self::Stop) => Service::ValveStop,
            (EntityKind::Valve, Self::SetPosition(p)) => Service::ValveSetPosition(position(p)),
            _ => return None,
        })
    }

    /// Where it asks it to end up; a stop leaves nothing for a toggle to go by.
    pub(crate) fn asks_for(self) -> Option<Typed> {
        let state = match self {
            Self::Open => OpenState::Open,
            Self::Close => OpenState::Closed,
            Self::SetPosition(position) => OpenState::at(position),
            Self::Stop => return None,
        };
        Some(Typed::Text(state.as_str().to_owned()))
    }

    /// What a `toggle` does, from where it is (or was last told to be): closes it when it's
    /// open or opening, opens it otherwise.
    pub(crate) fn toggle(current: Option<&Typed>) -> Self {
        match current {
            Some(Typed::Text(text))
                if OpenState::parse(text).is_some_and(OpenState::is_open_or_opening) =>
            {
                Self::Close
            }
            _ => Self::Open,
        }
    }

    /// The toggle's service for `kind`.
    pub(crate) fn toggle_service(kind: EntityKind, current: Option<&Typed>) -> ServiceName {
        Self::toggle(current)
            .service(kind)
            .expect("only covers and valves toggle by opening")
            .name()
    }
}

/// Whether something with `abilities` can do `command`. `Err` follows the entity's name.
pub(crate) fn supports(abilities: OpeningAbilities, command: OpeningCommand) -> Result<(), String> {
    match command {
        OpeningCommand::Stop if !abilities.stop => Err("can't be stopped while it moves".into()),
        OpeningCommand::SetPosition(_) if !abilities.position => {
            Err("can only open and close, not go part of the way".into())
        }
        OpeningCommand::SetPosition(position) if position > 100 => {
            Err(format!("takes a position from 0 to 100, not {position}"))
        }
        _ => Ok(()),
    }
}

/// Whether a reported state is one something with `abilities` can be in.
pub(crate) fn fits(abilities: OpeningAbilities, opening: OpeningState) -> Result<(), String> {
    if opening.position.is_some() && !abilities.position {
        return Err("it reports a position, but said it can't go part of the way".into());
    }
    Ok(())
}

/// The services `name` takes, of a cover's or a valve's opening ones: only a position takes data.
pub(crate) fn data_of(name: ServiceName) -> super::Data {
    match name {
        ServiceName::CoverSetPosition | ServiceName::ValveSetPosition => super::Data::Required,
        _ => super::Data::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cover_and_a_valve_open_the_same_way() {
        let shutoff = OpeningAbilities::default();
        assert!(supports(shutoff, OpeningCommand::Open).is_ok());
        assert_eq!(
            supports(shutoff, OpeningCommand::Stop),
            Err("can't be stopped while it moves".to_owned())
        );
        assert!(supports(shutoff, OpeningCommand::SetPosition(40)).is_err());
        let zone = OpeningAbilities {
            position: true,
            stop: true,
        };
        assert!(supports(zone, OpeningCommand::SetPosition(40)).is_ok());
        let ajar = OpeningState {
            state: OpenState::Open,
            position: Some(30),
        };
        assert!(fits(shutoff, ajar).is_err());
        assert!(fits(zone, ajar).is_ok());
        assert!(
            OpeningState {
                position: Some(140),
                ..ajar
            }
            .validate(EntityKind::Valve)
            .is_err_and(|e| e.0 == "a valve's position is 0-100, not 140")
        );
    }

    #[test]
    fn a_command_is_the_same_for_both_kinds() {
        for kind in [EntityKind::Cover, EntityKind::Valve] {
            for command in [
                OpeningCommand::Open,
                OpeningCommand::Close,
                OpeningCommand::Stop,
                OpeningCommand::SetPosition(30),
            ] {
                let service = command.service(kind).expect("opens and closes");
                assert_eq!(service.name().kind(), kind);
                assert_eq!(OpeningCommand::of(kind, &service), Some(command));
            }
        }
        // A valve's service isn't a cover's.
        assert_eq!(
            OpeningCommand::of(EntityKind::Cover, &Service::ValveOpen),
            None
        );
        assert_eq!(OpeningCommand::Open.service(EntityKind::Light), None);
        let open = Typed::Text("opening".into());
        assert_eq!(OpeningCommand::toggle(Some(&open)), OpeningCommand::Close);
        assert_eq!(OpeningCommand::toggle(None), OpeningCommand::Open);
        assert_eq!(OPEN_STATES.len(), 4);
    }
}
