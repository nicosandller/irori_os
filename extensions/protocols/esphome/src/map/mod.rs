//! Turning what ESPHome says into what Irori's model holds, one module per entity type: what it
//! is (`describe`), what it reports (`state`), and what it's told (`command`). What several share
//! lives here, and in [`opening`] for covers and valves.
//!
//! ESPHome identifies an entity by a `key`: a hash its firmware computes from the entity's
//! object id, stable across reboots. The protocol's own `unique_id` field was removed upstream,
//! so identity here is the device's MAC address plus that key — stable as long as the entity
//! keeps its name, which is the same promise ESPHome makes to Home Assistant.

pub mod binary_sensor;
pub mod button;
pub mod climate;
pub mod cover;
pub mod event;
pub mod fan;
pub mod light;
pub mod lock;
pub mod number;
pub mod opening;
pub mod select;
pub mod sensor;
pub mod siren;
pub mod switch;
pub mod text;
pub mod valve;
pub mod water_heater;

use esphome_client::types::{DeviceInfoResponse, EspHomeMessage};
use irori_protocol::ProtocolError;
use irori_protocol::types::units::{TemperatureUnit, round_to};
use irori_protocol::types::{
    DeviceDescription, EntityCategory, Name, ObjectId, UniqueId, Unmodeled,
};

/// How the device is identified to Irori: its MAC address, or its node name when a device
/// doesn't report one (the `host` platform on a machine without a MAC, for instance).
pub fn device_id(info: &DeviceInfoResponse) -> Result<UniqueId, ProtocolError> {
    let id = if info.mac_address.is_empty() {
        &info.name
    } else {
        &info.mac_address
    };
    if id.is_empty() {
        return Err(ProtocolError::new(
            "the device reported neither a MAC address nor a name, so there's nothing stable to \
             identify it by",
        ));
    }
    Ok(UniqueId::try_from(id.as_str())?)
}

/// An entity's id: the device, the kind, and ESPHome's key for it.
///
/// The kind is in there because ESPHome's key is a hash of the object id alone. Change a
/// component from a sensor to a switch while keeping its name and the key is unchanged — and an
/// entity is not allowed to change kind (`docs/specs/entities.md` §4), so the core would refuse
/// the new one and the entity would vanish. With the kind in the id they are simply two
/// different entities, and the old one is removed as any disappeared entity is.
pub fn entity_id(
    device: &UniqueId,
    kind: impl std::fmt::Display,
    key: u32,
) -> Result<UniqueId, ProtocolError> {
    Ok(UniqueId::try_from(format!("{device}-{kind}-{key}"))?)
}

pub fn device(info: &DeviceInfoResponse) -> Result<DeviceDescription, ProtocolError> {
    let name = first_non_empty(&[&info.friendly_name, &info.name])
        .ok_or_else(|| ProtocolError::new("the device reported no name"))?;
    Ok(DeviceDescription {
        unique_id: device_id(info)?,
        name: Name::try_from(name)?,
        manufacturer: optional(&info.manufacturer),
        model: optional(&info.model),
        // What's running on the device, which is what a person wants when something misbehaves.
        sw_version: optional(&info.esphome_version),
        hw_version: None,
        // An area name the device suggests; ignored if it isn't a name Irori would accept.
        suggested_area: optional(&info.suggested_area).and_then(|a| Name::try_from(a).ok()),
        via_device_unique_id: None,
    })
}

/// The value as written: ESPHome sends 32-bit floats, and `0.1_f32` widened is
/// `0.10000000149011612`. Its shortest decimal form is the number the device was configured with.
pub fn decimal(value: f32) -> f64 {
    value.to_string().parse().unwrap_or(f64::from(value))
}

/// The unit a climate entity or water heater speaks, from ESPHome's `TemperatureUnit`. Anything
/// unknown is °C, as Home Assistant reads it.
pub fn temperature_unit(unit: i32) -> TemperatureUnit {
    match unit {
        1 => TemperatureUnit::Fahrenheit,
        2 => TemperatureUnit::Kelvin,
        _ => TemperatureUnit::Celsius,
    }
}

/// A temperature from the device as °C, to a hundredth.
pub(crate) fn celsius(unit: TemperatureUnit, value: f32) -> f64 {
    round_to(unit.to_celsius(f64::from(value)), 2)
}

/// An entity of a kind Irori doesn't model yet, as listed on its device
/// (`docs/specs/protocols.md` §6.7). `None` for listings that aren't entities at all (the
/// device's user-defined actions) and for ones too new for this build to name.
pub fn unmodeled(device: &UniqueId, message: &EspHomeMessage) -> Option<Unmodeled> {
    use EspHomeMessage as M;
    let (platform, name) = match message {
        M::ListEntitiesAlarmControlPanelResponse(e) => ("alarm_control_panel", &e.name),
        M::ListEntitiesCameraResponse(e) => ("camera", &e.name),
        M::ListEntitiesClimateResponse(e) => ("climate", &e.name),
        M::ListEntitiesDateResponse(e) => ("date", &e.name),
        M::ListEntitiesDateTimeResponse(e) => ("datetime", &e.name),
        M::ListEntitiesInfraredResponse(e) => ("infrared", &e.name),
        M::ListEntitiesMediaPlayerResponse(e) => ("media_player", &e.name),
        M::ListEntitiesRadioFrequencyResponse(e) => ("radio_frequency", &e.name),
        M::ListEntitiesTimeResponse(e) => ("time", &e.name),
        M::ListEntitiesUpdateResponse(e) => ("update", &e.name),
        M::ListEntitiesWaterHeaterResponse(e) => ("water_heater", &e.name),
        _ => return None,
    };
    Some(Unmodeled {
        device_unique_id: Some(device.clone()),
        platform: ObjectId::try_from(platform).ok()?,
        name: Name::try_from(name.trim()).ok(),
        reason: None,
    })
}

/// ESPHome's `EntityCategory` (api.proto): 0 is none, 1 config, 2 diagnostic.
pub(crate) fn category(category: i32) -> Option<EntityCategory> {
    match category {
        1 => Some(EntityCategory::Config),
        2 => Some(EntityCategory::Diagnostic),
        _ => None,
    }
}

pub(crate) fn optional(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

pub(crate) fn first_non_empty<'a>(values: &[&'a String]) -> Option<&'a str> {
    values
        .iter()
        .map(|value| value.as_str())
        .find(|value| !value.is_empty())
}
