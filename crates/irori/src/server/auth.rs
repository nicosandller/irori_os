//! Who is asking, and whether they may (`docs/specs/config.md` §3.10).
//!
//! Irori asks who is there only once somebody has a password. Until then it is as it always
//! was: for the person in the room with it, and anyone who can reach it is looking after the
//! home. That is what keeps the first run a prompt and not a gate.
//!
//! Once it is locked, a request is somebody's by its session cookie, and one function,
//! [`needs`], says what each address asks of them. One table rather than a check in each
//! handler, so a route added later is closed until somebody decides otherwise.
//!
//! This is the sign-in for Irori's own page. Tokens for other programs, and scopes on them,
//! are the public API's (ROADMAP C16).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use irori_types::{Name, Role, User, UserId};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{AppState, UI_HEADER, refused};
use crate::config::{EditError, People, Refused};
use crate::db::Database;

/// The cookie a session is carried in.
const COOKIE: &str = "irori_session";

/// How long a sign-in lasts.
const SESSION_LIFE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// How many wrong passwords in a row before the next try has to wait.
const TRIES: u32 = 5;

/// How long it has to wait. Long enough that guessing is hopeless, short enough that a person
/// who mistyped five times is back in before they've found the config directory.
const LOCKOUT: Duration = Duration::from_secs(30);

/// Who a request is from, as far as Irori can tell. Put on every request the guard lets
/// through, for the handlers that care who is asking.
#[derive(Debug, Clone)]
pub struct Actor {
    /// The person signed in. `None` while Irori is open and nobody has been set up.
    pub user: Option<User>,
    /// Whether they may change how the home is set up.
    pub owner: bool,
}

impl Actor {
    /// Who a command is attributed to.
    pub fn user_id(&self) -> UserId {
        self.user
            .as_ref()
            .map_or_else(|| super::UNAUTHENTICATED.clone(), |user| user.id.clone())
    }
}

/// What an address asks of whoever calls it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Needs {
    /// Open to anyone who can reach Irori: the page itself, and what it takes to sign in.
    Nothing,
    /// Using the home: seeing it, and controlling devices.
    Use,
    /// Running the home: changing how it is set up.
    Run,
}

/// The one table of who may ask for what. Anything under `/api/` that isn't named is `Run`.
pub fn needs(method: &Method, path: &str) -> Needs {
    // The page, its files, and extensions' pages. Those load in a sandboxed frame with an
    // origin of its own, where no cookie is sent; they are static files, and what they show
    // comes through the shell.
    if !path.starts_with("/api/") && path != "/api" {
        return Needs::Nothing;
    }
    if matches!(path, "/api/health" | "/api/session" | "/api/setup") {
        return Needs::Nothing;
    }
    let Some(rest) = path.strip_prefix("/api/dev/") else {
        return Needs::Run;
    };
    if method == Method::GET {
        // An icon is an `<img>` on the sign-in page's own shell.
        return if rest.ends_with("/icon.svg") {
            Needs::Nothing
        } else {
            Needs::Use
        };
    }
    let using = rest == "command"
        // Asking the assistant, and stopping an answer. Setting it up is running the home.
        || rest == "assistant/turns"
        || (rest.starts_with("assistant/turns/") && rest.ends_with("/stop"))
        // A person's own name and password; the handler refuses anybody else's.
        || (method == Method::PATCH && rest.starts_with("users/"));
    if using { Needs::Use } else { Needs::Run }
}

#[derive(Debug, Clone)]
struct Session {
    user: UserId,
    /// Seconds since 1970.
    expires: u64,
}

#[derive(Debug, Clone, Copy)]
struct Attempts {
    failed: u32,
    last: Instant,
}

/// Sessions and sign-in attempts.
///
/// Sessions are kept in the database as well as in memory, so the Restart button doesn't sign
/// everybody out. Only the SHA-256 of a session's token is kept anywhere: somebody who reads
/// the database can't turn what they find into a cookie.
#[derive(Debug)]
pub struct Auth {
    db: PathBuf,
    sessions: Mutex<HashMap<String, Session>>,
    attempts: Mutex<HashMap<UserId, Attempts>>,
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

fn fingerprint(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

impl Auth {
    /// Reads the sessions that are still good. A database that can't be read starts with
    /// nobody signed in, which is the safe way to be wrong.
    pub fn open(db: &Database) -> Self {
        let auth = Self {
            db: db.path.clone(),
            sessions: Mutex::default(),
            attempts: Mutex::default(),
        };
        match auth.load() {
            Ok(sessions) => {
                *auth.sessions.lock().unwrap_or_else(PoisonError::into_inner) = sessions;
            }
            Err(error) => tracing::warn!(%error, "couldn't read who was signed in"),
        }
        auth
    }

    fn conn(&self) -> rusqlite::Result<Connection> {
        let conn = Connection::open(&self.db)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                token_hash TEXT PRIMARY KEY,
                user TEXT NOT NULL,
                expires INTEGER NOT NULL
            )",
        )?;
        Ok(conn)
    }

    fn load(&self) -> rusqlite::Result<HashMap<String, Session>> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM sessions WHERE expires <= ?1",
            [i64::try_from(now_seconds()).unwrap_or(i64::MAX)],
        )?;
        let mut rows = conn.prepare("SELECT token_hash, user, expires FROM sessions")?;
        let sessions = rows
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .filter_map(Result::ok)
            .filter_map(|(hash, user, expires)| {
                Some((
                    hash,
                    Session {
                        user: UserId::try_from(user).ok()?,
                        expires: u64::try_from(expires).ok()?,
                    },
                ))
            })
            .collect();
        Ok(sessions)
    }

    /// Signs `user` in, answering the token their cookie carries.
    pub fn begin(&self, user: &UserId) -> String {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .expect("the OS must be able to hand over random bytes for a session");
        let token = hex(&bytes);
        let hash = fingerprint(&token);
        let expires = now_seconds() + SESSION_LIFE.as_secs();
        let kept =
            self.conn().and_then(|conn| {
                conn.execute(
                "INSERT OR REPLACE INTO sessions (token_hash, user, expires) VALUES (?1, ?2, ?3)",
                rusqlite::params![hash, user.as_str(), i64::try_from(expires).unwrap_or(i64::MAX)],
            )
            });
        if let Err(error) = kept {
            // Still signed in for as long as this process runs.
            tracing::warn!(%error, "couldn't keep a sign-in; it will be lost when Irori restarts");
        }
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                hash,
                Session {
                    user: user.clone(),
                    expires,
                },
            );
        token
    }

    /// Whose session a token is, if it is anybody's still.
    fn whose(&self, token: &str) -> Option<UserId> {
        let sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        sessions
            .get(&fingerprint(token))
            .filter(|session| session.expires > now_seconds())
            .map(|session| session.user.clone())
    }

    fn forget(&self, keep: impl Fn(&str, &Session) -> bool) {
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let gone: Vec<String> = sessions
            .iter()
            .filter(|(hash, session)| !keep(hash, session))
            .map(|(hash, _)| hash.clone())
            .collect();
        for hash in &gone {
            sessions.remove(hash);
        }
        drop(sessions);
        if gone.is_empty() {
            return;
        }
        let dropped = self.conn().and_then(|conn| {
            for hash in &gone {
                conn.execute("DELETE FROM sessions WHERE token_hash = ?1", [hash])?;
            }
            Ok(())
        });
        if let Err(error) = dropped {
            tracing::warn!(%error, "couldn't forget a sign-in in the database");
        }
    }

    /// Ends one session.
    fn end(&self, token: &str) {
        let hash = fingerprint(token);
        self.forget(|other, _| other != hash);
    }

    /// Ends every session of `user`, apart from the one `except` carries: a changed password
    /// signs the other screens out, not the one it was changed on.
    pub fn end_all(&self, user: &UserId, except: Option<&str>) {
        let kept = except.map(fingerprint);
        self.forget(|hash, session| &session.user != user || kept.as_deref() == Some(hash));
    }

    /// How long `user` has to wait before trying a password again, if they do.
    fn must_wait(&self, user: &UserId) -> Option<Duration> {
        let attempts = self.attempts.lock().unwrap_or_else(PoisonError::into_inner);
        let tries = attempts.get(user)?;
        (tries.failed >= TRIES)
            .then(|| LOCKOUT.checked_sub(tries.last.elapsed()))
            .flatten()
    }

    fn tried(&self, user: &UserId, right: bool) {
        let mut attempts = self.attempts.lock().unwrap_or_else(PoisonError::into_inner);
        if right {
            attempts.remove(user);
            return;
        }
        let tries = attempts.entry(user.clone()).or_insert(Attempts {
            failed: 0,
            last: Instant::now(),
        });
        // A wait that has been sat out starts the count again.
        if tries.failed >= TRIES && tries.last.elapsed() >= LOCKOUT {
            tries.failed = 0;
        }
        tries.failed += 1;
        tries.last = Instant::now();
    }

    fn dismissed(&self) -> bool {
        self.conn()
            .and_then(|conn| {
                conn.query_row(
                    "SELECT 1 FROM meta WHERE key = 'welcome_dismissed'",
                    [],
                    |_| Ok(()),
                )
            })
            .is_ok()
    }

    fn dismiss(&self) -> rusqlite::Result<()> {
        self.conn()?.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('welcome_dismissed', '1')",
            [],
        )?;
        Ok(())
    }
}

/// A password's hash, as it's kept: argon2id with a salt of its own, in the PHC string form.
pub fn hash_password(password: &str) -> Result<String, String> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|e| e.to_string())?;
    let salt = SaltString::encode_b64(&salt).map_err(|e| e.to_string())?;
    argon2::Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| e.to_string())
}

fn right_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|hash| {
        argon2::Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

/// The session token a request carries, if it carries one.
pub fn token_of(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|cookies| cookies.split(';'))
        .filter_map(|cookie| cookie.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, token)| token)
}

fn cookie(token: &str, life: Duration) -> HeaderValue {
    // Not `Secure`: Irori is reached over plain http on the home's own network. `SameSite`
    // keeps another site from borrowing it, and `HttpOnly` keeps the page's own scripts out.
    HeaderValue::from_str(&format!(
        "{COOKIE}={token}; Path=/; Max-Age={}; HttpOnly; SameSite=Strict",
        life.as_secs()
    ))
    .unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// The cookie that carries a new session.
pub fn session_cookie(token: &str) -> HeaderValue {
    cookie(token, SESSION_LIFE)
}

/// Who a request is from: `None` when Irori is locked and it's nobody's.
fn actor(auth: &Auth, people: &People, headers: &HeaderMap) -> Option<Actor> {
    if !people.locked() {
        // Open: whoever can reach it is looking after the home. Attributed to the owner, if
        // one has been set up.
        return Some(Actor {
            user: people
                .users
                .iter()
                .find(|user| user.role.runs_the_home())
                .cloned(),
            owner: true,
        });
    }
    let user = token_of(headers)
        .and_then(|token| auth.whose(token))
        .and_then(|id| people.user(&id).cloned())?;
    Some(Actor {
        owner: user.role.runs_the_home(),
        user: Some(user),
    })
}

/// Lets a request through, or says why not. Over every route.
pub async fn guard(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let people = state.0.config.people().await;
    let who = actor(&state.0.auth, &people, request.headers());
    let needed = needs(request.method(), request.uri().path());
    if needed != Needs::Nothing {
        let Some(who) = &who else {
            return refused(
                StatusCode::UNAUTHORIZED,
                "sign in first: this home asks who is there".to_owned(),
            );
        };
        if needed == Needs::Run && !who.owner {
            return refused(
                StatusCode::FORBIDDEN,
                "only an owner can change how the home is set up".to_owned(),
            );
        }
        // A cookie is sent by the browser whoever wrote the page, so a change also has to
        // carry the header only Irori's own page sends (see `UI_HEADER`).
        let changes = !matches!(*request.method(), Method::GET | Method::HEAD);
        if people.locked()
            && changes
            && !request
                .headers()
                .get(UI_HEADER)
                .is_some_and(|value| value == "1")
        {
            return refused(
                StatusCode::FORBIDDEN,
                format!("a change needs the `{UI_HEADER}: 1` header, which only the page sends"),
            );
        }
    }
    // Open addresses get whoever it is too, or nobody: the session endpoints read it.
    request.extensions_mut().insert(who.unwrap_or(Actor {
        user: None,
        owner: false,
    }));
    next.run(request).await
}

/// What the page boots from.
#[derive(Debug, Serialize)]
struct SessionView {
    /// Whether Irori asks who is there.
    locked: bool,
    /// Who this browser is signed in as.
    user: Option<User>,
    /// Whether this request may change how the home is set up.
    owner: bool,
    setup: Setup,
    /// Who can sign in, for the sign-in page to offer by name. Only while locked.
    people: Vec<Person>,
}

#[derive(Debug, Serialize)]
struct Setup {
    /// Whether anybody has been set up as the owner.
    owner: bool,
    /// Whether the home has a time zone.
    place: bool,
    /// Whether the welcome was put off.
    dismissed: bool,
}

#[derive(Debug, Serialize)]
struct Person {
    id: UserId,
    name: Name,
}

async fn view(state: &AppState, who: &Actor) -> SessionView {
    let people = state.0.config.people().await;
    let home = state.0.core.place();
    let locked = people.locked();
    SessionView {
        locked,
        user: who.user.clone(),
        owner: who.owner,
        setup: Setup {
            owner: people.users.iter().any(|user| user.role.runs_the_home()),
            place: home.time_zone.is_some(),
            dismissed: state.0.auth.dismissed(),
        },
        people: if locked {
            people
                .users
                .iter()
                .map(|user| Person {
                    id: user.id.clone(),
                    name: user.name.clone(),
                })
                .collect()
        } else {
            Vec::new()
        },
    }
}

pub async fn session(State(state): State<AppState>, Extension(who): Extension<Actor>) -> Response {
    Json(view(&state, &who).await).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignIn {
    user: UserId,
    password: String,
}

/// Never the password.
impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignIn").field("user", &self.user).finish()
    }
}

/// Signs somebody in. A wrong name and a wrong password get the same answer, after the same
/// work, so neither says which it was.
pub async fn sign_in(State(state): State<AppState>, Json(ask): Json<SignIn>) -> Response {
    let auth = &state.0.auth;
    if let Some(wait) = auth.must_wait(&ask.user) {
        return refused(
            StatusCode::TOO_MANY_REQUESTS,
            format!(
                "too many wrong passwords; try again in {} seconds",
                wait.as_secs().max(1)
            ),
        );
    }
    let people = state.0.config.people().await;
    let hash = people
        .user(&ask.user)
        .and_then(|user| people.hashes.get(&user.id))
        .cloned();
    let password = ask.password;
    let right = tokio::task::spawn_blocking(move || match hash {
        Some(hash) => right_password(&password, &hash),
        None => {
            // Nobody by that name, or nobody with a password: do the work all the same.
            let _ = hash_password(&password);
            false
        }
    })
    .await
    .unwrap_or(false);
    auth.tried(&ask.user, right);
    let Some(user) = people.user(&ask.user).filter(|_| right).cloned() else {
        return refused(
            StatusCode::UNAUTHORIZED,
            "that name and password don't go together".to_owned(),
        );
    };
    let token = auth.begin(&user.id);
    tracing::info!(user = %user.id, "signed in");
    let who = Actor {
        owner: user.role.runs_the_home(),
        user: Some(user),
    };
    let mut response = Json(view(&state, &who).await).into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, cookie(&token, SESSION_LIFE));
    response
}

pub async fn sign_out(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = token_of(&headers) {
        state.0.auth.end(token);
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, cookie("", Duration::ZERO));
    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirstOwner {
    name: Name,
    /// Left out, or empty, for a home that doesn't ask who is there.
    #[serde(default)]
    password: Option<String>,
}

impl std::fmt::Debug for FirstOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirstOwner")
            .field("name", &self.name)
            .finish()
    }
}

/// Sets up the first owner. Open to anyone who can reach Irori, and only while there is
/// nobody: after that, people are added by an owner.
pub async fn set_up(State(state): State<AppState>, Json(ask): Json<FirstOwner>) -> Response {
    let password = ask.password.filter(|password| !password.is_empty());
    let hash = match password_hash(password).await {
        Ok(hash) => hash,
        Err(why) => return refused(StatusCode::UNPROCESSABLE_ENTITY, why),
    };
    let Some(id) = irori_types::user_id_from(ask.name.as_str()) else {
        return refused(
            StatusCode::UNPROCESSABLE_ENTITY,
            "that name has no letters or numbers to make an id from".to_owned(),
        );
    };
    let user = User {
        id,
        name: ask.name,
        role: Role::Owner,
    };
    let locking = hash.is_some();
    let made = state
        .0
        .config
        .edit_people(&state.0.core, |people| {
            if !people.users.is_empty() {
                return Err(Refused(
                    "this home already has an owner; people are added in Settings".to_owned(),
                ));
            }
            people.users.push(user.clone());
            if let Some(hash) = hash {
                people.hashes.insert(user.id.clone(), hash);
            }
            Ok(())
        })
        .await;
    if let Err(error) = made {
        return edit_failed(error);
    }
    tracing::info!(user = %user.id, locked = locking, "the home has an owner");
    let who = Actor {
        user: Some(user.clone()),
        owner: true,
    };
    // A password locks the home from here on, so the person who just set it is signed in
    // rather than shown the door.
    let token = locking.then(|| state.0.auth.begin(&user.id));
    let mut response = (StatusCode::CREATED, Json(view(&state, &who).await)).into_response();
    if let Some(token) = token {
        response
            .headers_mut()
            .insert(header::SET_COOKIE, cookie(&token, SESSION_LIFE));
    }
    response
}

/// Puts the welcome off: it isn't shown again, on this browser or any other.
pub async fn dismiss(State(state): State<AppState>) -> Response {
    match state.0.auth.dismiss() {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => refused(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}

/// Checks and hashes a password off the async runtime: hashing is meant to be slow.
pub async fn password_hash(password: Option<String>) -> Result<Option<String>, String> {
    let Some(password) = password else {
        return Ok(None);
    };
    irori_types::check_password(&password).map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|e| e.to_string())?
        .map(Some)
}

pub fn edit_failed(error: EditError) -> Response {
    match error {
        EditError::Refused(why) => refused(StatusCode::UNPROCESSABLE_ENTITY, why.0),
        EditError::Io(error) => refused(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("couldn't write it down: {error}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_says_who_may_ask_for_what() {
        use Needs::{Nothing, Run, Use};
        let cases = [
            (Method::GET, "/", Nothing),
            (Method::GET, "/settings", Nothing),
            (Method::GET, "/pages/automations/app.js", Nothing),
            (Method::GET, "/api/health", Nothing),
            (Method::POST, "/api/session", Nothing),
            (Method::POST, "/api/setup", Nothing),
            (Method::GET, "/api/dev/extensions/demo/icon.svg", Nothing),
            (Method::GET, "/api/dev/home", Use),
            (Method::GET, "/api/dev/users", Use),
            (Method::POST, "/api/dev/command", Use),
            (Method::POST, "/api/dev/assistant/turns", Use),
            (Method::POST, "/api/dev/assistant/turns/general/stop", Use),
            (Method::PATCH, "/api/dev/users/nico", Use),
            (Method::POST, "/api/dev/users", Run),
            (Method::DELETE, "/api/dev/users/nico", Run),
            (Method::PUT, "/api/dev/place", Run),
            (Method::POST, "/api/dev/restart", Run),
            (Method::PUT, "/api/dev/assistant", Run),
            (Method::POST, "/api/dev/apps/automations/rpc", Run),
            (Method::PUT, "/api/dev/extensions/mqtt/secrets", Run),
            (Method::POST, "/api/setup/dismiss", Run),
            // Nobody has named it, so it's closed.
            (Method::POST, "/api/something/new", Run),
            (Method::GET, "/api/something/new", Run),
        ];
        for (method, path, expected) in cases {
            assert_eq!(needs(&method, path), expected, "{method} {path}");
        }
    }

    #[test]
    fn a_password_is_kept_as_a_hash_that_only_it_opens() -> Result<(), String> {
        let hash = hash_password("correct horse")?;
        assert!(hash.starts_with("$argon2id$"), "{hash}");
        assert!(!hash.contains("correct"));
        assert!(right_password("correct horse", &hash));
        assert!(!right_password("battery staple", &hash));
        // The same password twice doesn't hash the same: each has a salt of its own.
        assert_ne!(hash, hash_password("correct horse")?);
        Ok(())
    }

    #[test]
    fn the_session_cookie_is_found_among_others() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("theme=dark; irori_session=abc123; other=1"),
        );
        assert_eq!(token_of(&headers), Some("abc123"));
        assert_eq!(token_of(&HeaderMap::new()), None);
    }
}
