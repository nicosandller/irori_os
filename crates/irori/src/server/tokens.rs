//! Access tokens for programs that aren't the page (`docs/specs/api.md` §2).
//!
//! An owner creates one here. The secret is in the response once and nowhere else: the
//! database keeps its SHA-256, the same way it keeps a session. The guard in `auth` asks
//! [`bearer_actor`] and [`extension_actor`] which requests that secret may make. What each
//! address allows is [`super::auth::token_access`], next to the cookie table.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use irori_types::{ApiScope, ExtensionId, Name, TokenId, UserId};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::auth::{self, Actor, Auth, TokenAccess, TokenGrant};
use super::{AppState, refused};
use crate::config::People;
use crate::db::Database;

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

/// One access token, as it's kept. The secret is not one of these fields: only its hash is,
/// and only as the map's key.
#[derive(Debug, Clone)]
struct AccessToken {
    id: TokenId,
    name: String,
    user: UserId,
    scopes: Vec<ApiScope>,
    extension: Option<ExtensionId>,
    /// Seconds since 1970.
    created: u64,
}

/// A secret just created, with the token it belongs to. `Debug` leaves the secret out.
struct Issued {
    secret: String,
    token: AccessToken,
}

impl std::fmt::Debug for Issued {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Issued")
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

/// Why a token wasn't created.
#[derive(Debug)]
enum IssueError {
    /// The home already holds [`MAX_TOKENS`].
    Full,
    /// Another token already has this id.
    Taken,
    /// The database didn't take it.
    Store(String),
}

/// How many tokens one home holds. Plenty for the programs a home runs, and a bound so a
/// bug that creates them in a loop can't fill the database.
const MAX_TOKENS: usize = 32;

fn view(token: &AccessToken) -> TokenView {
    TokenView {
        id: token.id.clone(),
        name: token.name.clone(),
        user: token.user.clone(),
        scopes: token.scopes.clone(),
        extension: token.extension.clone(),
        created: token.created,
    }
}

/// The tokens, in memory and in `tokens` beside `sessions`. Its own connection: the sign-in
/// store doesn't have to know they exist.
#[derive(Debug)]
pub(super) struct TokenStore {
    db: PathBuf,
    tokens: Mutex<HashMap<String, AccessToken>>,
}

impl TokenStore {
    /// Reads the tokens. A database that can't be read starts with none, which is the safe
    /// way to be wrong: a secret we can't check is not a way in.
    pub(super) fn open(db: &Database) -> Self {
        let store = Self {
            db: db.path.clone(),
            tokens: Mutex::default(),
        };
        match store.load() {
            Ok(tokens) => {
                *store.tokens.lock().unwrap_or_else(PoisonError::into_inner) = tokens;
            }
            Err(error) => tracing::warn!(%error, "couldn't read access tokens"),
        }
        store
    }

    fn conn(&self) -> rusqlite::Result<Connection> {
        let conn = Connection::open(&self.db)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tokens (
                token_hash TEXT PRIMARY KEY,
                id TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL,
                user TEXT NOT NULL,
                scopes TEXT NOT NULL,
                extension TEXT,
                created INTEGER NOT NULL
            )",
        )?;
        Ok(conn)
    }

    fn load(&self) -> rusqlite::Result<HashMap<String, AccessToken>> {
        let conn = self.conn()?;
        let mut rows = conn
            .prepare("SELECT token_hash, id, name, user, scopes, extension, created FROM tokens")?;
        let tokens = rows
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })?
            .filter_map(Result::ok)
            .filter_map(|(hash, id, name, user, scopes, extension, created)| {
                let scopes = serde_json::from_str::<Vec<ApiScope>>(&scopes).ok()?;
                let extension = match extension {
                    Some(id) => Some(ExtensionId::try_from(id).ok()?),
                    None => None,
                };
                Some((
                    hash,
                    AccessToken {
                        id: TokenId::try_from(id).ok()?,
                        name,
                        user: UserId::try_from(user).ok()?,
                        scopes,
                        extension,
                        created: u64::try_from(created).ok()?,
                    },
                ))
            })
            .collect();
        Ok(tokens)
    }

    /// Creates a token and returns its secret, once. The database is written before the
    /// secret is remembered here, so a secret that couldn't be kept is never handed out.
    fn issue(
        &self,
        id: TokenId,
        name: String,
        user: UserId,
        scopes: Vec<ApiScope>,
        extension: Option<ExtensionId>,
    ) -> Result<Issued, IssueError> {
        let mut tokens = self.tokens.lock().unwrap_or_else(PoisonError::into_inner);
        if tokens.len() >= MAX_TOKENS {
            return Err(IssueError::Full);
        }
        if tokens.values().any(|token| token.id == id) {
            return Err(IssueError::Taken);
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .expect("the OS must be able to hand over random bytes for an access token");
        let secret = format!("irori_{}", auth::hex(&bytes));
        let hash = auth::fingerprint(&secret);
        let token = AccessToken {
            id: id.clone(),
            name,
            user,
            scopes,
            extension,
            created: auth::now_seconds(),
        };
        let scopes_json = serde_json::to_string(&token.scopes)
            .map_err(|error| IssueError::Store(error.to_string()))?;
        let stored = self.conn().and_then(|conn| {
            conn.execute(
                "INSERT INTO tokens (token_hash, id, name, user, scopes, extension, created)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    hash,
                    token.id.as_str(),
                    token.name,
                    token.user.as_str(),
                    scopes_json,
                    token.extension.as_ref().map(ExtensionId::as_str),
                    i64::try_from(token.created).unwrap_or(i64::MAX),
                ],
            )
        });
        if let Err(error) = stored {
            // A unique-id race against a row this process didn't load. Rare, and the same
            // answer as one we already know about.
            let message = error.to_string();
            if message.contains("UNIQUE") {
                return Err(IssueError::Taken);
            }
            return Err(IssueError::Store(message));
        }
        tokens.insert(hash, token.clone());
        Ok(Issued { secret, token })
    }

    /// Every token, oldest first. No secrets.
    fn list(&self) -> Vec<AccessToken> {
        let mut tokens: Vec<_> = self
            .tokens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        tokens.sort_by(|a, b| {
            a.created
                .cmp(&b.created)
                .then_with(|| a.id.as_str().cmp(b.id.as_str()))
        });
        tokens
    }

    /// The token `secret` is, if it is one we issued.
    fn find(&self, secret: &str) -> Option<AccessToken> {
        let hash = auth::fingerprint(secret);
        self.tokens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&hash)
            .cloned()
    }

    /// Revokes the token with this id. `Ok(false)` when there wasn't one. `Err` when the
    /// database didn't forget it: the token stays, so a restart is not what brings it back.
    fn revoke(&self, id: &TokenId) -> Result<bool, String> {
        let hash = {
            let tokens = self.tokens.lock().unwrap_or_else(PoisonError::into_inner);
            tokens
                .iter()
                .find(|(_, token)| &token.id == id)
                .map(|(hash, _)| hash.clone())
        };
        let Some(hash) = hash else {
            return Ok(false);
        };
        self.drop_tokens(|other, _| other != hash)?;
        Ok(true)
    }

    /// Revokes every token that belongs to `user`.
    pub(super) fn revoke_user(&self, user: &UserId) -> Result<(), String> {
        self.drop_tokens(|_, token| &token.user != user)
    }

    /// Revokes every token. Setting the home up again does this: the welcome is also how a
    /// forgotten password is replaced, and tokens from before that must not keep working.
    pub(super) fn revoke_all(&self) -> Result<(), String> {
        self.drop_tokens(|_, _| false)
    }

    fn drop_tokens(&self, keep: impl Fn(&str, &AccessToken) -> bool) -> Result<(), String> {
        let mut tokens = self.tokens.lock().unwrap_or_else(PoisonError::into_inner);
        let gone: Vec<String> = tokens
            .iter()
            .filter(|(hash, token)| !keep(hash, token))
            .map(|(hash, _)| hash.clone())
            .collect();
        if gone.is_empty() {
            return Ok(());
        }
        // One transaction, so a failure leaves every row where it was. The hashes stay in
        // memory in that case: forgetting them here would make a restart the only way back,
        // and until then the token would already be gone from the check.
        let dropped = self.conn().and_then(|mut conn| {
            let transaction = conn.transaction()?;
            for hash in &gone {
                transaction.execute("DELETE FROM tokens WHERE token_hash = ?1", [hash])?;
            }
            transaction.commit()
        });
        if let Err(error) = dropped {
            tracing::warn!(%error, "couldn't revoke an access token");
            return Err(error.to_string());
        }
        for hash in &gone {
            tokens.remove(hash);
        }
        Ok(())
    }
}

pub async fn list(State(state): State<AppState>) -> Json<Vec<TokenView>> {
    Json(state.0.auth.tokens.list().iter().map(view).collect())
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
        .tokens
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
            format!("a home holds at most {MAX_TOKENS} tokens"),
        ),
        Err(IssueError::Store(why)) => refused(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("couldn't write it down: {why}"),
        ),
    }
}

pub async fn revoke(State(state): State<AppState>, Path(id): Path<TokenId>) -> Response {
    match state.0.auth.tokens.revoke(&id) {
        Ok(true) => {
            state.0.sockets.close_token(&id);
            tracing::info!(token = %id, "an access token was revoked");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => refused(
            StatusCode::NOT_FOUND,
            format!("there's no token called `{id}`"),
        ),
        Err(why) => refused(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("couldn't write it down: {why}"),
        ),
    }
}

/// Whether `Authorization` is an attempt to present a token.
///
/// `Bearer`, a header with no scheme (`nope`), and a value that isn't text are tokens:
/// a bad one is refused, and is not the cookie that came along too. Another named scheme,
/// such as `Basic` from a reverse proxy, is not a token and falls through to the cookie.
pub(super) fn authorization_is_a_token(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return false;
    };
    let Ok(text) = value.to_str() else {
        return true;
    };
    match text.split_once(' ') {
        Some((scheme, _)) => scheme.eq_ignore_ascii_case("bearer"),
        None => true,
    }
}

/// The bearer secret a request presents, or why the header isn't one.
fn presented(headers: &HeaderMap) -> Result<&str, ()> {
    let value = headers.get(header::AUTHORIZATION).ok_or(())?;
    let text = value.to_str().map_err(|_| ())?;
    let Some((scheme, secret)) = text.split_once(' ') else {
        return Err(());
    };
    if !scheme.eq_ignore_ascii_case("bearer")
        || secret.is_empty()
        || secret.chars().any(char::is_whitespace)
    {
        return Err(());
    }
    Ok(secret)
}

/// Whether this token still stands for someone. A locked home only honours a token whose
/// person still has a password. An open home honours it either way (`docs/specs/api.md` §2).
fn token_actor(people: &People, token: &AccessToken) -> Option<Actor> {
    let user = people.user(&token.user).cloned();
    if people.locked() && (user.is_none() || !people.hashes.contains_key(&token.user)) {
        return None;
    }
    Some(Actor {
        user,
        owner: false,
        token: Some(TokenGrant {
            id: token.id.clone(),
            extension: token.extension.clone(),
        }),
    })
}

pub(super) fn extension_actor(
    auth: &Auth,
    people: &People,
    headers: &HeaderMap,
) -> Result<Actor, Box<Response>> {
    let denied = || {
        Box::new(refused(
            StatusCode::UNAUTHORIZED,
            "an extension connects with the token made for it".to_owned(),
        ))
    };
    let secret = presented(headers).map_err(|()| denied())?;
    let Some(token) = auth.tokens.find(secret) else {
        return Err(denied());
    };
    let Some(who) = token_actor(people, &token) else {
        return Err(denied());
    };
    if token.extension.is_none() {
        return Err(Box::new(refused(
            StatusCode::FORBIDDEN,
            "this token isn't for connecting an extension".to_owned(),
        )));
    }
    Ok(who)
}

pub(super) fn bearer_actor(
    auth: &Auth,
    people: &People,
    headers: &HeaderMap,
    method: &Method,
    path: &str,
) -> Result<Actor, Box<Response>> {
    let denied = || {
        Box::new(refused(
            StatusCode::UNAUTHORIZED,
            "that access token isn't one Irori issued".to_owned(),
        ))
    };
    let secret = presented(headers).map_err(|()| denied())?;
    let Some(token) = auth.tokens.find(secret) else {
        return Err(denied());
    };
    let Some(who) = token_actor(people, &token) else {
        return Err(denied());
    };
    if token.extension.is_some() {
        return Err(Box::new(refused(
            StatusCode::FORBIDDEN,
            "this token is only for connecting its extension".to_owned(),
        )));
    }
    match auth::token_access(method, path) {
        TokenAccess::Scopes(need) => {
            if let Some(missing) = missing_scope(need, &token.scopes) {
                return Err(Box::new(refused(
                    StatusCode::FORBIDDEN,
                    format!("this token doesn't include {missing}"),
                )));
            }
        }
        TokenAccess::Closed => {
            return Err(Box::new(refused(
                StatusCode::FORBIDDEN,
                "a token can't change how the home is set up".to_owned(),
            )));
        }
        TokenAccess::Open => {}
    }
    Ok(who)
}

/// The first required scope `have` does not contain.
fn missing_scope(need: &[ApiScope], have: &[ApiScope]) -> Option<ApiScope> {
    need.iter().copied().find(|scope| !have.contains(scope))
}

#[cfg(test)]
mod tests {
    use axum::http::Method;

    use irori_types::ApiScope;

    use super::super::auth::{TokenAccess, token_access};
    use super::missing_scope;

    fn scopes(method: &Method, path: &str) -> &'static [ApiScope] {
        match token_access(method, path) {
            TokenAccess::Scopes(scopes) => scopes,
            other => panic!("{method} {path} should name scopes, not {other:?}"),
        }
    }

    #[test]
    fn a_token_can_read_and_control_and_nothing_else() {
        assert_eq!(scopes(&Method::GET, "/api/states"), &[ApiScope::StatesRead]);
        assert_eq!(
            scopes(&Method::GET, "/api/states?x=1"),
            &[ApiScope::StatesRead]
        );
        assert_eq!(
            scopes(&Method::GET, "/api/home").len(),
            2,
            "the whole picture needs the registry and the states"
        );
        assert_eq!(
            scopes(&Method::HEAD, "/api/home"),
            scopes(&Method::GET, "/api/home"),
            "HEAD is not a read for a cookie, and it still needs the home's scopes"
        );
        assert_eq!(
            scopes(&Method::POST, "/api/command"),
            &[ApiScope::ServicesCall]
        );
        assert_eq!(scopes(&Method::GET, "/api/ws").len(), 3);
        assert_eq!(
            scopes(&Method::GET, "/api/history/light.hall"),
            &[ApiScope::HistoryRead]
        );
        assert_eq!(
            scopes(&Method::GET, "/api/history"),
            &[ApiScope::HistoryRead]
        );
        assert_eq!(
            scopes(&Method::GET, "/api/devices"),
            &[ApiScope::RegistryRead]
        );
        // A single device is not the registry list.
        assert_eq!(
            token_access(&Method::GET, "/api/devices/lamp"),
            TokenAccess::Closed
        );
        assert_eq!(token_access(&Method::GET, "/api/health"), TokenAccess::Open);
        assert_eq!(
            token_access(&Method::GET, "/api/extensions/demo/icon.svg"),
            TokenAccess::Open
        );
        // Setup is not a scope. A token is refused these, including reads the page makes
        // that the scope list does not name.
        for (method, path) in [
            (Method::POST, "/api/tokens"),
            (Method::DELETE, "/api/tokens/tablet"),
            (Method::POST, "/api/restart"),
            (Method::PUT, "/api/floorplan"),
            (Method::POST, "/api/areas"),
            (Method::GET, "/api/system"),
            (Method::GET, "/api/extensions/demo/log"),
        ] {
            assert_eq!(
                token_access(&method, path),
                TokenAccess::Closed,
                "{method} {path}"
            );
        }
    }

    #[test]
    fn the_missing_scope_is_named() {
        let need = scopes(&Method::GET, "/api/home");
        assert_eq!(
            missing_scope(need, &[ApiScope::RegistryRead]),
            Some(ApiScope::StatesRead)
        );
        assert_eq!(missing_scope(need, need), None);
    }
}
