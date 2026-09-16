//! What happened in the core, published to everyone who listens (the UI, the CLI, later rules).

use irori_types::{
    Context, Device, DeviceId, Entity, EntityId, EntityState, ExtensionId, ServiceName,
};
use serde::Serialize;

use crate::ExtensionStatus;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// An entity's state, availability, or attributes changed. `old_state` is `None` for a new
    /// entity.
    StateChanged {
        entity_id: EntityId,
        // Boxed: states are large, and most events aren't state changes.
        old_state: Option<Box<EntityState>>,
        new_state: Box<EntityState>,
    },
    DeviceAdded {
        device: Device,
    },
    DeviceUpdated {
        device: Device,
    },
    DeviceRemoved {
        device_id: DeviceId,
    },
    EntityAdded {
        entity: Entity,
    },
    EntityUpdated {
        entity: Entity,
    },
    EntityRemoved {
        entity_id: EntityId,
    },
    ExtensionStatusChanged {
        extension_id: ExtensionId,
        status: ExtensionStatus,
    },
    /// A service call was sent to an integration.
    ServiceCalled {
        entity_id: EntityId,
        service: ServiceName,
        context: Context,
    },
}
