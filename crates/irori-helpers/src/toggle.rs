//! A toggle: an on/off value, `switch.<id>`. "Guests are over", "holiday mode".

use irori_protocol::ProtocolError;
use irori_protocol::types::{Capabilities, Name, ObjectId, SwitchCapabilities, Typed};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::Made;

/// `[toggles.<id>]`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Toggle {
    pub name: Name,
    /// What it is until someone switches it.
    #[serde(default)]
    pub initial: bool,
}

pub(crate) fn made(id: &ObjectId, toggle: &Toggle) -> Result<Vec<Made>, ProtocolError> {
    Ok(vec![Made::value(
        "toggle",
        id,
        &toggle.name,
        Capabilities::Switch(SwitchCapabilities { device_class: None }),
        Typed::Bool(toggle.initial),
    )?])
}
