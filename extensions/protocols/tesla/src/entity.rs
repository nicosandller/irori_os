//! What a car's or a site's entities are made of: a key (the end of the entity's unique id), a
//! name, what it can do, and whether it's a setting or a diagnostic. Cars and sites each say
//! which they have; this is the one place their parts are built.
//!
//! Keys are part of the unique id (`{vin}:{key}`, `{site}:{key}`), so they never change once
//! released: renaming one would leave the old entity behind in every home.

use irori_types::{
    BinarySensorCapabilities, BinarySensorClass, BinarySensorState, ButtonCapabilities,
    Capabilities, EntityCategory, NumberCapabilities, NumberMode, NumberState, SelectCapabilities,
    SelectState, SensorCapabilities, SensorClass, SensorState, SensorValue, SensorValueType, State,
    StateClass, SwitchCapabilities, SwitchState, UniqueId,
};

#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub key: &'static str,
    pub name: String,
    pub capabilities: Capabilities,
    pub category: Option<EntityCategory>,
}

impl Entity {
    pub fn new(key: &'static str, name: impl Into<String>, capabilities: Capabilities) -> Self {
        Self {
            key,
            name: name.into(),
            capabilities,
            category: None,
        }
    }

    pub fn diagnostic(self) -> Self {
        Self {
            category: Some(EntityCategory::Diagnostic),
            ..self
        }
    }

    pub fn config(self) -> Self {
        Self {
            category: Some(EntityCategory::Config),
            ..self
        }
    }
}

/// The unique id of a car's or site's entity.
pub fn id(device: &str, key: &str) -> Option<UniqueId> {
    UniqueId::try_from(format!("{device}:{key}")).ok()
}

/// A sensor that measures something.
pub fn measured(
    key: &'static str,
    name: &'static str,
    class: Option<SensorClass>,
    unit: &str,
    state_class: StateClass,
) -> Entity {
    Entity::new(
        key,
        name,
        Capabilities::Sensor(SensorCapabilities {
            value_type: SensorValueType::Number,
            device_class: class,
            unit: Some(unit.to_owned()),
            state_class: Some(state_class),
            options: Vec::new(),
        }),
    )
}

/// A sensor whose text is one of `options`.
pub fn listed(key: &'static str, name: &'static str, options: &[&str]) -> Entity {
    Entity::new(
        key,
        name,
        Capabilities::Sensor(SensorCapabilities {
            value_type: SensorValueType::Text,
            device_class: None,
            unit: None,
            state_class: None,
            options: options.iter().map(|option| (*option).to_owned()).collect(),
        }),
    )
}

pub fn binary(key: &'static str, name: &'static str, class: BinarySensorClass) -> Entity {
    Entity::new(
        key,
        name,
        Capabilities::BinarySensor(BinarySensorCapabilities {
            device_class: Some(class),
        }),
    )
}

pub fn switch(key: &'static str, name: &'static str) -> Entity {
    Entity::new(
        key,
        name,
        Capabilities::Switch(SwitchCapabilities { device_class: None }),
    )
}

pub fn button(key: &'static str, name: impl Into<String>) -> Entity {
    Entity::new(
        key,
        name,
        Capabilities::Button(ButtonCapabilities { device_class: None }),
    )
}

pub fn select(key: &'static str, name: &'static str, options: &[&str]) -> Entity {
    Entity::new(
        key,
        name,
        Capabilities::Select(SelectCapabilities {
            options: options.iter().map(|option| (*option).to_owned()).collect(),
        }),
    )
}

pub fn number(
    key: &'static str,
    name: &'static str,
    (min, max, step): (f64, f64, f64),
    unit: &str,
    mode: NumberMode,
) -> Entity {
    Entity::new(
        key,
        name,
        Capabilities::Number(NumberCapabilities {
            min,
            max,
            step,
            unit: Some(unit.to_owned()),
            device_class: None,
            mode,
        }),
    )
}

pub fn reading(value: Option<f64>) -> Option<State> {
    value.filter(|value| value.is_finite()).map(|value| {
        State::Sensor(SensorState {
            value: SensorValue::Number(value),
        })
    })
}

pub fn text(value: Option<&str>) -> Option<State> {
    value.map(|value| {
        State::Sensor(SensorState {
            value: SensorValue::Text(value.to_owned()),
        })
    })
}

pub fn on(value: Option<bool>) -> Option<State> {
    value.map(|on| State::BinarySensor(BinarySensorState { on }))
}

pub fn switched(value: Option<bool>) -> Option<State> {
    value.map(|on| State::Switch(SwitchState { on }))
}

pub fn chosen(value: Option<&str>) -> Option<State> {
    value.map(|option| {
        State::Select(SelectState {
            option: option.to_owned(),
        })
    })
}

pub fn set_to(value: Option<f64>) -> Option<State> {
    value
        .filter(|value| value.is_finite())
        .map(|value| State::Number(NumberState { value }))
}
