//! A counter: a number that's counted up and down, `number.<id>`. "Times the doorbell rang
//! today". It's a number like any other, so `number.increment` and `number.decrement` count it
//! (the core works those out for every number) and `number.set_value` puts it back.

use irori_protocol::ProtocolError;
use irori_protocol::types::{Capabilities, Name, NumberCapabilities, NumberMode, ObjectId, Typed};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::Made;

/// `[counters.<id>]`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Counter {
    pub name: Name,
    /// Where it starts, and what a flow sets it back to. 0 unless said.
    #[serde(default)]
    pub initial: f64,
    /// How much one count is. 1 unless said.
    #[serde(default = "crate::number::one")]
    pub step: f64,
    /// The least it can be. 0 unless said.
    #[serde(default)]
    pub min: f64,
    /// The most it can be, where counting stops. A million unless said.
    #[serde(default = "a_million")]
    pub max: f64,
}

fn a_million() -> f64 {
    1_000_000.0
}

pub(crate) fn made(id: &ObjectId, counter: &Counter) -> Result<Vec<Made>, ProtocolError> {
    let capabilities = NumberCapabilities {
        min: counter.min,
        max: counter.max,
        step: counter.step,
        unit: None,
        device_class: None,
        // A count is read and typed, not dragged.
        mode: NumberMode::Box,
    };
    capabilities
        .validate()
        .map_err(|e| ProtocolError::new(format!("the counter `{id}`: {e}")))?;
    Ok(vec![Made::value(
        "counter",
        id,
        &counter.name,
        Capabilities::Number(capabilities),
        Typed::Number(counter.initial),
    )?])
}
