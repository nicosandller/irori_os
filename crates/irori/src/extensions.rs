//! The event log: what the core said, written as Irori's own log lines.
//!
//! Official extensions are packages, not compiled into this binary. A built-in is the other kind
//! of thing the host runs — helpers, part of Irori itself (ROADMAP D45) — and the two are worded
//! differently here, because `extension=` in a log means "one of the installable ones on the
//! Extensions page" to anyone reading it.

use std::collections::BTreeSet;

use irori_core::Event;
use irori_types::ExtensionId;
use tokio::sync::broadcast::{Receiver, error::RecvError};

/// Logs the core's events: extension status at `info`, device and state changes at `debug`.
///
/// `builtins` are the ids Irori compiled in rather than installed (helpers, ROADMAP D45). A line
/// about one of those says `builtin`, not `extension`: they never appear on the Extensions page,
/// so a line naming helpers an extension points a person at a list it is deliberately absent from.
pub async fn log_events(mut events: Receiver<Event>, builtins: BTreeSet<ExtensionId>) {
    loop {
        match events.recv().await {
            Ok(event) => log(&event, &builtins),
            Err(RecvError::Lagged(missed)) => {
                tracing::warn!(missed, "event log fell behind; skipped events");
            }
            Err(RecvError::Closed) => return,
        }
    }
}

fn log(event: &Event, builtins: &BTreeSet<ExtensionId>) {
    match event {
        Event::ExtensionStatusChanged {
            extension_id,
            status,
        } => {
            let status = serde_json::to_string(status).unwrap_or_default();
            if builtins.contains(extension_id) {
                tracing::info!(builtin = %extension_id, %status, "builtin status");
            } else {
                tracing::info!(extension = %extension_id, %status, "extension status");
            }
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
            tracing::debug!(device = %device.id, name = %device.name, protocol = %device.protocol, "device added");
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

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex, PoisonError};

    use irori_core::ExtensionStatus;
    use tracing_subscriber::fmt::MakeWriter;

    use super::*;

    /// The lines one call to [`log`] wrote, with the subscriber scoped to the call — so each test
    /// reads its own lines and no test sets a process-wide default that the next one inherits.
    fn said(write: impl FnOnce()) -> String {
        let kept = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(Keep(Arc::clone(&kept)))
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, write);
        let kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8(kept.clone()).expect("a log line is text")
    }

    /// The writer a subscriber formats into: it keeps what it was given instead of printing it.
    #[derive(Clone, Debug)]
    struct Keep(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Keep {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Keep {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn id(of: &str) -> ExtensionId {
        of.parse().expect("a valid extension id")
    }

    /// The ids of whatever Irori was built with, as the log is told about them.
    fn builtins(of: &[&str]) -> BTreeSet<ExtensionId> {
        of.iter().copied().map(id).collect()
    }

    fn status_of(extension: &str) -> Event {
        Event::ExtensionStatusChanged {
            extension_id: id(extension),
            status: ExtensionStatus::Running,
        }
    }

    /// A built-in is part of Irori, not something a person installed (ROADMAP D45). A line that
    /// calls it an extension points at the Extensions page, which is exactly where it isn't —
    /// which is the whole of issue 34: `extension=helpers` in the log, helpers having stopped
    /// being an extension days earlier.
    #[test]
    fn a_builtin_is_not_logged_as_an_extension() {
        let line = said(|| log(&status_of("helpers"), &builtins(&["helpers"])));
        assert!(line.contains("builtin=helpers"), "{line}");
        assert!(!line.contains("extension=helpers"), "{line}");
        // The state itself still says what happened — only the name of the thing changed.
        assert!(line.contains(r#"status={"state":"running"}"#), "{line}");
    }

    /// An installed package is an extension, and stays one in the log: it is what the Extensions
    /// page lists, what a person uninstalls, and what `extension=<id>` greps for.
    #[test]
    fn an_installed_extension_is_still_logged_as_an_extension() {
        let line = said(|| log(&status_of("demo"), &builtins(&["helpers"])));
        assert!(line.contains("extension=demo"), "{line}");
        assert!(line.contains("extension status"), "{line}");
        assert!(!line.contains("builtin"), "{line}");
    }
}
