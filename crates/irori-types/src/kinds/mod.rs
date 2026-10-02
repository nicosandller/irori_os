//! What differs from one entity kind to the next. Each kind has its own file with its
//! capabilities, state, and service data, and everything about them: its checks, its primary
//! value, its toggle, the data its services take. This module only hands each question to the
//! kind, through the tagged enums ([`Capabilities`], [`State`], [`Service`]).
//!
//! Adding a kind: a file here, then a variant in [`EntityKind`], [`Capabilities`], [`State`],
//! [`Service`] and [`ServiceName`]. The compiler lists every `match` that has to answer for it,
//! and each answer is one line calling into the kind's file. A cover and a valve share
//! [`opening`]; a kind like another one shares that way rather than being cast into it.

pub(crate) mod binary_sensor;
pub(crate) mod button;
pub(crate) mod climate;
pub(crate) mod cover;
pub(crate) mod event;
pub(crate) mod fan;
pub(crate) mod humidifier;
pub(crate) mod light;
pub(crate) mod lock;
pub(crate) mod number;
pub(crate) mod opening;
pub(crate) mod select;
pub(crate) mod sensor;
pub(crate) mod siren;
pub(crate) mod switch;
pub(crate) mod text;
pub(crate) mod valve;
pub(crate) mod water_heater;

use crate::{Capabilities, EntityKind, InvariantError, Service, ServiceName, State};

use self::climate::CLIMATE_MODES;
use self::lock::LOCK_STATES;
use self::opening::{OPEN_STATES, OpeningCommand};
use self::sensor::SensorValueType;
use self::water_heater::OPERATION_MODES;

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

/// Whether a service takes `data`, and whether it can do without.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Data {
    /// Refused: `switch.turn_on`.
    None,
    /// Taken, all of it optional: `light.turn_on`.
    Optional,
    /// Has a field that has to be there: a number's `value`.
    Required,
}

/// `data` as the data `T` of service `name`, with a message naming the service when it isn't.
pub(crate) fn parse<T: serde::de::DeserializeOwned>(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<T, InvariantError> {
    serde_json::from_value(serde_json::Value::Object(data))
        .map_err(|e| InvariantError(format!("`{name}` data: {e}")))
}

/// A kind's file was asked to build another kind's service: a slip in this module's dispatch.
pub(crate) fn not_mine(name: ServiceName) -> InvariantError {
    InvariantError(format!("`{name}` isn't a {} service", name.kind()))
}

/// The toggle of a kind whose primary value is on or off.
pub(crate) fn on_off_toggle(
    current: Option<&Typed>,
    turn_on: ServiceName,
    turn_off: ServiceName,
) -> ServiceName {
    if matches!(current, Some(Typed::Bool(true))) {
        turn_off
    } else {
        turn_on
    }
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
        Some(match self {
            Self::Light => light::toggle(current),
            Self::Switch => switch::toggle(current),
            Self::Fan => fan::toggle(current),
            Self::Siren => siren::toggle(current),
            Self::Humidifier => humidifier::toggle(current),
            Self::Climate => climate::toggle(current),
            Self::WaterHeater => water_heater::toggle(current),
            Self::Lock => lock::toggle(current),
            Self::Cover | Self::Valve => OpeningCommand::toggle_service(self, current),
            Self::Sensor
            | Self::BinarySensor
            | Self::Number
            | Self::Select
            | Self::Text
            | Self::Button
            | Self::Event => return None,
        })
    }
}

impl Capabilities {
    /// The shape of this entity's primary value, or `None` when it has none (a button).
    pub fn primary_shape(&self) -> Option<ValueShape> {
        Some(match self {
            Self::Light(_)
            | Self::Switch(_)
            | Self::BinarySensor(_)
            | Self::Fan(_)
            | Self::Siren(_)
            | Self::Humidifier(_) => ValueShape::Bool,
            Self::Number(_) => ValueShape::Number,
            Self::Select(_)
            | Self::Text(_)
            | Self::Event(_)
            | Self::Cover(_)
            | Self::Valve(_)
            | Self::Lock(_)
            | Self::Climate(_)
            | Self::WaterHeater(_) => ValueShape::Text,
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
            Self::Cover(_) | Self::Valve(_) => Some(&OPEN_STATES),
            Self::Lock(_) => Some(&LOCK_STATES),
            Self::Climate(_) => Some(&CLIMATE_MODES),
            Self::WaterHeater(_) => Some(&OPERATION_MODES),
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
            (Self::Valve(caps), State::Valve(state)) => valve::fits(caps, state),
            (Self::Climate(caps), State::Climate(state)) => climate::fits(caps, state),
            (Self::Humidifier(caps), State::Humidifier(state)) => humidifier::fits(caps, state),
            (Self::WaterHeater(caps), State::WaterHeater(state)) => water_heater::fits(caps, state),
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
        match self {
            Self::Light(caps) => light::supports_service(caps, service),
            Self::Number(caps) => number::supports_service(caps, service),
            Self::Select(caps) => select::supports_service(caps, service),
            Self::Text(caps) => text::supports_service(caps, service),
            Self::Cover(caps) => cover::supports_service(caps, service),
            Self::Valve(caps) => valve::supports_service(caps, service),
            Self::Siren(caps) => siren::supports_service(caps, service),
            Self::Lock(caps) => lock::supports_service(caps, service),
            Self::Fan(caps) => fan::supports_service(caps, service),
            Self::Climate(caps) => climate::supports_service(caps, service),
            Self::WaterHeater(caps) => water_heater::supports_service(caps, service),
            Self::Humidifier(caps) => humidifier::supports_service(caps, service),
            Self::Switch(_) | Self::Button(_) => Ok(()),
            Self::Sensor(_) | Self::BinarySensor(_) | Self::Event(_) => Ok(()),
        }
    }
}

impl State {
    /// The value automations compare: `on` for the on/off kinds, the reading for a sensor.
    pub fn primary(&self) -> Typed {
        match self {
            Self::Light(state) => light::primary(state),
            Self::Switch(state) => switch::primary(state),
            Self::BinarySensor(state) => binary_sensor::primary(state),
            Self::Fan(state) => fan::primary(state),
            Self::Siren(state) => siren::primary(state),
            Self::Humidifier(state) => humidifier::primary(state),
            Self::Number(state) => number::primary(state),
            Self::Select(state) => select::primary(state),
            Self::Text(state) => text::primary(state),
            Self::Event(state) => event::primary(state),
            Self::Cover(state) => cover::primary(state),
            Self::Valve(state) => valve::primary(state),
            Self::Lock(state) => lock::primary(state),
            Self::Climate(state) => climate::primary(state),
            Self::WaterHeater(state) => water_heater::primary(state),
            Self::Sensor(state) => sensor::primary(state),
        }
    }

    /// A state of `kind` holding `value`, keeping the rest of `previous` (a light keeps its
    /// brightness). `None` if that kind can't hold that value, or needs more than the value to
    /// be made from nothing (a light needs to have reported once).
    pub fn with_primary(kind: EntityKind, previous: Option<&State>, value: &Typed) -> Option<Self> {
        /// The previous state, when it was one of this kind's.
        macro_rules! previous {
            ($variant:ident) => {
                match previous {
                    Some(State::$variant(state)) => Some(state),
                    _ => None,
                }
            };
        }
        Some(match kind {
            EntityKind::Light => State::Light(light::with_primary(previous!(Light), value)?),
            EntityKind::Switch => State::Switch(switch::with_primary(value)?),
            EntityKind::Sensor => State::Sensor(sensor::with_primary(value)?),
            EntityKind::BinarySensor => State::BinarySensor(binary_sensor::with_primary(value)?),
            EntityKind::Number => State::Number(number::with_primary(value)?),
            EntityKind::Select => State::Select(select::with_primary(value)?),
            EntityKind::Text => State::Text(text::with_primary(value)?),
            EntityKind::Event => State::Event(event::with_primary(value)?),
            EntityKind::Lock => State::Lock(lock::with_primary(value)?),
            EntityKind::Siren => State::Siren(siren::with_primary(value)?),
            EntityKind::Fan => State::Fan(fan::with_primary(previous!(Fan), value)?),
            EntityKind::Cover => State::Cover(cover::with_primary(previous!(Cover), value)?),
            EntityKind::Valve => State::Valve(valve::with_primary(previous!(Valve), value)?),
            EntityKind::Climate => {
                State::Climate(climate::with_primary(previous!(Climate), value)?)
            }
            EntityKind::WaterHeater => {
                State::WaterHeater(water_heater::with_primary(previous!(WaterHeater), value)?)
            }
            EntityKind::Humidifier => {
                State::Humidifier(humidifier::with_primary(previous!(Humidifier), value)?)
            }
            EntityKind::Button => return None,
        })
    }
}

impl Service {
    /// The service `name` with `data`, checked: data a service doesn't take, or data of the
    /// wrong shape, is refused with a message naming the service.
    pub fn from_data(
        name: ServiceName,
        data: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, InvariantError> {
        if !name.takes_data() && !data.is_empty() {
            return Err(InvariantError(format!("`{name}` takes no data")));
        }
        let service = match name.kind() {
            EntityKind::Light => light::service(name, data),
            EntityKind::Switch => switch::service(name, data),
            EntityKind::Number => number::service(name, data),
            EntityKind::Select => select::service(name, data),
            EntityKind::Text => text::service(name, data),
            EntityKind::Button => button::service(name, data),
            EntityKind::Cover => cover::service(name, data),
            EntityKind::Valve => valve::service(name, data),
            EntityKind::Lock => lock::service(name, data),
            EntityKind::Fan => fan::service(name, data),
            EntityKind::Siren => siren::service(name, data),
            EntityKind::Climate => climate::service(name, data),
            EntityKind::WaterHeater => water_heater::service(name, data),
            EntityKind::Humidifier => humidifier::service(name, data),
            EntityKind::Sensor | EntityKind::BinarySensor | EntityKind::Event => {
                Err(not_mine(name))
            }
        }?;
        service.validate()?;
        Ok(service)
    }

    /// Its data as a JSON object, or `None` when there's nothing to send (a `turn_on` with
    /// nothing set).
    pub fn data(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        match serde_json::to_value(self).ok()? {
            serde_json::Value::Object(map) if !map.is_empty() => Some(map),
            _ => None,
        }
    }

    /// Deserialization runs this; call it yourself when building a service in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        match self {
            Self::LightTurnOn(data) => data.validate(),
            Self::NumberSetValue(data) => data.validate(),
            Self::SirenTurnOn(data) => data.validate(),
            Self::ClimateSetTemperature(data) => data.validate(),
            Self::ClimateSetHumidity(data) | Self::HumidifierSetHumidity(data) => data.validate(),
            _ => Ok(()),
        }
    }

    /// The primary value it asks the entity to have, if it asks for one: `turn_on` asks for
    /// `on`. The core remembers it until the device reports, so a `toggle` in between resolves
    /// against what was asked rather than the value that's about to change.
    pub fn asks_for(&self) -> Option<Typed> {
        match self.name().kind() {
            EntityKind::Light => light::asks_for(self),
            EntityKind::Switch => switch::asks_for(self),
            EntityKind::Number => number::asks_for(self),
            EntityKind::Select => select::asks_for(self),
            EntityKind::Text => text::asks_for(self),
            EntityKind::Cover => cover::asks_for(self),
            EntityKind::Valve => valve::asks_for(self),
            EntityKind::Lock => lock::asks_for(self),
            EntityKind::Fan => fan::asks_for(self),
            EntityKind::Siren => siren::asks_for(self),
            EntityKind::Climate => climate::asks_for(self),
            EntityKind::WaterHeater => water_heater::asks_for(self),
            EntityKind::Humidifier => humidifier::asks_for(self),
            // A press leaves nothing for a toggle to go by.
            EntityKind::Button => None,
            EntityKind::Sensor | EntityKind::BinarySensor | EntityKind::Event => None,
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

    fn data(self) -> Data {
        match self.kind() {
            EntityKind::Light => light::data_of(self),
            EntityKind::Switch => switch::data_of(self),
            EntityKind::Number => number::data_of(self),
            EntityKind::Select => select::data_of(self),
            EntityKind::Text => text::data_of(self),
            EntityKind::Button => button::data_of(self),
            EntityKind::Cover => cover::data_of(self),
            EntityKind::Valve => valve::data_of(self),
            EntityKind::Lock => lock::data_of(self),
            EntityKind::Fan => fan::data_of(self),
            EntityKind::Siren => siren::data_of(self),
            EntityKind::Climate => climate::data_of(self),
            EntityKind::WaterHeater => water_heater::data_of(self),
            EntityKind::Humidifier => humidifier::data_of(self),
            EntityKind::Sensor | EntityKind::BinarySensor | EntityKind::Event => Data::None,
        }
    }

    /// Whether it can't do without `data`: its data has a field that has to be there (a
    /// number's `value`), where a light's `turn_on` has none.
    pub fn requires_data(self) -> bool {
        self.data() == Data::Required
    }

    /// Whether it takes `data`.
    pub fn takes_data(self) -> bool {
        self.data() != Data::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LightTurnOn;

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
