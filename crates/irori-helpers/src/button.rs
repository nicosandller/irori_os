//! A button: something to press, from a page or a flow, that other flows start from. "Movie
//! time", "Leaving".
//!
//! Two entities, because pressing and having been pressed are different things
//! (`docs/specs/entities.md` §5.3): `button.<id>` is what's pressed and has no value, and
//! `event.<id>` reports `pressed` every time it is. A flow starts from the event.

use irori_protocol::ProtocolError;
use irori_protocol::types::{ButtonCapabilities, Capabilities, EventCapabilities, Name, ObjectId};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::Made;

/// `[buttons.<id>]`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Button {
    pub name: Name,
}

/// What its event reports.
pub(crate) const PRESSED: &str = "pressed";

pub(crate) fn made(id: &ObjectId, button: &Button) -> Result<Vec<Made>, ProtocolError> {
    let pressed = Made::happens(
        "pressed",
        id,
        // A name already as long as a name can be keeps it as it is.
        &Name::try_from(format!("{} pressed", button.name).as_str())
            .unwrap_or_else(|_| button.name.clone()),
        Capabilities::Event(EventCapabilities {
            event_types: vec![PRESSED.to_owned()],
            device_class: None,
        }),
    )?;
    let press = Made::press(
        "button",
        id,
        &button.name,
        Capabilities::Button(ButtonCapabilities { device_class: None }),
        pressed.unique.clone(),
    )?;
    Ok(vec![press, pressed])
}
