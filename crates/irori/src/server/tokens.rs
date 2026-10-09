//! Access tokens for programs that aren't the page (`docs/specs/api.md` §2).
//!
//! An owner creates one here. The secret is in the response once and nowhere else: the
//! database keeps its SHA-256, the same way it keeps a session.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use irori_types::{ApiScope, ExtensionId, Name, TokenId, UserId};
use serde::{Deserialize, Serialize};

use super::auth::{self, Actor, IssueError};
use super::{AppState, refused};

/// What the page sends to make a token. Exactly one of `scopes` and `extension`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NewToken {
    name: String,
    #[serde(default)]
    scopes: Vec<ApiScope>,
    #[serde(default)]
    extension: Option<ExtensionId>,
}

/// A token as the page lists it. No secret.
#[derive(Debug, Serialize)]
pub(super) struct TokenView {
    id: TokenId,
    name: String,
    user: UserId,
    scopes: Vec<ApiScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    extension: Option<ExtensionId>,
    created: u64,
}

#[derive(Debug, Serialize)]
struct Created {
    #[serde(flatten)]
    token: TokenView,
    secret: String,
}

fn view(token: &auth::AccessToken) -> TokenView {
    TokenView {
        id: token.id.clone(),
        name: token.name.clone(),
        user: token.user.clone(),
        scopes: token.scopes.clone(),
        extension: token.extension.clone(),
        created: token.created,
    }
}

pub async fn list(State(state): State<AppState>) -> Json<Vec<TokenView>> {
    Json(state.0.auth.list().iter().map(view).collect())
}

pub async fn create(
    State(state): State<AppState>,
    Extension(who): Extension<Actor>,
    Json(ask): Json<NewToken>,
) -> Response {
    let name = match Name::try_from(ask.name) {
        Ok(name) => name,
        Err(error) => return refused(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
    };
    let Some(id) = irori_types::token_id_from(name.as_str()) else {
        return refused(
            StatusCode::UNPROCESSABLE_ENTITY,
            "that name has no letters or numbers to make an id from".to_owned(),
        );
    };
    let (scopes, extension) = match (ask.scopes.is_empty(), ask.extension) {
        (false, Some(_)) => {
            return refused(
                StatusCode::UNPROCESSABLE_ENTITY,
                "a token is either for a program or for an extension, not both".to_owned(),
            );
        }
        (true, None) => {
            return refused(
                StatusCode::UNPROCESSABLE_ENTITY,
                "a token needs a scope, or the extension it connects".to_owned(),
            );
        }
        (false, None) => {
            let mut scopes = Vec::with_capacity(ask.scopes.len());
            for scope in ask.scopes {
                if scopes.contains(&scope) {
                    return refused(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        format!("scopes lists `{scope}` more than once"),
                    );
                }
                scopes.push(scope);
            }
            (scopes, None)
        }
        (true, Some(extension)) => {
            if let Err(why) = state.0.host.connects_in(&extension) {
                return refused(StatusCode::UNPROCESSABLE_ENTITY, why);
            }
            (Vec::new(), Some(extension))
        }
    };
    match state
        .0
        .auth
        .issue(id, name.to_string(), who.user_id(), scopes, extension)
    {
        Ok(issued) => {
            tracing::info!(token = %issued.token.id, "an access token was created");
            (
                StatusCode::CREATED,
                Json(Created {
                    token: view(&issued.token),
                    secret: issued.secret,
                }),
            )
                .into_response()
        }
        Err(IssueError::Taken) => refused(
            StatusCode::CONFLICT,
            "there's already a token with that name".to_owned(),
        ),
        Err(IssueError::Full) => refused(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("a home holds at most {MAX} tokens", MAX = 32),
        ),
        Err(IssueError::Store(why)) => refused(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("couldn't write it down: {why}"),
        ),
    }
}

pub async fn revoke(State(state): State<AppState>, Path(id): Path<TokenId>) -> Response {
    if state.0.auth.revoke(&id) {
        tracing::info!(token = %id, "an access token was revoked");
        StatusCode::NO_CONTENT.into_response()
    } else {
        refused(
            StatusCode::NOT_FOUND,
            format!("there's no token called `{id}`"),
        )
    }
}
