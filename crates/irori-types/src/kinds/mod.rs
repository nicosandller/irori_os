//! What differs from one entity kind to the next. Each kind has its own file with its
//! capabilities, state, and service data, and the checks that go with them. This module is where
//! the tagged enums ([`Capabilities`], [`State`], [`Service`]) hand each question to the kind.
//!
//! Adding a kind: a file here, then a variant in [`EntityKind`], [`Capabilities`], [`State`],
//! [`Service`] and [`ServiceName`]. The compiler lists every `match` that has to answer for it.

pub(crate) mod binary_sensor;
pub(crate) mod button;
pub(crate) mod cover;
pub(crate) mod event;
pub(crate) mod fan;
pub(crate) mod light;
pub(crate) mod lock;
pub(crate) mod number;
pub(crate) mod select;
pub(crate) mod sensor;
pub(crate) mod switch;
pub(crate) mod text;

use crate::{Capabilities, EntityKind, InvariantError, Service, ServiceName, State};

use self::cover::{CoverState, OPEN_STATES, OpenState, SetPosition, SetTilt};
use self::event::EventState;
use self::fan::{FanState, FanTurnOn};
use self::light::LightTurnOn;
use self::lock::{LOCK_STATES, LockCode, LockState, LockStatus};
use self::number::{NumberSetValue, NumberState};
use self::select::{SelectOption, SelectState};
use self::sensor::{SensorState, SensorValue, SensorValueType};
use self::text::{TextSetValue, TextState};

/// Reads a device class (or another name-only enum) by its Home Assistant name, which is also
/// Irori's spelling. `None` for a name Irori doesn't have, which a protocol leaves absent rather
/// than guessing.
pub(crate) fn from_ha<T: serde::de::DeserializeOwned>(name: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(name.to_owned())).ok()
}

/// An entity's value the way automations see it: what `on()`, `num()` and `text()` read, and
/// what a state trigger's `to` is compared with (`docs/specs/rules.md` §5.1). Each kind has one,
/// its *primary* value: a light's `on`, a sensor's reading.
#[derive(Debug, Clone, PartialEq)]
pub enum Typed {
    Bool(bool),
    Number(f64),
    Text(String),
}

impl Typed {
    pub fn shape(&self) -> ValueShape {
        match self {
            Self::Bool(_) => ValueShape::Bool,
            Self::Number(_) => ValueShape::Number,
            Self::Text(_) => ValueShape::Text,
        }
    }

    /// As plain JSON: `true`, `21.5`, `"rinse"`.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Bool(on) => serde_json::Value::Bool(*on),
            Self::Number(n) => serde_json::json!(n),
            Self::Text(text) => serde_json::Value::String(text.clone()),
        }
    }
}

/// Which of the three a [`Typed`] value is, known from the registry before any value arrives,
/// so rules can be checked when they're saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueShape {
    Bool,
    Number,
    Text,
}

impl EntityKind {
    /// Whether every report is something happening, even one identical to the last: two
    /// `double` presses of a remote are two presses (`docs/specs/entities.md` §5.3).
    pub fn counts_every_report(self) -> bool {
        matches!(self, Self::Event)
    }

    /// The standard services for this kind (`docs/specs/protocols.md` §7.1).
    pub fn services(self) -> impl Iterator<Item = ServiceName> {
        ServiceName::ALL
            .iter()
            .copied()
            .filter(move |name| name.kind() == self)
    }

    /// Whether anything can be asked of it: a light or switch can, a sensor only reports.
    pub fn has_services(self) -> bool {
        self.services().next().is_some()
    }

    /// The service a `toggle` on this kind resolves to, from the value it has (or was last told
    /// to have). `None` for kinds that can't be toggled.
    pub fn toggle(self, current: Option<&Typed>) -> Option<ServiceName> {
        let on = matches!(current, Some(Typed::Bool(true)));
        match self {
            Self::Light if on => Some(ServiceName::LightTurnOff),
            Self::Light => Some(ServiceName::LightTurnOn),
            Self::Switch if on => Some(ServiceName::SwitchTurnOff),
            Self::Switch => Some(ServiceName::SwitchTurnOn),
            Self::Fan if on => Some(ServiceName::FanTurnOff),
            Self::Fan => Some(ServiceName::FanTurnOn),
            Self::Sensor
            | Self::BinarySensor
            | Self::Number
            | Self::Select
            | Self::Text
            | Self::Button
            | Self::Event => None,
            Self::Cover => match current.and_then(|current| match current {
                Typed::Text(text) => OpenState::parse(text),
                _ => None,
            }) {
                Some(state) if state.is_open_or_opening() => Some(ServiceName::CoverClose),
                _ => Some(ServiceName::CoverOpen),
            },
            Self::Lock => match current {
                Some(Typed::Text(text))
                    if matches!(
                        LockStatus::parse(text),
                        Some(LockStatus::Locked | LockStatus::Locking)
                    ) =>
                {
                    Some(ServiceName::LockUnlock)
                }
                _ => Some(ServiceName::LockLock),
            },
        }
    }
}

impl Capabilities {
    /// The shape of this entity's primary value, or `None` when it has none (a button).
    pub fn primary_shape(&self) -> Option<ValueShape> {
        Some(match self {
            Self::Light(_) | Self::Switch(_) | Self::BinarySensor(_) | Self::Fan(_) => {
                ValueShape::Bool
            }
            Self::Number(_) => ValueShape::Number,
            Self::Select(_) | Self::Text(_) | Self::Event(_) | Self::Cover(_) | Self::Lock(_) => {
                ValueShape::Text
            }
            Self::Sensor(sensor) => match sensor.value_type {
                SensorValueType::Number => ValueShape::Number,
                SensorValueType::Text => ValueShape::Text,
            },
            Self::Button(_) => return None,
        })
    }

    /// Every text its primary value can be, when that's a fixed list. Rules check the text
    /// they compare against it, and the editor offers it.
    pub fn text_options(&self) -> Option<&[String]> {
        match self {
            Self::Sensor(sensor) if !sensor.options.is_empty() => Some(&sensor.options),
            Self::Select(select) => Some(&select.options),
            Self::Event(event) => Some(&event.event_types),
            Self::Cover(_) => Some(&OPEN_STATES),
            Self::Lock(_) => Some(&LOCK_STATES),
            _ => None,
        }
    }

    /// Whether a reported state is one this entity can be in. `Err` says why not, as the end
    /// of a sentence about the report ("it isn't dimmable").
    pub fn fits(&self, state: &State) -> Result<(), String> {
        match (self, state) {
            (caps, state) if caps.kind() != state.kind() => Err(format!(
                "it's a {}, but the report is for a {}",
                caps.kind(),
                state.kind()
            )),
            (Self::Light(caps), State::Light(light)) => light::fits(caps, light),
            (Self::Sensor(caps), State::Sensor(sensor)) => sensor::fits(caps, sensor),
            (Self::Number(caps), State::Number(state)) => number::fits(caps, state),
            (Self::Select(caps), State::Select(state)) => select::fits(caps, state),
            (Self::Text(caps), State::Text(state)) => text::fits(caps, state),
            (Self::Event(caps), State::Event(state)) => event::fits(caps, state),
            (Self::Cover(caps), State::Cover(state)) => cover::fits(caps, state),
            (Self::Fan(caps), State::Fan(state)) => fan::fits(caps, state),
            _ => Ok(()),
        }
    }

    /// Whether this entity can do what a service asks. `Err` says what it can't, after the
    /// entity's name ("isn't dimmable").
    pub fn supports(&self, service: &Service) -> Result<(), String> {
        let name = service.name();
        if name.kind() != self.kind() {
            return Err(format!(
                "is a {}; `{name}` is for a {}",
                self.kind(),
                name.kind()
            ));
        }
        match (self, service) {
            (Self::Light(caps), Service::LightTurnOn(data)) => light::supports(caps, data),
            (Self::Number(caps), Service::NumberSetValue(data)) => number::supports(caps, data),
            (Self::Select(caps), Service::SelectSelectOption(data)) => select::supports(caps, data),
            (Self::Text(caps), Service::TextSetValue(data)) => text::supports(caps, data),
            (Self::Cover(caps), Service::CoverSetPosition(data)) => {
                cover::supports_position(caps, data)
            }
            (Self::Cover(caps), Service::CoverSetTilt(data)) => cover::supports_tilt(caps, data),
            (Self::Cover(caps), Service::CoverStop) => cover::supports_stop(caps),
            (Self::Lock(caps), Service::LockLock(data) | Service::LockUnlock(data)) => {
                lock::supports(caps, data, false)
            }
            (Self::Lock(caps), Service::LockOpen(data)) => lock::supports(caps, data, true),
            (Self::Fan(caps), Service::FanTurnOn(data)) => fan::supports_turn_on(caps, data),
            (Self::Fan(caps), Service::FanSetPercentage(data)) => {
                fan::supports_percentage(caps, data.percentage)
            }
            (Self::Fan(caps), Service::FanOscillate(_)) => fan::supports_oscillate(caps),
            (Self::Fan(caps), Service::FanSetDirection(_)) => fan::supports_direction(caps),
            (Self::Fan(caps), Service::FanSetPresetMode(data)) => {
                fan::supports_preset(caps, &data.preset_mode)
            }
            _ => Ok(()),
        }
    }
}

impl State {
    /// The value automations compare: `on` for the on/off kinds, the reading for a sensor.
    pub fn primary(&self) -> Typed {
        match self {
            Self::Light(light) => Typed::Bool(light.on),
            Self::Switch(switch) => Typed::Bool(switch.on),
            Self::BinarySensor(sensor) => Typed::Bool(sensor.on),
            Self::Fan(fan) => Typed::Bool(fan.on),
            Self::Number(number) => Typed::Number(number.value),
            Self::Select(select) => Typed::Text(select.option.clone()),
            Self::Text(text) => Typed::Text(text.value.clone()),
            Self::Event(event) => Typed::Text(event.event_type.clone()),
            Self::Cover(cover) => Typed::Text(cover.state.as_str().to_owned()),
            Self::Lock(lock) => Typed::Text(lock.state.as_str().to_owned()),
            Self::Sensor(sensor) => match &sensor.value {
                SensorValue::Number(n) => Typed::Number(*n),
                SensorValue::Text(text) => Typed::Text(text.clone()),
            },
        }
    }

    /// A state of `kind` holding `value`, keeping the rest of `previous` (a light keeps its
    /// brightness). `None` if that kind can't hold that value, or needs more than the value to
    /// be made from nothing (a light needs to have reported once).
    pub fn with_primary(kind: EntityKind, previous: Option<&State>, value: &Typed) -> Option<Self> {
        Some(match (kind, previous, value) {
            (EntityKind::Light, Some(State::Light(light)), Typed::Bool(on)) => {
                let mut light = light.clone();
                light.on = *on;
                State::Light(light)
            }
            (EntityKind::Switch, _, Typed::Bool(on)) => {
                State::Switch(switch::SwitchState { on: *on })
            }
            (EntityKind::BinarySensor, _, Typed::Bool(on)) => {
                State::BinarySensor(binary_sensor::BinarySensorState { on: *on })
            }
            (EntityKind::Sensor, _, Typed::Number(n)) => State::Sensor(SensorState {
                value: SensorValue::Number(*n),
            }),
            (EntityKind::Number, _, Typed::Number(value)) => {
                State::Number(NumberState { value: *value })
            }
            (EntityKind::Select, _, Typed::Text(option)) => State::Select(SelectState {
                option: option.clone(),
            }),
            (EntityKind::Text, _, Typed::Text(value)) => State::Text(TextState {
                value: value.clone(),
            }),
            (EntityKind::Event, _, Typed::Text(event_type)) => State::Event(EventState {
                event_type: event_type.clone(),
            }),
            (EntityKind::Fan, previous, Typed::Bool(on)) => match previous {
                Some(State::Fan(fan)) => State::Fan(FanState {
                    on: *on,
                    ..fan.clone()
                }),
                _ => State::Fan(FanState {
                    on: *on,
                    percentage: None,
                    oscillating: None,
                    direction: None,
                    preset_mode: None,
                }),
            },
            (EntityKind::Lock, _, Typed::Text(text)) => State::Lock(LockState {
                state: LockStatus::parse(text)?,
            }),
            (EntityKind::Cover, previous, Typed::Text(text)) => {
                let state = OpenState::parse(text)?;
                match previous {
                    Some(State::Cover(cover)) => State::Cover(CoverState {
                        state,
                        ..cover.clone()
                    }),
                    _ => State::Cover(CoverState {
                        state,
                        position: None,
                        tilt: None,
                    }),
                }
            }
            (EntityKind::Sensor, _, Typed::Text(text)) => State::Sensor(SensorState {
                value: SensorValue::Text(text.clone()),
            }),
            _ => return None,
        })
    }
}

/// `data` as the data `T` of service `name`, with a message naming the service when it isn't.
fn parse<T: serde::de::DeserializeOwned>(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<T, InvariantError> {
    serde_json::from_value(serde_json::Value::Object(data))
        .map_err(|e| InvariantError(format!("`{name}` data: {e}")))
}

impl Service {
    /// The service `name` with `data`, checked: data a service doesn't take, or data of the
    /// wrong shape, is refused with a message naming the service.
    pub fn from_data(
        name: ServiceName,
        data: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, InvariantError> {
        use serde::Deserialize as _;
        if !name.takes_data() && !data.is_empty() {
            return Err(InvariantError(format!("`{name}` takes no data")));
        }
        let service = match name {
            ServiceName::LightTurnOn => Service::LightTurnOn(
                LightTurnOn::deserialize(serde_json::Value::Object(data))
                    .map_err(|e| InvariantError(format!("`{name}` data: {e}")))?,
            ),
            ServiceName::LightTurnOff => Service::LightTurnOff,
            ServiceName::SwitchTurnOn => Service::SwitchTurnOn,
            ServiceName::SwitchTurnOff => Service::SwitchTurnOff,
            ServiceName::ButtonPress => Service::ButtonPress,
            ServiceName::LockLock | ServiceName::LockUnlock | ServiceName::LockOpen => {
                let code = LockCode::deserialize(serde_json::Value::Object(data))
                    .map_err(|e| InvariantError(format!("`{name}` data: {e}")))?;
                match name {
                    ServiceName::LockLock => Service::LockLock(code),
                    ServiceName::LockUnlock => Service::LockUnlock(code),
                    _ => Service::LockOpen(code),
                }
            }
            ServiceName::FanTurnOff => Service::FanTurnOff,
            ServiceName::FanTurnOn => Service::FanTurnOn(parse(name, data)?),
            ServiceName::FanSetPercentage => Service::FanSetPercentage(parse(name, data)?),
            ServiceName::FanOscillate => Service::FanOscillate(parse(name, data)?),
            ServiceName::FanSetDirection => Service::FanSetDirection(parse(name, data)?),
            ServiceName::FanSetPresetMode => Service::FanSetPresetMode(parse(name, data)?),
            ServiceName::CoverOpen => Service::CoverOpen,
            ServiceName::CoverClose => Service::CoverClose,
            ServiceName::CoverStop => Service::CoverStop,
            ServiceName::CoverSetPosition => Service::CoverSetPosition(
                SetPosition::deserialize(serde_json::Value::Object(data))
                    .map_err(|e| InvariantError(format!("`{name}` data: {e}")))?,
            ),
            ServiceName::CoverSetTilt => Service::CoverSetTilt(
                SetTilt::deserialize(serde_json::Value::Object(data))
                    .map_err(|e| InvariantError(format!("`{name}` data: {e}")))?,
            ),
            ServiceName::NumberSetValue => Service::NumberSetValue(
                NumberSetValue::deserialize(serde_json::Value::Object(data))
                    .map_err(|e| InvariantError(format!("`{name}` data: {e}")))?,
            ),
            ServiceName::SelectSelectOption => Service::SelectSelectOption(
                SelectOption::deserialize(serde_json::Value::Object(data))
                    .map_err(|e| InvariantError(format!("`{name}` data: {e}")))?,
            ),
            ServiceName::TextSetValue => Service::TextSetValue(
                TextSetValue::deserialize(serde_json::Value::Object(data))
                    .map_err(|e| InvariantError(format!("`{name}` data: {e}")))?,
            ),
        };
        service.validate()?;
        Ok(service)
    }

    /// Its data as a JSON object, or `None` when there's nothing to send.
    pub fn data(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        let value = match self {
            Self::LightTurnOn(data) if *data != LightTurnOn::default() => {
                serde_json::to_value(data).ok()?
            }
            Self::NumberSetValue(data) => serde_json::to_value(data).ok()?,
            Self::SelectSelectOption(data) => serde_json::to_value(data).ok()?,
            Self::TextSetValue(data) => serde_json::to_value(data).ok()?,
            Self::CoverSetPosition(data) => serde_json::to_value(data).ok()?,
            Self::CoverSetTilt(data) => serde_json::to_value(data).ok()?,
            Self::FanTurnOn(data) if *data != FanTurnOn::default() => {
                serde_json::to_value(data).ok()?
            }
            Self::FanSetPercentage(data) => serde_json::to_value(data).ok()?,
            Self::FanOscillate(data) => serde_json::to_value(data).ok()?,
            Self::FanSetDirection(data) => serde_json::to_value(data).ok()?,
            Self::FanSetPresetMode(data) => serde_json::to_value(data).ok()?,
            Self::LockLock(code) | Self::LockUnlock(code) | Self::LockOpen(code)
                if code.code.is_some() =>
            {
                serde_json::to_value(code).ok()?
            }
            _ => return None,
        };
        match value {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        }
    }

    /// Deserialization runs this; call it yourself when building a service in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        match self {
            Self::LightTurnOn(data) => data.validate(),
            Self::NumberSetValue(data) => data.validate(),
            _ => Ok(()),
        }
    }

    /// The primary value it asks the entity to have, if it asks for one: `turn_on` asks for
    /// `on`. The core remembers it until the device reports, so a `toggle` in between resolves
    /// against what was asked rather than the value that's about to change.
    pub fn asks_for(&self) -> Option<Typed> {
        match self {
            Self::LightTurnOn(_) | Self::SwitchTurnOn => Some(Typed::Bool(true)),
            Self::LightTurnOff | Self::SwitchTurnOff => Some(Typed::Bool(false)),
            Self::NumberSetValue(data) => Some(Typed::Number(data.value)),
            Self::SelectSelectOption(data) => Some(Typed::Text(data.option.clone())),
            Self::TextSetValue(data) => Some(Typed::Text(data.value.clone())),
            Self::FanTurnOn(_) => Some(Typed::Bool(true)),
            Self::FanTurnOff => Some(Typed::Bool(false)),
            Self::FanSetPercentage(data) => Some(Typed::Bool(data.percentage > 0)),
            Self::FanOscillate(_) | Self::FanSetDirection(_) | Self::FanSetPresetMode(_) => None,
            Self::LockLock(_) => Some(Typed::Text(LockStatus::Locked.as_str().to_owned())),
            Self::LockUnlock(_) => Some(Typed::Text(LockStatus::Unlocked.as_str().to_owned())),
            Self::LockOpen(_) => Some(Typed::Text(LockStatus::Open.as_str().to_owned())),
            Self::CoverOpen => Some(Typed::Text(OpenState::Open.as_str().to_owned())),
            Self::CoverClose => Some(Typed::Text(OpenState::Closed.as_str().to_owned())),
            Self::CoverSetPosition(data) => Some(Typed::Text(
                if data.position == 0 {
                    OpenState::Closed
                } else {
                    OpenState::Open
                }
                .as_str()
                .to_owned(),
            )),
            // A press, a stop or a tilt leaves nothing for a toggle to go by.
            Self::ButtonPress | Self::CoverStop | Self::CoverSetTilt(_) => None,
        }
    }
}

impl ServiceName {
    /// The service of `kind` called `action`: `(Light, "turn_on")` is `light.turn_on`.
    pub fn of(kind: EntityKind, action: &str) -> Option<Self> {
        kind.services().find(|name| name.action() == action)
    }

    /// The part after the dot: `turn_on`.
    pub fn action(self) -> &'static str {
        let name = self.as_str();
        name.split_once('.').map_or(name, |(_, action)| action)
    }

    /// Whether it can't do without `data`: its data has a field that has to be there (a
    /// number's `value`), where a light's `turn_on` has none.
    pub fn requires_data(self) -> bool {
        matches!(
            self,
            Self::NumberSetValue
                | Self::SelectSelectOption
                | Self::TextSetValue
                | Self::CoverSetPosition
                | Self::CoverSetTilt
                | Self::FanSetPercentage
                | Self::FanOscillate
                | Self::FanSetDirection
                | Self::FanSetPresetMode
        )
    }

    /// Whether it takes `data`.
    pub fn takes_data(self) -> bool {
        matches!(
            self,
            Self::LightTurnOn
                | Self::NumberSetValue
                | Self::SelectSelectOption
                | Self::TextSetValue
                | Self::CoverSetPosition
                | Self::CoverSetTilt
                | Self::LockLock
                | Self::LockUnlock
                | Self::LockOpen
                | Self::FanTurnOn
                | Self::FanSetPercentage
                | Self::FanOscillate
                | Self::FanSetDirection
                | Self::FanSetPresetMode
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_service_is_found_by_its_kind_and_action() {
        for name in ServiceName::ALL {
            assert_eq!(ServiceName::of(name.kind(), name.action()), Some(*name));
        }
        assert_eq!(ServiceName::of(EntityKind::Sensor, "turn_on"), None);
        assert!(!EntityKind::BinarySensor.has_services());
    }

    /// The schema requires `data` exactly where Rust can't do without it.
    #[test]
    fn a_service_requires_data_only_when_it_has_a_field_that_must_be_there() {
        for name in ServiceName::ALL {
            let without = Service::from_data(*name, serde_json::Map::new());
            assert_eq!(without.is_err(), name.requires_data(), "{name}");
        }
    }

    #[test]
    fn data_a_service_does_not_take_is_refused() {
        let mut data = serde_json::Map::new();
        data.insert("brightness".into(), serde_json::json!(5));
        let refused = Service::from_data(ServiceName::SwitchTurnOn, data.clone());
        assert!(refused.is_err_and(|e| e.to_string() == "`switch.turn_on` takes no data"));
        let light = Service::from_data(ServiceName::LightTurnOn, data).expect("valid");
        assert_eq!(
            light.data().and_then(|d| d.get("brightness").cloned()),
            Some(5.into())
        );
        assert_eq!(Service::LightTurnOn(LightTurnOn::default()).data(), None);
    }

    #[test]
    fn a_kinds_primary_value_round_trips_through_with_primary() {
        for (kind, value) in [
            (EntityKind::Switch, Typed::Bool(true)),
            (EntityKind::BinarySensor, Typed::Bool(false)),
            (EntityKind::Sensor, Typed::Number(21.5)),
            (EntityKind::Sensor, Typed::Text("rinse".into())),
        ] {
            let state = State::with_primary(kind, None, &value).expect("holds it");
            assert_eq!(state.primary(), value);
        }
        // A light keeps everything but `on`, so it has to have reported once.
        assert_eq!(
            State::with_primary(EntityKind::Light, None, &Typed::Bool(true)),
            None
        );
        assert_eq!(
            State::with_primary(EntityKind::Switch, None, &Typed::Number(1.0)),
            None
        );
    }
}
