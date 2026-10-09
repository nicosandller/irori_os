//! The live socket and the socket an extension dials in on (`docs/specs/api.md` §4, §6).
//!
//! `/api/ws` only pushes. The client sends nothing; commands stay on HTTP. `/api/extension`
//! carries the same messages a child process would, one JSON object per frame.

use axum::Extension;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::Response;
use irori_core::Event;
use irori_protocol::{FromExt, ToExt};
use irori_types::{ExtensionId, LiveMessage};
use tokio::sync::mpsc;

use super::auth::Actor;
use super::{AppState, refused};

/// Pushes the home, then each change, for as long as the socket stays open.
pub async fn feed(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| push(socket, state))
}

async fn push(mut socket: WebSocket, state: AppState) {
    if send_snapshot(&mut socket, &state).await.is_err() {
        return;
    }
    let mut events = state.0.core.subscribe();
    loop {
        let message = match events.recv().await {
            Ok(event) => frame(event),
            // This client fell behind. A fresh picture, then a subscription that starts
            // after it, so the gap is the snapshot rather than a skipped event.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                events = events.resubscribe();
                if send_snapshot(&mut socket, &state).await.is_err() {
                    break;
                }
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        };
        if send(&mut socket, &message).await.is_err() {
            break;
        }
    }
}

fn frame(event: Event) -> LiveMessage {
    match event {
        Event::StateChanged {
            entity_id,
            new_state,
            ..
        } => LiveMessage::State {
            entity_id,
            state: new_state,
        },
        Event::DeviceAdded { .. }
        | Event::DeviceUpdated { .. }
        | Event::DeviceRemoved { .. }
        | Event::EntityAdded { .. }
        | Event::EntityUpdated { .. }
        | Event::EntityRemoved { .. }
        | Event::ExtensionStatusChanged { .. }
        | Event::PlaceChanged { .. }
        | Event::SettingsChanged
        | Event::ServiceCalled { .. } => LiveMessage::Changed,
    }
}

async fn send_snapshot(socket: &mut WebSocket, state: &AppState) -> Result<(), ()> {
    let home = serde_json::to_value(super::snapshot(state)).map_err(|error| {
        tracing::error!(%error, "couldn't write the home as JSON");
    })?;
    send(socket, &LiveMessage::Snapshot { home }).await
}

async fn send(socket: &mut WebSocket, message: &LiveMessage) -> Result<(), ()> {
    let text = serde_json::to_string(message).map_err(|error| {
        tracing::error!(%error, "couldn't write a live message");
    })?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|_| ())
}

/// One extension's connection. The guard has already required its token.
pub async fn extension(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Extension(who): Extension<Actor>,
) -> Response {
    let Some(id) = who.token.as_ref().and_then(|token| token.extension.clone()) else {
        return refused(
            StatusCode::UNAUTHORIZED,
            "an extension connects with the token made for it".to_owned(),
        );
    };
    ws.on_upgrade(move |socket| connect(socket, state, id))
}

async fn connect(mut socket: WebSocket, state: AppState, id: ExtensionId) {
    // 64, the same bound the host uses for a child process's calls.
    let (outgoing, mut to_socket) = mpsc::channel::<ToExt>(64);
    let (from_socket, incoming) = mpsc::channel::<FromExt>(64);
    if let Err(why) = state
        .0
        .host
        .offer_inbound(&id, irori_core::InboundLink { incoming, outgoing })
    {
        let _ = socket
            .send(Message::Close(Some(CloseFrame {
                code: 1008,
                reason: why.into(),
            })))
            .await;
        return;
    }
    loop {
        tokio::select! {
            biased;
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(message) = serde_json::from_str::<FromExt>(&text) else {
                            break;
                        };
                        if from_socket.send(message).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_) | Message::Ping(_) | Message::Pong(_))) => {}
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break,
                }
            }
            outgoing = to_socket.recv() => {
                let Some(message) = outgoing else { break };
                let Ok(text) = serde_json::to_string(&message) else { break };
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_types::{Availability, EntityId, EntityState};

    fn state() -> EntityState {
        serde_json::from_value(serde_json::json!({
            "entity_id": "light.hall",
            "availability": "available",
            "state": { "kind": "light", "on": true },
            "last_changed": "2026-01-01T00:00:00Z",
            "last_updated": "2026-01-01T00:00:00Z",
            "last_reported": "2026-01-01T00:00:00Z",
            "context": {
                "id": "01K5B2Q9A1B2C3D4E5F6G7H8J9",
                "origin": { "type": "system" }
            }
        }))
        .expect("a valid state")
    }

    #[test]
    fn a_state_change_names_the_entity_and_anything_else_is_a_nudge() {
        let entity_id = EntityId::try_from("light.hall").expect("valid");
        let state_frame = frame(Event::StateChanged {
            entity_id: entity_id.clone(),
            old_state: None,
            new_state: Box::new(state()),
        });
        assert!(matches!(
            state_frame,
            LiveMessage::State { entity_id: id, state }
                if id == entity_id && state.availability == Availability::Available
        ));
        assert_eq!(frame(Event::SettingsChanged), LiveMessage::Changed);
    }
}
