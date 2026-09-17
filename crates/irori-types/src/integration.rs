//! What an integration and the core say to each other. See `docs/specs/integrations.md`.
//!
//! Integrations refer to their devices and entities by `unique_id`, their own permanent handle.
//! The core assigns the user-facing ids (`DeviceId`, `EntityId`), which the user may rename.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use crate::num::{Num, whole};
use crate::{
    Attributes, Capabilities, Context, ContextId, EntityKind, InvariantError, Name, ObjectId,
    State, UniqueId,
};

/// Something an integration found but can't use yet, because it needs a person first: a device
/// that wants an encryption key, one that has to be paired, an account that has to be signed in
/// to. See `docs/specs/integrations.md` §6.6.
///
/// Not a device in the registry. It has no entities and nothing is known about it beyond what
/// it announced, so putting it there would show a device that can't do anything. It's listed on
/// its extension instead, where the UI can offer to fix it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Waiting {
    /// The same handle the device will have once it's in the registry, so what a person
    /// provides now is attached to what it's for (ROADMAP D31).
    pub unique_id: UniqueId,
    /// What it calls itself, for recognising it.
    pub name: Name,
    /// What's needed, in a sentence, e.g. "it wants an encryption key".
    pub reason: String,
    /// Where a secret that would unlock it goes, if a secret is what it needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<SecretRequest>,
}

/// Where a secret goes: a path inside the extension's table in `secrets.toml`
/// (`docs/specs/config.md`). The extension decides the path, so the UI and the core can take a
/// secret for any extension without knowing what it means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretRequest {
    /// Table keys from the extension's own table down to the value, e.g.
    /// `["keys", "00:11:22:33:44:55"]`. At least one, and none empty — the same rules
    /// [`crate::ExtensionSettings::set`] enforces when the secret is written.
    pub path: Vec<String>,
    /// What to call the field, e.g. "Encryption key".
    pub label: String,
    /// Where to find it, e.g. "`api: encryption: key:` in the device's YAML".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSecretRequest {
    path: Vec<String>,
    label: String,
    #[serde(default)]
    hint: Option<String>,
}

impl<'de> Deserialize<'de> for SecretRequest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawSecretRequest::deserialize(deserializer)?;
        let request = SecretRequest {
            path: raw.path,
            label: raw.label,
            hint: raw.hint,
        };
        request.validate().map_err(serde::de::Error::custom)?;
        Ok(request)
    }
}

impl SecretRequest {
    /// Deserialization runs this; call it yourself when building a request in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.path.is_empty() {
            return Err(InvariantError(
                "a secret request needs a path of at least one key".into(),
            ));
        }
        if self.path.iter().any(|key| key.is_empty()) {
            return Err(InvariantError(
                "a secret request's path can't have an empty key in it".into(),
            ));
        }
        Ok(())
    }
}

/// A device as an integration describes it. The core adds it to the registry, or updates the
/// entry with the same `unique_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceDescription {
    /// The integration's permanent handle for the device, e.g. its MAC address.
    pub unique_id: UniqueId,
    pub name: Name,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sw_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hw_version: Option<String>,
    /// An area name the device reports for itself, e.g. ESPHome's `area`. The core uses it only
    /// when the device is new and the user hasn't assigned an area.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_area: Option<Name>,
    /// The `unique_id` of the bridge or hub this device is reached through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_device_unique_id: Option<UniqueId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDeviceDescription {
    unique_id: UniqueId,
    name: Name,
    #[serde(default)]
    manufacturer: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    sw_version: Option<String>,
    #[serde(default)]
    hw_version: Option<String>,
    #[serde(default)]
    suggested_area: Option<Name>,
    #[serde(default)]
    via_device_unique_id: Option<UniqueId>,
}

impl<'de> Deserialize<'de> for DeviceDescription {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawDeviceDescription::deserialize(deserializer)?;
        let device = DeviceDescription {
            unique_id: raw.unique_id,
            name: raw.name,
            manufacturer: raw.manufacturer,
            model: raw.model,
            sw_version: raw.sw_version,
            hw_version: raw.hw_version,
            suggested_area: raw.suggested_area,
            via_device_unique_id: raw.via_device_unique_id,
        };
        device.validate().map_err(serde::de::Error::custom)?;
        Ok(device)
    }
}

impl DeviceDescription {
    /// Deserialization runs this; call it yourself when building a description in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.via_device_unique_id.as_ref() == Some(&self.unique_id) {
            return Err(InvariantError(format!(
                "device `{}` can't be reached via itself (via_device_unique_id is its own unique_id)",
                self.unique_id
            )));
        }
        Ok(())
    }
}

/// An entity as an integration describes it. The core adds it to the registry, or updates the
/// entry with the same `unique_id`.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = crate::schema::entity_description_name_or_device)]
pub struct EntityDescription {
    /// The integration's permanent handle for the entity.
    pub unique_id: UniqueId,
    /// Leave out for a device's main feature (e.g. the relay of a smart plug): the entity then
    /// uses its device's name, and must have `device_unique_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Name>,
    /// The `unique_id` of the device it belongs to. The device must be described first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_unique_id: Option<UniqueId>,
    /// What to call it after the `.` in its entity id, when it's new. Otherwise the core derives
    /// one from the names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_object_id: Option<ObjectId>,
    /// What it can do. `capabilities.kind` is the entity's kind.
    pub capabilities: Capabilities,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntityDescription {
    unique_id: UniqueId,
    #[serde(default)]
    name: Option<Name>,
    #[serde(default)]
    device_unique_id: Option<UniqueId>,
    #[serde(default)]
    suggested_object_id: Option<ObjectId>,
    capabilities: Capabilities,
}

impl<'de> Deserialize<'de> for EntityDescription {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawEntityDescription::deserialize(deserializer)?;
        let entity = EntityDescription {
            unique_id: raw.unique_id,
            name: raw.name,
            device_unique_id: raw.device_unique_id,
            suggested_object_id: raw.suggested_object_id,
            capabilities: raw.capabilities,
        };
        entity.validate().map_err(serde::de::Error::custom)?;
        Ok(entity)
    }
}

impl EntityDescription {
    /// Deserialization runs this; call it yourself when building a description in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.name.is_none() && self.device_unique_id.is_none() {
            return Err(InvariantError(format!(
                "entity `{}` has no name, so it must belong to a device (set `device_unique_id`) whose name it uses",
                self.unique_id
            )));
        }
        self.capabilities
            .validate()
            .map_err(|e| InvariantError(format!("entity `{}`: {e}", self.unique_id)))
    }

    pub fn kind(&self) -> EntityKind {
        self.capabilities.kind()
    }
}

/// A new value for one entity, from its integration. The core adds the timestamps and context,
/// and checks the value against the entity's kind and capabilities.
///
/// Reporting a value doesn't change the entity's availability; that's reported separately.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = crate::schema::state_report_requires_state)]
pub struct StateReport {
    pub unique_id: UniqueId,
    /// The typed value, or `null` if the device doesn't know it (e.g. it just restarted). Must be
    /// present even when `null`.
    pub state: Option<State>,
    /// Replaces all of the entity's attributes. Leave out to clear them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(schema_with = "crate::schema::attributes_schema")]
    pub attributes: Attributes,
    /// The context of the service call that caused this change, when the integration knows it
    /// (e.g. the device confirmed a command). Otherwise the change is attributed to the device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by: Option<ContextId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStateReport {
    unique_id: UniqueId,
    // `deserialize_with` turns off serde's "missing Option means None", making the key required.
    #[serde(deserialize_with = "Option::deserialize")]
    state: Option<State>,
    #[serde(default)]
    attributes: Attributes,
    #[serde(default)]
    caused_by: Option<ContextId>,
}

impl<'de> Deserialize<'de> for StateReport {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawStateReport::deserialize(deserializer)?;
        let report = StateReport {
            unique_id: raw.unique_id,
            state: raw.state,
            attributes: raw.attributes,
            caused_by: raw.caused_by,
        };
        report.validate().map_err(serde::de::Error::custom)?;
        Ok(report)
    }
}

impl StateReport {
    /// Deserialization runs this; call it yourself when building a report in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        match &self.state {
            Some(state) => state
                .validate()
                .map_err(|e| InvariantError(format!("entity `{}`: {e}", self.unique_id))),
            None => Ok(()),
        }
    }
}

/// The core asking an integration to act on one of its entities.
///
/// By the time an integration receives a call, the core has checked that the entity exists,
/// belongs to it, is of the service's kind, and supports what's asked (e.g. `brightness` only on
/// a dimmable light).
///
/// There's no `entity_id`: that's the user's name for the entity and may change, while the
/// integration only ever uses its own `unique_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceCall {
    /// Which entity, in the integration's own terms.
    pub unique_id: UniqueId,
    pub service: Service,
    /// Why it's being called. Pass its id back as `caused_by` when reporting the result.
    pub context: Context,
}

/// A service and its data. The standard services of every entity kind; each integration handles
/// the ones for the kinds it provides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Service {
    LightTurnOn(LightTurnOn),
    LightTurnOff,
    SwitchTurnOn,
    SwitchTurnOff,
}

impl Service {
    pub fn name(&self) -> ServiceName {
        match self {
            Self::LightTurnOn(_) => ServiceName::LightTurnOn,
            Self::LightTurnOff => ServiceName::LightTurnOff,
            Self::SwitchTurnOn => ServiceName::SwitchTurnOn,
            Self::SwitchTurnOff => ServiceName::SwitchTurnOff,
        }
    }
}

/// The name of a standard service: `<kind>.<action>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ServiceName {
    #[serde(rename = "light.turn_on")]
    LightTurnOn,
    #[serde(rename = "light.turn_off")]
    LightTurnOff,
    #[serde(rename = "switch.turn_on")]
    SwitchTurnOn,
    #[serde(rename = "switch.turn_off")]
    SwitchTurnOff,
}

impl ServiceName {
    pub const ALL: [ServiceName; 4] = [
        Self::LightTurnOn,
        Self::LightTurnOff,
        Self::SwitchTurnOn,
        Self::SwitchTurnOff,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LightTurnOn => "light.turn_on",
            Self::LightTurnOff => "light.turn_off",
            Self::SwitchTurnOn => "switch.turn_on",
            Self::SwitchTurnOff => "switch.turn_off",
        }
    }

    /// The kind of entity it acts on.
    pub fn kind(self) -> EntityKind {
        match self {
            Self::LightTurnOn | Self::LightTurnOff => EntityKind::Light,
            Self::SwitchTurnOn | Self::SwitchTurnOff => EntityKind::Switch,
        }
    }

    /// Whether it takes `data`.
    fn takes_data(self) -> bool {
        matches!(self, Self::LightTurnOn)
    }
}

impl fmt::Display for ServiceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The JSON shape of a [`ServiceCall`]: the service's data sits next to its name.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServiceCall {
    service: ServiceName,
    unique_id: UniqueId,
    // Present-but-null is an error, not "no data": the schema says `data` is an object.
    #[serde(
        default,
        deserialize_with = "some_object",
        skip_serializing_if = "Option::is_none"
    )]
    data: Option<serde_json::Map<String, serde_json::Value>>,
    context: Context,
}

fn some_object<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, D::Error> {
    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Object(map) => Ok(Some(map)),
        other => Err(serde::de::Error::custom(format!(
            "`data` must be an object, not {}; leave `data` out when there's nothing to send",
            json_type(&other)
        ))),
    }
}

fn json_type(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

impl Serialize for ServiceCall {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let data = match &self.service {
            Service::LightTurnOn(data) if *data != LightTurnOn::default() => {
                match serde_json::to_value(data).map_err(serde::ser::Error::custom)? {
                    serde_json::Value::Object(map) => Some(map),
                    _ => None,
                }
            }
            _ => None,
        };
        RawServiceCall {
            service: self.service.name(),
            unique_id: self.unique_id.clone(),
            data,
            context: self.context.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ServiceCall {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let raw = RawServiceCall::deserialize(deserializer)?;
        let name = raw.service;
        let data = raw.data.unwrap_or_default();
        if !name.takes_data() && !data.is_empty() {
            return Err(D::Error::custom(format!("`{name}` takes no data")));
        }
        let service = match name {
            ServiceName::LightTurnOn => Service::LightTurnOn(
                LightTurnOn::deserialize(serde_json::Value::Object(data))
                    .map_err(|e| D::Error::custom(format!("`{name}` data: {e}")))?,
            ),
            ServiceName::LightTurnOff => Service::LightTurnOff,
            ServiceName::SwitchTurnOn => Service::SwitchTurnOn,
            ServiceName::SwitchTurnOff => Service::SwitchTurnOff,
        };
        let call = ServiceCall {
            unique_id: raw.unique_id,
            service,
            context: raw.context,
        };
        call.validate().map_err(D::Error::custom)?;
        Ok(call)
    }
}

impl ServiceCall {
    /// Deserialization runs this; call it yourself when building a call in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        match &self.service {
            Service::LightTurnOn(data) => data.validate(),
            _ => Ok(()),
        }
    }
}

impl JsonSchema for ServiceCall {
    fn schema_name() -> Cow<'static, str> {
        "ServiceCall".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let no_data = json_schema!({ "type": "object", "maxProperties": 0 });
        let light_turn_on = generator.subschema_for::<LightTurnOn>();
        // Per service: the shape of `data`.
        let rules: Vec<_> = ServiceName::ALL
            .iter()
            .map(|name| {
                let data = match name {
                    ServiceName::LightTurnOn => light_turn_on.clone(),
                    _ => no_data.clone(),
                };
                json_schema!({
                    "if": {
                        "properties": { "service": { "const": name.as_str() } },
                        "required": ["service"],
                    },
                    "then": { "properties": { "data": data } },
                })
            })
            .collect();
        json_schema!({
            "type": "object",
            "description": "The core asking an integration to act on one of its entities.",
            "properties": {
                "service": generator.subschema_for::<ServiceName>(),
                "unique_id": generator.subschema_for::<UniqueId>(),
                "data": { "type": "object", "description": "The service's parameters. Leave out when there are none." },
                "context": generator.subschema_for::<Context>(),
            },
            "required": ["service", "unique_id", "context"],
            "additionalProperties": false,
            "allOf": rules,
        })
    }
}

/// Data for `light.turn_on`. Everything is optional: with no data, the light turns on at its
/// last brightness and color.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = crate::schema::one_color_setting)]
pub struct LightTurnOn {
    /// 1-255. Needs a dimmable light.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 255))]
    pub brightness: Option<u8>,
    /// Needs color temperature support, within the light's range. Not together with `rgb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1000, max = 20000))]
    pub color_temp_kelvin: Option<u16>,
    /// Needs RGB support. Not together with `color_temp_kelvin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgb: Option<[u8; 3]>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLightTurnOn {
    // Numbers as written, so the checks below can name the field (see `crate::num`).
    #[serde(default)]
    brightness: Option<Num>,
    #[serde(default)]
    color_temp_kelvin: Option<Num>,
    #[serde(default)]
    rgb: Option<[Num; 3]>,
}

impl<'de> Deserialize<'de> for LightTurnOn {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let raw = RawLightTurnOn::deserialize(deserializer)?;
        if raw
            .brightness
            .is_some_and(|n| whole::<u8>("brightness", n, 0, 0).is_ok())
        {
            return Err(D::Error::custom(TURN_ON_BRIGHTNESS_ZERO));
        }
        let rgb = match raw.rgb {
            Some([r, g, b]) => Some([
                whole("rgb[0]", r, 0, 255).map_err(D::Error::custom)?,
                whole("rgb[1]", g, 0, 255).map_err(D::Error::custom)?,
                whole("rgb[2]", b, 0, 255).map_err(D::Error::custom)?,
            ]),
            None => None,
        };
        let data = LightTurnOn {
            brightness: raw
                .brightness
                .map(|n| whole("brightness", n, 1, 255))
                .transpose()
                .map_err(D::Error::custom)?,
            color_temp_kelvin: raw
                .color_temp_kelvin
                .map(|n| whole("color_temp_kelvin", n, 1000, 20000))
                .transpose()
                .map_err(D::Error::custom)?,
            rgb,
        };
        data.validate().map_err(D::Error::custom)?;
        Ok(data)
    }
}

const TURN_ON_BRIGHTNESS_ZERO: &str =
    "brightness 0 is invalid; brightness is 1-255 (use `light.turn_off` to turn a light off)";

impl LightTurnOn {
    /// Deserialization runs this; call it yourself when building the data in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if self.brightness == Some(0) {
            return Err(InvariantError(TURN_ON_BRIGHTNESS_ZERO.into()));
        }
        if let Some(kelvin) = self.color_temp_kelvin
            && !(1000..=20000).contains(&kelvin)
        {
            return Err(InvariantError(format!(
                "color_temp_kelvin {kelvin} is out of range; it must be from 1000 to 20000"
            )));
        }
        if self.color_temp_kelvin.is_some() && self.rgb.is_some() {
            return Err(InvariantError(
                "set `color_temp_kelvin` or `rgb`, not both".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod secret_request_tests {
    use super::*;

    #[test]
    fn an_empty_secret_path_is_refused() {
        let empty = SecretRequest {
            path: vec![],
            label: "Key".into(),
            hint: None,
        };
        assert!(empty.validate().is_err());
        let blank_key = SecretRequest {
            path: vec!["keys".into(), "".into()],
            label: "Key".into(),
            hint: None,
        };
        assert!(blank_key.validate().is_err());
        let ok = SecretRequest {
            path: vec!["keys".into(), "aa:bb".into()],
            label: "Key".into(),
            hint: None,
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn deserializing_an_empty_secret_path_fails() {
        let err = serde_json::from_value::<SecretRequest>(serde_json::json!({
            "path": [],
            "label": "Key"
        }));
        assert!(err.is_err(), "{err:?}");
    }
}
