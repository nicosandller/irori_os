//! `GET /api/audit`: the activity log, for an owner (`docs/specs/access.md` §3).
//!
//! Owner-only for a cookie, and closed to every token: it names who did what in the home, and
//! a program is not the person to ask.

use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use irori_types::{Outcome, Timestamp};

use super::{AppState, query_value, refused};
use crate::audit::Query;

/// The newest rows first, narrowed by what the query names: `user`, `action`, `outcome`,
/// `since` (a time), `before` (a row's id, to read on from where a page ended) and `limit`.
pub async fn list(State(state): State<AppState>, RawQuery(query): RawQuery) -> Response {
    let named = |name: &str| query_value(query.as_deref(), name).filter(|value| !value.is_empty());
    let asked = (|| -> Result<Query, String> {
        Ok(Query {
            user: named("user"),
            action: named("action"),
            outcome: named("outcome")
                .map(|text| text.parse::<Outcome>())
                .transpose()?,
            since: named("since")
                .map(|text| text.parse::<Timestamp>().map_err(|e| e.to_string()))
                .transpose()?,
            before: named("before")
                .map(|text| {
                    text.parse::<u64>()
                        .map_err(|_| "before is the id of a row".to_owned())
                })
                .transpose()?,
            limit: named("limit")
                .map(|text| {
                    text.parse::<usize>()
                        .map_err(|_| "limit is a number".to_owned())
                })
                .transpose()?,
        })
    })();
    let asked = match asked {
        Ok(asked) => asked,
        Err(why) => return refused(StatusCode::UNPROCESSABLE_ENTITY, why),
    };
    let audit = state.0.audit.clone();
    match tokio::task::spawn_blocking(move || audit.list(&asked)).await {
        Ok(Ok(page)) => Json(page).into_response(),
        Ok(Err(why)) => refused(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("couldn't read the activity log: {why}"),
        ),
        Err(error) => refused(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("couldn't read the activity log: {error}"),
        ),
    }
}
