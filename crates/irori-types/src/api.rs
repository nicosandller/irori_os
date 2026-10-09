//! Messages on the public websocket (`docs/specs/api.md` §4).
//!
//! Shared with the page, which is compiled to wasm, so this stays free of the server.

use serde::{Deserialize, Serialize};

use crate::{EntityId, EntityState};

/// One text frame on `GET /api/ws`. The server sends these. The client sends nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LiveMessage {
    /// The home as `GET /api/home` would return it, sent when the socket opens and again
    /// if this client fell behind.
    Snapshot { home: serde_json::Value },
    /// One entity's new state.
    State {
        entity_id: EntityId,
        state: Box<EntityState>,
    },
    /// The registry, an extension, the place, or settings changed. Read `/api/home` again.
    Changed,
}
