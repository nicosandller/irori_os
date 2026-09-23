//! Turning a [`crate::discovery::ParsedConfig`] into what the protocol contract wants:
//! `DeviceDescription` and `EntityDescription` (`docs/specs/protocols.md` §6.1-6.2).

use irori_types::{DeviceDescription, EntityDescription, ObjectId};

use crate::discovery::ParsedConfig;

/// `parsed.device`, if any, as a `DeviceDescription` ready for `ctx.describe_device`.
pub fn device(parsed: &ParsedConfig) -> Option<DeviceDescription> {
    let device = parsed.device.as_ref()?;
    Some(DeviceDescription {
        unique_id: device.unique_id.clone(),
        name: device.name.clone(),
        manufacturer: device.manufacturer.clone(),
        model: device.model.clone(),
        sw_version: device.sw_version.clone(),
        hw_version: device.hw_version.clone(),
        suggested_area: None, // HA discovery has no equivalent of ESPHome's `area`
        via_device_unique_id: device.via_device_unique_id.clone(),
    })
}

/// `parsed` as an `EntityDescription` ready for `ctx.describe_entity`. `object_id_hint` is the
/// discovery topic's own `object_id` segment — a reasonable `suggested_object_id` when it
/// already looks like a slug, since that's the part of the entity id a person would otherwise
/// have to make up themselves.
pub fn entity(parsed: &ParsedConfig, object_id_hint: &str) -> EntityDescription {
    EntityDescription {
        unique_id: parsed.unique_id.clone(),
        name: parsed.name.clone(),
        device_unique_id: parsed.device.as_ref().map(|d| d.unique_id.clone()),
        suggested_object_id: ObjectId::try_from(object_id_hint).ok(),
        capabilities: parsed.capabilities.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{self};
    use crate::topic::Component;

    const Z2M_LIGHT: &str = r#"{
        "unique_id": "0x0017880104e45520_light",
        "device": {
            "identifiers": ["0x0017880104e45520"],
            "name": "Living room lamp"
        },
        "schema": "json",
        "state_topic": "zigbee2mqtt/Living room lamp",
        "command_topic": "zigbee2mqtt/Living room lamp/set"
    }"#;

    #[test]
    fn maps_a_device_and_entity_with_a_slug_object_id_hint() {
        let parsed = discovery::parse(Component::Light, Z2M_LIGHT.as_bytes()).expect("valid");
        let described_device = device(&parsed).expect("has a device");
        assert_eq!(described_device.unique_id.as_str(), "0x0017880104e45520");

        let described_entity = entity(&parsed, "living_room_lamp");
        assert_eq!(
            described_entity.device_unique_id,
            Some(described_device.unique_id)
        );
        assert_eq!(
            described_entity
                .suggested_object_id
                .map(|id| id.to_string()),
            Some("living_room_lamp".to_owned())
        );
    }

    #[test]
    fn a_deviceless_entity_keeps_its_own_name() {
        let payload = br#"{
            "unique_id": "tasmota_ABC123_switch",
            "name": "Garage plug",
            "command_topic": "tasmota/garage/cmnd/POWER"
        }"#;
        let parsed = discovery::parse(Component::Switch, payload).expect("valid");
        assert!(parsed.device.is_none());
        let described = entity(&parsed, "garage_plug");
        assert_eq!(
            described.name.as_ref().map(|n| n.as_str()),
            Some("Garage plug")
        );
        assert_eq!(described.device_unique_id, None);
    }
}
