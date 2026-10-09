//! `/api/assistant`: whether a model is ready, and one turn of a remembered conversation.
//!
//! The key never leaves this process. A turn is a stream of server-sent events. The scope is a
//! path segment (`general`, `device:<id>`, `automation:<id>`, `floorplan:<floor>`), not a query: this server's axum
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
    match assistant::save(&state.0.config, &state.0.core, data_dir(&state), &body).await {
        Ok(_) => Json(assistant::read_status(turn(&state), data_dir(&state)).await).into_response(),
        Err(error) => refused(StatusCode::BAD_REQUEST, error),
    }
}

#[derive(Debug, Deserialize)]
pub struct Ask {
    scope: String,
    message: String,
    /// The floorplan the person is editing, sent by the Floorplan page while they are. The
    /// model may draw on it and the page is sent the result; nothing here saves it.
    #[serde(default)]
    plan: Option<irori_types::Floorplan>,
    /// A picture or PDF of a floorplan to draw from. Read for this answer and not kept.
    #[serde(default)]
    attachment: Option<assistant::Attachment>,
}

/// The most a question may weigh: the words, the plan being edited, and a file of the size
/// [`assistant::Attachment`] allows, with room to spare.
pub const MOST_ASKED: usize = 14 * 1024 * 1024;

/// Takes a question and streams its answer. The answer is Irori's to finish from here: the
/// page leaving stops the stream, not the answer.
pub async fn turns(State(state): State<AppState>, Json(ask): Json<Ask>) -> impl IntoResponse {
    let pending = match state.0.turns.begin(&ask.scope, &ask.message) {
        Ok(pending) => pending,
        Err(error) => {
            let (tx, rx) = mpsc::channel(1);
            let _ = tx.try_send(ChatEvent::Error(error));
            return events(rx);
        }
    };
    let following = pending.follow();
    tokio::spawn(async move {
        let scope = ask.scope;
        state
            .0
            .turns
            .run(&scope, pending, |tx| {
                assistant::take_turn(
                    turn(&state),
                    scope.clone(),
                    ask.message,
                    ask.plan,
                    ask.attachment,
                    tx,
                )
            })
            .await;
    });
    events(following)
}

/// The answer `scope` is in the middle of, from its first word, for a page that wasn't there
/// when it was asked. Ends at once when nothing is being answered.
pub async fn follow(State(state): State<AppState>, Path(scope): Path<String>) -> impl IntoResponse {
    match state.0.turns.pending(&scope) {
        Some(pending) => events(pending.follow()),
        None => {
            let (tx, rx) = mpsc::channel(1);
            let _ = tx.try_send(ChatEvent::Done);
            events(rx)
        }
    }
}

/// Stops the answer `scope` is in the middle of. Neither it nor its question is kept.
pub async fn stop(State(state): State<AppState>, Path(scope): Path<String>) -> StatusCode {
    state.0.turns.stop(&scope);
    StatusCode::NO_CONTENT
}

#[derive(Debug, Deserialize)]
pub struct Pull {
    tag: String,
}

pub async fn pull(State(state): State<AppState>, Json(body): Json<Pull>) -> impl IntoResponse {
    let (tx, rx) = mpsc::channel(32);
    let config = state.0.config.clone();
    let data = data_dir(&state).to_owned();
    tokio::spawn(async move {
        assistant::pull(&config, &data, &body.tag, tx).await;
    });
    events(rx)
}

pub async fn install(State(state): State<AppState>) -> impl IntoResponse {
    let (tx, rx) = mpsc::channel(32);
    let data = data_dir(&state).to_owned();
    tokio::spawn(async move {
        assistant::install(&data, tx).await;
    });
    events(rx)
}

/// Deletes one downloaded model. A body, not a path: a tag has colons and slashes in it.
pub async fn forget(State(state): State<AppState>, Json(body): Json<Pull>) -> Response {
    match assistant::forget(&state.0.config, &body.tag).await {
        Ok(()) => {
            Json(assistant::read_status(turn(&state), data_dir(&state)).await).into_response()
        }
        Err(error) => refused(StatusCode::BAD_REQUEST, error),
    }
}

pub async fn load(State(state): State<AppState>, Json(body): Json<Pull>) -> Response {
    hold(&state, &body.tag, true).await
}

pub async fn unload(State(state): State<AppState>, Json(body): Json<Pull>) -> Response {
    hold(&state, &body.tag, false).await
}

async fn hold(state: &AppState, tag: &str, load: bool) -> Response {
    // Loading a model is choosing it: it becomes the one in use, in place of a cloud model
    // or another local one. Unloading leaves the choice alone.
    let done = if load {
        assistant::choose(&state.0.config, data_dir(state), tag).await
    } else {
        // Letting go takes no context: that is only said when a model is loaded.
        assistant::hold(data_dir(state), tag, false, 0).await
    };
    match done {
        Ok(()) => Json(assistant::read_status(turn(state), data_dir(state)).await).into_response(),
        Err(error) => refused(StatusCode::BAD_REQUEST, error),
    }
}

/// What Irori's own Ollama has said lately, in the shape the log window reads.
pub async fn model_log(State(state): State<AppState>) -> Response {
    Json(serde_json::json!({ "lines": assistant::model_log(data_dir(&state)) })).into_response()
}

pub async fn uninstall(State(state): State<AppState>) -> Response {
    match assistant::uninstall(&state.0.config, data_dir(&state)).await {
        Ok(()) => {
            Json(assistant::read_status(turn(&state), data_dir(&state)).await).into_response()
        }
        Err(error) => refused(StatusCode::BAD_REQUEST, error),
    }
}

pub async fn transcript(State(state): State<AppState>, Path(scope): Path<String>) -> Response {
    match assistant::transcript(&state.0.db.path, &scope) {
        Ok((turns, context)) => {
            let turns: Vec<Stored> = turns
                .iter()
                .map(|turn| Stored {
                    role: turn.role.as_str(),
                    body: turn.body.clone(),
                })
                .collect();
            let pending = state.0.turns.pending(&scope).map(|pending| pending.view());
            Json(serde_json::json!({ "turns": turns, "pending": pending, "context": context }))
                .into_response()
        }
        Err(error) => refused(StatusCode::BAD_REQUEST, error),
    }
}

pub async fn clear(State(state): State<AppState>, Path(scope): Path<String>) -> Response {
    state.0.turns.stop(&scope);
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
            ChatEvent::Step(tool) => serde_json::json!({ "step": tool }),
            ChatEvent::Plan(plan) => serde_json::json!({ "plan": plan }),
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
        log: &state.0.log,
    }
}

fn data_dir(state: &AppState) -> &std::path::Path {
    state.0.db.path.parent().unwrap_or(&state.0.db.path)
}
