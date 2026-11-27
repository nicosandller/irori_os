//! `image`: a picture something keeps and changes now and then: a doorbell's last visitor, a
//! weather map, a vacuum's map of the floor. Home Assistant's `image`.
//!
//! Like a button it has no value of its own, so its `EntityState.state` is always `null`. What it
//! has is a picture, which a page asks its protocol for the way it asks a camera
//! (`docs/specs/protocols.md` §5.2).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What an image can do: be looked at. Nothing to say yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImageCapabilities {}
