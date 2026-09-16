//! The built-in extensions compiled into this build (cargo features, ROADMAP D17), and a log of
//! what they do.

use irori_core::Event;
use irori_integration::Builtin;
use tokio::sync::broadcast::{Receiver, error::RecvError};

/// Every built-in extension in this build. All of them run until the config dir (M0.7) lets
/// people choose.
pub fn builtins() -> anyhow::Result<Vec<Builtin>> {
    Ok(vec![
        #[cfg(feature = "int-demo")]
        irori_integration::builtin::<irori_int_demo::Demo>().map_err(anyhow::Error::msg)?,
        #[cfg(feature = "int-esphome")]
        irori_integration::builtin::<irori_int_esphome::Esphome>().map_err(anyhow::Error::msg)?,
        #[cfg(feature = "int-helpers")]
        irori_integration::builtin::<irori_int_helpers::Helpers>().map_err(anyhow::Error::msg)?,
    ])
}

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
