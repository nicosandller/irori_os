//! A text: a few words someone sets, `text.<id>`. A message for a display, a note a flow reads.

use irori_protocol::ProtocolError;
use irori_protocol::types::{Capabilities, Name, ObjectId, TextCapabilities, TextMode, Typed};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::Made;

/// `[texts.<id>]`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Text {
    pub name: Name,
    /// The fewest characters it takes. None unless said.
    #[serde(default)]
    #[schemars(range(max = 255))]
    pub min_length: u32,
    /// The most characters it takes. 255 unless said, which is also the most there is.
    #[serde(default = "longest")]
    #[schemars(range(max = 255))]
    pub max_length: u32,
    /// What it says until someone sets it. Nothing unless said.
    #[serde(default)]
    pub initial: String,
}

fn longest() -> u32 {
    255
}

pub(crate) fn made(id: &ObjectId, text: &Text) -> Result<Vec<Made>, ProtocolError> {
    let capabilities = TextCapabilities {
        min_length: text.min_length,
        max_length: text.max_length,
        pattern: None,
        mode: TextMode::Text,
    };
    capabilities
        .validate()
        .map_err(|e| ProtocolError::new(format!("the text `{id}`: {e}")))?;
    Ok(vec![Made::value(
        "text",
        id,
        &text.name,
        Capabilities::Text(capabilities),
        Typed::Text(text.initial.clone()),
    )?])
}
