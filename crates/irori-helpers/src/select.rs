//! A choice: one of a list a person writes, `select.<id>`. "Home, away or asleep".

use irori_protocol::ProtocolError;
use irori_protocol::types::{Capabilities, Name, ObjectId, SelectCapabilities, Typed};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::Made;

/// `[selects.<id>]`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Select {
    pub name: Name,
    /// Everything it can be, at least one.
    #[schemars(length(min = 1, max = 256))]
    pub options: Vec<String>,
    /// What it is until someone chooses. The first option unless said.
    #[serde(default)]
    pub initial: Option<String>,
}

pub(crate) fn made(id: &ObjectId, select: &Select) -> Result<Vec<Made>, ProtocolError> {
    let capabilities = SelectCapabilities {
        options: select.options.clone(),
    };
    capabilities
        .validate()
        .map_err(|e| ProtocolError::new(format!("the choice `{id}`: {e}")))?;
    let initial = select
        .initial
        .clone()
        .or_else(|| select.options.first().cloned())
        .unwrap_or_default();
    Ok(vec![Made::value(
        "select",
        id,
        &select.name,
        Capabilities::Select(capabilities),
        Typed::Text(initial),
    )?])
}
