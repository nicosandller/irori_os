//! From a SwitchBot device to Irori entities, and from Irori's services back to SwitchBot
//! commands. One module per family of devices; readings every family shares (temperature,
//! battery…) live in `sensors`.
//!
//! An entity's set can depend on what the device says: a Bot in switch mode is a switch, in
//! press mode a button. So [`entities`] takes the reading, and the protocol describes whatever
//! the set is now and removes what dropped out.

mod cover;
mod humidifier;
mod lock;
mod power;
pub(crate) mod sensors;
mod vacuum;

use irori_protocol::ServiceError;
use irori_types::{Capabilities, EntityCategory, Service, State};
use serde_json::Value;

use crate::model::{Family, Reading};

/// One entity of a device: its key (the end of its unique id), its name (none for the device's
/// main feature), what it can do, and whether it's a setting or a diagnostic.
#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub key: &'static str,
    pub name: Option<&'static str>,
    pub capabilities: Capabilities,
    pub category: Option<EntityCategory>,
}

impl Entity {
    pub(crate) fn main(key: &'static str, capabilities: Capabilities) -> Self {
        Self {
            key,
            name: None,
            capabilities,
            category: None,
        }
    }
}

/// A command for SwitchBot's `POST /v1.1/devices/{id}/commands`.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub command: String,
    pub parameter: Value,
}

impl Command {
    /// A command that takes no parameter (SwitchBot's `"default"`).
    pub fn plain(command: &str) -> Self {
        Self {
            command: command.to_owned(),
            parameter: Value::String("default".to_owned()),
        }
    }

    pub fn with(command: &str, parameter: impl Into<Value>) -> Self {
        Self {
            command: command.to_owned(),
            parameter: parameter.into(),
        }
    }
}

/// What the device has, given what's known of it.
pub fn entities(family: Family, reading: &Reading) -> Vec<Entity> {
    match family {
        Family::Hub(_)
        | Family::Meter { .. }
        | Family::Contact
        | Family::Motion
        | Family::Presence
        | Family::Leak => sensors::entities(family),
        Family::Lock { .. } => lock::entities(family),
        Family::Bot | Family::Plug { .. } | Family::Relay2 => power::entities(family, reading),
        Family::Curtain | Family::RollerShade | Family::BlindTilt | Family::Garage => {
            cover::entities(family)
        }
        Family::Humidifier | Family::Evaporative => humidifier::entities(family),
        Family::Vacuum(_) => vacuum::entities(family),
    }
}

/// The state of the entity with `key`, or `None` while nothing is known. A button has none.
pub fn state(family: Family, key: &str, reading: &Reading) -> Option<State> {
    let own = match family {
        Family::Lock { .. } => lock::state(key, reading),
        Family::Bot | Family::Plug { .. } | Family::Relay2 => power::state(key, reading),
        Family::Curtain | Family::RollerShade | Family::BlindTilt | Family::Garage => {
            cover::state(family, key, reading)
        }
        Family::Humidifier | Family::Evaporative => humidifier::state(family, key, reading),
        Family::Vacuum(_) => vacuum::state(family, key, reading),
        _ => None,
    };
    own.or_else(|| sensors::state(key, reading))
}

/// The SwitchBot commands that carry out `service` on the entity with `key`.
pub fn command(
    family: Family,
    key: &str,
    service: &Service,
    reading: &Reading,
) -> Result<Vec<Command>, ServiceError> {
    match family {
        Family::Lock { deadbolt, .. } => lock::command(deadbolt, key, service),
        Family::Bot | Family::Plug { .. } | Family::Relay2 => power::command(family, key, service),
        Family::Curtain | Family::RollerShade | Family::BlindTilt | Family::Garage => {
            cover::command(family, key, service)
        }
        Family::Humidifier | Family::Evaporative => {
            humidifier::command(family, key, service, reading)
        }
        Family::Vacuum(api) => vacuum::command(api, key, service),
        _ => Err(unsupported()),
    }
}

pub(crate) fn unsupported() -> ServiceError {
    ServiceError::failed("SwitchBot can't do that on this device")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HubSensors, Meter, VacuumApi};
    use irori_types::{EntityDescription, Name, UniqueId};

    /// Every entity of every family is one the core accepts: built the way the protocol builds
    /// it, checked the way the wire checks it, and of a kind the manifest lists (the core refuses
    /// any other).
    #[test]
    fn every_family_describes_valid_entities_with_distinct_keys() {
        let manifest = irori_protocol::parse_manifest(include_str!("../../irori-extension.toml"))
            .expect("a valid manifest");
        let kinds = &manifest.contributes.protocol[0].entity_kinds;
        let pressing = entities(Family::Bot, &Reading::default());
        for entity in &pressing {
            assert!(kinds.contains(&entity.capabilities.kind()), "{entity:?}");
        }
        let full = Reading {
            bot_mode: Some("switchMode".into()),
            ..Reading::default()
        };
        for family in [
            Family::Hub(HubSensors::None),
            Family::Hub(HubSensors::Hub2),
            Family::Hub(HubSensors::Hub3),
            Family::Meter { co2: true },
            Family::Contact,
            Family::Motion,
            Family::Presence,
            Family::Leak,
            Family::Lock {
                door: true,
                deadbolt: true,
            },
            Family::Bot,
            Family::Plug {
                meter: Meter::Full,
                outlet: false,
            },
            Family::Plug {
                meter: Meter::Mini,
                outlet: true,
            },
            Family::Relay2,
            Family::Curtain,
            Family::RollerShade,
            Family::BlindTilt,
            Family::Garage,
            Family::Humidifier,
            Family::Evaporative,
            Family::Vacuum(VacuumApi::Mop),
        ] {
            let mut keys = Vec::new();
            for entity in entities(family, &full) {
                assert!(
                    !keys.contains(&entity.key),
                    "{family:?} has {} twice",
                    entity.key
                );
                keys.push(entity.key);
                let described = EntityDescription {
                    unique_id: UniqueId::try_from(format!("AABBCCDDEEFF:{}", entity.key))
                        .expect("valid"),
                    name: entity.name.map(|name| Name::try_from(name).expect("valid")),
                    device_unique_id: Some(UniqueId::try_from("AABBCCDDEEFF").expect("valid")),
                    suggested_object_id: None,
                    capabilities: entity.capabilities,
                    entity_category: entity.category,
                };
                assert!(
                    kinds.contains(&described.kind()),
                    "{family:?} {} is a {}, which the manifest doesn't list",
                    entity.key,
                    described.kind()
                );
                let json = serde_json::to_value(&described).expect("json");
                serde_json::from_value::<EntityDescription>(json)
                    .unwrap_or_else(|error| panic!("{family:?} {}: {error}", entity.key));
            }
            assert_eq!(
                keys.iter().filter(|key| **key == "main").count(),
                usize::from(!matches!(
                    family,
                    Family::Hub(_)
                        | Family::Meter { .. }
                        | Family::Contact
                        | Family::Motion
                        | Family::Presence
                        | Family::Leak
                        | Family::Relay2
                        | Family::Bot
                )),
                "{family:?}: one main feature, unless it's a sensor, has channels, or is a Bot"
            );
        }
    }
}
