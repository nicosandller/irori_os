//! What a built-in protocol may read of the home beyond its own entities, and ask of it: the
//! in-process counterpart of an engine's requests (`docs/specs/automations.md` §B2), for
//! built-ins that work out values from other entities (a helper that groups lights, one that
//! meters a power reading).
//!
//! A built-in has it only when its manifest asks for API scopes, and each method needs its
//! scope, as an engine's requests do. Without the scope a method answers with nothing, or a
//! refusal, rather than panicking: a built-in that asked for too little is a bug to see in its
//! log, not a crash.

use std::future::Future;
use std::pin::Pin;

use irori_types::{ContextId, Entity, EntityId, EntityState, HomeSettings, Timestamp};
use tokio::sync::mpsc;

/// A service call's answer, on its way.
pub type Answer = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// The home, as a built-in reads it. Implemented by the core.
pub trait Home: Send + Sync + std::fmt::Debug {
    /// Every entity (`registry:read`).
    fn entities(&self) -> Vec<Entity>;
    /// Every entity's state now (`states:read`).
    fn states(&self) -> Vec<EntityState>;
    /// Every state change from now on, oldest first (`states:read`). A listener that falls far
    /// behind misses changes rather than holding the home up; the next change of an entity says
    /// where it is now.
    fn watch(&self) -> mpsc::UnboundedReceiver<EntityState>;
    /// An entity's states since `since`, oldest first (`history:read`).
    fn history(&self, entity_id: &EntityId, since: Timestamp) -> Vec<EntityState>;
    /// Where the home is and its time zone, for anything that keeps to its clock. Always
    /// readable: it's no secret of anyone's.
    fn place(&self) -> HomeSettings;
    /// Asks an entity to do something, as this extension, on behalf of the call `caused_by`
    /// when there was one (`services:call`). `action` is the part of the service after the
    /// dot.
    fn call(
        &self,
        entity_id: EntityId,
        action: String,
        data: serde_json::Map<String, serde_json::Value>,
        caused_by: Option<ContextId>,
    ) -> Answer;
}
