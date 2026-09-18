//! Event log for extensions. Official extensions are packages, not compiled into this binary.

use irori_core::Event;
use tokio::sync::broadcast::{Receiver, error::RecvError};

/// Logs the core's events: extension status at `info`, device and state changes at `debug`.
pub async fn log_events(mut events: Receiver<Event>) {
    loop {
        match events.recv().await {
            Ok(event) => log(&event),
            Err(RecvError::Lagged(missed)) => {
                tracing::warn!(missed, "event log fell behind; skipped events");
            }
            Err(RecvError::Closed) => return,
        }
    }
}

fn log(event: &Event) {
    match event {
        Event::ExtensionStatusChanged {
            extension_id,
            status,
        } => {
            let status = serde_json::to_string(status).unwrap_or_default();
            tracing::info!(extension = %extension_id, %status, "extension status");
        }
        Event::StateChanged {
            entity_id,
            new_state,
            ..
        } => {
            let state = serde_json::to_string(&new_state.state).unwrap_or_default();
            let origin = serde_json::to_string(&new_state.context.origin).unwrap_or_default();
            let caused_by = new_state
                .context
                .parent_id
                .as_ref()
                .map_or_else(String::new, ToString::to_string);
            tracing::debug!(
                entity = %entity_id,
                availability = ?new_state.availability,
                %state,
                %origin,
                %caused_by,
                "state changed"
            );
        }
        Event::DeviceAdded { device } => {
            tracing::debug!(device = %device.id, name = %device.name, integration = %device.integration, "device added");
        }
        Event::EntityAdded { entity } => {
            tracing::debug!(entity = %entity.id, name = %entity.name, "entity added");
        }
        Event::ServiceCalled {
            entity_id, service, ..
        } => {
            tracing::debug!(entity = %entity_id, %service, "service called");
        }
        Event::DeviceUpdated { .. }
        | Event::DeviceRemoved { .. }
        | Event::EntityUpdated { .. }
        | Event::EntityRemoved { .. } => {
            tracing::debug!(event = ?event, "registry changed");
        }
    }
}
