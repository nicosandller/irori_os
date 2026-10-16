//! A number: a value within a range, `number.<id>`. A brightness for the evening, a threshold a
//! flow compares against, set from a page instead of written into the flow.

use irori_protocol::ProtocolError;
use irori_protocol::types::{Capabilities, Name, NumberCapabilities, NumberMode, ObjectId, Typed};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::Made;

/// `[numbers.<id>]`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Number {
    pub name: Name,
    pub min: f64,
    pub max: f64,
    /// How finely it's set. 1 unless said.
    #[serde(default = "one")]
    pub step: f64,
    /// What it counts in, as it should read on a page: `%`, `°C`, `min`.
    #[serde(default)]
    pub unit: Option<String>,
    /// A slider, or a box to type in. Irori picks unless said.
    #[serde(default)]
    pub mode: NumberMode,
    /// What it is until someone sets it. Its `min` unless said.
    #[serde(default)]
    pub initial: Option<f64>,
}

pub(crate) fn one() -> f64 {
    1.0
}

pub(crate) fn made(id: &ObjectId, number: &Number) -> Result<Vec<Made>, ProtocolError> {
    let capabilities = NumberCapabilities {
        min: number.min,
        max: number.max,
        step: number.step,
        unit: number.unit.clone(),
        device_class: None,
        mode: number.mode,
    };
    capabilities
        .validate()
        .map_err(|e| ProtocolError::new(format!("the number `{id}`: {e}")))?;
    Ok(vec![Made::value(
        "number",
        id,
        &number.name,
        Capabilities::Number(capabilities),
        Typed::Number(number.initial.unwrap_or(number.min)),
    )?])
}
