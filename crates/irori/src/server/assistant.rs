//! `/api/dev/assistant`: whether a model is ready, and one turn of a remembered conversation.
//!
//! The key never leaves this process. A turn is a stream of server-sent events. The scope is a
//! path segment (`general`, `device:<id>`, `automation:<id>`), not a query: this server's axum
//! does not enable query parsing.

use std::convert::Infallible;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt as _;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::assistant::{self, ChatEvent, Turn};

use super::{AppState, refused};

pub async fn get(State(state): State<AppState>) -> Json<assistant::Status> {
    Json(assistant::read_status(turn(&state), data_dir(&state)).await)
}

pub async fn put(State(state): State<AppState>, Json(body): Json<serde_json::Value>) -> Response {
    match assistant::save(&state.0.config, &state.0.core, &body).await {
        Ok(_) => Json(assistant::read_status(turn(&state), data_dir(&state)).await).into_response(),
        Err(error) => refused(StatusCode::BAD_REQUEST, error),
    }
}

#[derive(Debug, Deserialize)]
pub struct Ask {
    scope: String,
    message: String,
}

pub async fn turns(State(state): State<AppState>, Json(ask): Json<Ask>) -> impl IntoResponse {
    let (tx, rx) = mpsc::channel(32);
    let core = state.0.core.clone();
    let config = state.0.config.clone();
    let history = state.0.history.clone();
    let db = state.0.db.path.clone();
    tokio::spawn(async move {
        assistant::take_turn(
            Turn {
                core: &core,
                config: &config,
                history: &history,
                db: &db,
            },
            ask.scope,
            ask.message,
            tx,
        )
        .await;
    });
    events(rx)
}

#[derive(Debug, Deserialize)]
pub struct Pull {
    tag: String,
}

pub async fn pull(State(state): State<AppState>, Json(body): Json<Pull>) -> impl IntoResponse {
    let (tx, rx) = mpsc::channel(32);
    let config = state.0.config.clone();
    tokio::spawn(async move {
        assistant::pull(&config, &body.tag, tx).await;
    });
    events(rx)
}

pub async fn transcript(State(state): State<AppState>, Path(scope): Path<String>) -> Response {
    match assistant::transcript(&state.0.db.path, &scope) {
        Ok(turns) => {
            let turns: Vec<Stored> = turns
                .iter()
                .map(|turn| Stored {
                    role: turn.role.as_str(),
                    body: turn.body.clone(),
                })
                .collect();
            Json(turns).into_response()
        }
        Err(error) => refused(StatusCode::BAD_REQUEST, error),
    }
}

pub async fn clear(State(state): State<AppState>, Path(scope): Path<String>) -> Response {
    match assistant::clear(&state.0.db.path, &scope) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => refused(StatusCode::BAD_REQUEST, error),
    }
}

#[derive(Debug, Serialize)]
struct Stored<'a> {
    role: &'a str,
    body: String,
}

fn events(
    rx: mpsc::Receiver<ChatEvent>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>> + Send> {
    Sse::new(ReceiverStream::new(rx).map(|event| {
        let data = match event {
            ChatEvent::Delta(text) => serde_json::json!({ "delta": text }),
            ChatEvent::Error(text) => serde_json::json!({ "error": text }),
            ChatEvent::Done => serde_json::json!({ "done": true }),
        };
        Ok(Event::default().data(data.to_string()))
    }))
}

fn turn(state: &AppState) -> Turn<'_> {
    Turn {
        core: &state.0.core,
        config: &state.0.config,
        history: &state.0.history,
        db: &state.0.db.path,
    }
}

fn data_dir(state: &AppState) -> &std::path::Path {
    state.0.db.path.parent().unwrap_or(&state.0.db.path)
}
