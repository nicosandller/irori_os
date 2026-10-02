//! One module per Home Assistant MQTT component Irori reads. Each has its topics, how its
//! discovery config becomes those topics and its capabilities (`parse`), how a message on them
//! becomes a state (`decode`), and how a service becomes messages (`encode`).
//! [`crate::discovery`] and [`crate::state`] only hand each entity to its module.
//!
//! What several share lives in its own module: [`setting`] (a setting, a reading, a power
//! switch) and [`opening`] (what a cover and a valve both do).

pub mod binary_sensor;
pub mod button;
pub mod climate;
pub mod cover;
pub mod event;
pub mod fan;
pub mod humidifier;
pub mod light;
pub mod lock;
pub mod number;
pub mod opening;
pub mod select;
pub mod sensor;
pub mod setting;
pub mod siren;
pub mod switch;
pub mod text;
pub mod valve;
pub mod water_heater;

use irori_types::Service;

/// The error for a service of the right kind that this component has no way to send.
pub(crate) fn no_service(what: &str, service: &Service) -> String {
    format!("{what} has no `{}` service", service.name())
}
