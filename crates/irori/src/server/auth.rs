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
//! This is the sign-in for Irori's own page. Tokens for other programs are the public API's
//! (`docs/specs/api.md`): the guard below asks `tokens` to check a bearer, because both
//! arrive on the same requests. What each address allows, for a cookie and for a token, is
//! [`token_access`] and [`needs`].

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
use irori_types::{ApiScope, ExtensionId, Name, Role, TokenId, User, UserId};
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

/// The most people trying passwords that are kept count of at once. Far more than a home has
/// people or devices; past it, new ones wait until old ones are forgotten.
const MAX_TRIERS: usize = 1024;

/// How long somebody who has stopped trying is remembered.
const FORGET_AFTER: Duration = Duration::from_secs(10 * 60);

/// Whose password is being tried, and from where. Counted together, so somebody guessing from
/// one machine can't make the owner wait on another. A name that isn't anybody's is counted
/// as nobody, so made-up names can't fill the count.
pub type Trier = (Option<UserId>, Option<std::net::IpAddr>);

/// Where a request came from, when the server knows. Put on every request by the guard.
#[derive(Debug, Clone, Copy)]
pub struct Client(pub Option<std::net::IpAddr>);

/// Who a request is from, as far as Irori can tell. Put on every request the guard lets
/// through, for the handlers that care who is asking.
#[derive(Debug, Clone)]
pub struct Actor {
    /// The person signed in. `None` while Irori is open and nobody has been set up.
    pub user: Option<User>,
    /// Whether they may change how the home is set up. A token is never an owner: setup
    /// stays on the page (`docs/specs/api.md` §2).
    pub owner: bool,
    /// The access token this request presented, when it presented one.
    pub token: Option<TokenGrant>,
}

/// An access token a request presented. The secret is not here; this is who it is.
#[derive(Debug, Clone)]
pub struct TokenGrant {
    pub id: TokenId,
    /// Set when the token exists only to connect this extension.
    pub extension: Option<ExtensionId>,
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
    cookie_needs(method, strip_query(path))
}

fn cookie_needs(method: &Method, path: &str) -> Needs {
    // The page, its files, and extensions' pages. Those load in a sandboxed frame with an
    // origin of its own, where no cookie is sent; they are static files, and what they show
    // comes through the shell.
    if !path.starts_with("/api/") && path != "/api" {
        return Needs::Nothing;
    }
    if matches!(path, "/api/health" | "/api/session" | "/api/setup") {
        return Needs::Nothing;
    }
    let Some(rest) = path.strip_prefix("/api/") else {
        return Needs::Run;
    };
    // Tokens, and the socket an extension dials in on, are how the home is set up. A person
    // who isn't an owner doesn't create either. The guard still refuses a token that tries
    // to call them: a token is not an owner.
    if rest == "tokens" || rest.starts_with("tokens/") || rest == "extension" {
        return Needs::Run;
    }
    if method == Method::GET {
        // An icon is an `<img>` on the sign-in page's own shell.
        if rest.ends_with("/icon.svg") {
            return Needs::Nothing;
        }
        // A path nobody has named is closed. A new read has to be added to `named_read`.
        return if named_read(rest) {
            Needs::Use
        } else {
            Needs::Run
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

/// A GET that reads the home, as opposed to one that runs it.
///
/// The list is the routes the page already fetches. A path that isn't here is `Run` until
/// somebody decides a signed-in person may read it.
fn named_read(rest: &str) -> bool {
    matches!(
        rest,
        "home"
            | "users"
            | "place"
            | "ws"
            | "devices"
            | "entities"
            | "states"
            | "areas"
            | "floors"
            | "floorplan"
            | "extensions"
            | "catalog"
            | "apps"
            | "system"
            | "system/usage"
            | "system/log"
            | "serial-ports"
            | "assistant"
            | "assistant/log"
    ) || rest.starts_with("history/")
        || (rest.starts_with("extensions/") && rest.ends_with("/log"))
        || rest.starts_with("assistant/turns/")
        || rest.starts_with("assistant/transcript/")
}

fn strip_query(path: &str) -> &str {
    path.split_once('?').map_or(path, |(path, _)| path)
}

/// What a token may do at an address. A cookie is [`Needs`]; a token is this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TokenAccess {
    /// Health, session, setup, icons, and the page: open to anyone who can reach Irori.
    Open,
    /// The token must hold every scope.
    Scopes(&'static [ApiScope]),
    /// Setup, logs, and any path the scope list does not name.
    Closed,
}

const SCOPE_HOME: &[ApiScope] = &[ApiScope::RegistryRead, ApiScope::StatesRead];
const SCOPE_STATES: &[ApiScope] = &[ApiScope::StatesRead];
const SCOPE_HISTORY: &[ApiScope] = &[ApiScope::HistoryRead];
const SCOPE_REGISTRY: &[ApiScope] = &[ApiScope::RegistryRead];
const SCOPE_COMMAND: &[ApiScope] = &[ApiScope::ServicesCall];
const SCOPE_LIVE: &[ApiScope] = &[
    ApiScope::RegistryRead,
    ApiScope::StatesRead,
    ApiScope::EventsRead,
];

/// What a token may call (`docs/specs/api.md` §2.1).
///
/// A path in the scope list needs those scopes even where a cookie is judged differently.
/// `HEAD` is not a read for a cookie, so `HEAD /api/home` is [`Needs::Run`] there and still
/// the home's scopes here. `/api/ws` asks for the registry, the states, and events. Anything
/// the list does not name is [`TokenAccess::Open`] only when a cookie would be
/// [`Needs::Nothing`], and [`TokenAccess::Closed`] otherwise.
pub(super) fn token_access(method: &Method, path: &str) -> TokenAccess {
    let path = strip_query(path);
    let reading = method == Method::GET || method == Method::HEAD;
    let scopes = if path == "/api/ws" {
        Some(SCOPE_LIVE)
    } else if method == Method::POST && path == "/api/command" {
        Some(SCOPE_COMMAND)
    } else if reading && path == "/api/home" {
        Some(SCOPE_HOME)
    } else if reading && path == "/api/states" {
        Some(SCOPE_STATES)
    } else if reading && (path == "/api/history" || path.starts_with("/api/history/")) {
        Some(SCOPE_HISTORY)
    } else if reading
        && matches!(
            path,
            "/api/devices"
                | "/api/entities"
                | "/api/areas"
                | "/api/floors"
                | "/api/floorplan"
                | "/api/extensions"
                | "/api/apps"
        )
    {
        Some(SCOPE_REGISTRY)
    } else {
        None
    };
    match scopes {
        Some(scopes) => TokenAccess::Scopes(scopes),
        None if cookie_needs(method, path) == Needs::Nothing => TokenAccess::Open,
        None => TokenAccess::Closed,
    }
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
    /// Access tokens. The secret itself is never here (`server/tokens.rs`).
    pub(super) tokens: super::tokens::TokenStore,
    attempts: Mutex<HashMap<Trier, Attempts>>,
}

pub(super) fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

pub(super) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

pub(super) fn fingerprint(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

impl Auth {
    /// Reads the sessions that are still good. A database that can't be read starts with
    /// nobody signed in, which is the safe way to be wrong.
    pub fn open(db: &Database) -> Self {
        let auth = Self {
            db: db.path.clone(),
            sessions: Mutex::default(),
            tokens: super::tokens::TokenStore::open(db),
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
        crate::db::wait_when_busy(&conn)?;
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
        let hash = fingerprint(token);
        let found = {
            let sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
            sessions.get(&hash).cloned()
        };
        let session = found?;
        if session.expires > now_seconds() {
            return Some(session.user);
        }
        // Run out: it goes, and so does every other one that has, here and in the database.
        let now = now_seconds();
        self.forget(|_, session| session.expires > now);
        None
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

    /// The hash of the cookie's session, when that cookie is still a sign-in.
    pub(super) fn live_session(&self, headers: &HeaderMap) -> Option<String> {
        let token = token_of(headers)?;
        self.whose(token)?;
        Some(fingerprint(token))
    }

    /// Ends every session of `user`, apart from the one `except` carries: a changed password
    /// signs the other screens out, not the one it was changed on.
    pub fn end_all(&self, user: &UserId, except: Option<&str>) {
        let kept = except.map(fingerprint);
        self.forget(|hash, session| &session.user != user || kept.as_deref() == Some(hash));
    }

    /// Counts a try at `who`'s password, before the password is looked at, or says how long
    /// they have to wait. Counted first so that many tries sent at once are each counted: a
    /// check made before the slow work and a count made after it would let them all through.
    pub fn try_now(&self, who: &Trier) -> Result<(), Duration> {
        let mut attempts = self.attempts.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(tries) = attempts.get_mut(who) {
            if tries.failed >= TRIES {
                match LOCKOUT.checked_sub(tries.last.elapsed()) {
                    Some(wait) => return Err(wait),
                    // A wait that has been sat out starts the count again.
                    None => tries.failed = 0,
                }
            }
            tries.failed += 1;
            tries.last = Instant::now();
            return Ok(());
        }
        if attempts.len() >= MAX_TRIERS {
            // Whoever has been quiet for long enough is forgotten to make room.
            attempts.retain(|_, tries| tries.last.elapsed() < FORGET_AFTER);
            if attempts.len() >= MAX_TRIERS {
                return Err(LOCKOUT);
            }
        }
        attempts.insert(
            who.clone(),
            Attempts {
                failed: 1,
                last: Instant::now(),
            },
        );
        Ok(())
    }

    /// The password was right: what was counted against `who` is forgotten.
    pub fn got_in(&self, who: &Trier) {
        self.attempts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(who);
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

/// Whether this server is serving over https (`--tls`). Set once, as it starts.
static OVER_TLS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Says whether cookies are for https only from here on.
pub fn over_tls(tls: bool) {
    OVER_TLS.store(tls, std::sync::atomic::Ordering::Relaxed);
}

fn cookie(token: &str, life: Duration) -> HeaderValue {
    // `SameSite` keeps another site from borrowing it, and `HttpOnly` keeps the page's own
    // scripts out. `Secure` when this server is https, so the browser never sends it in the
    // clear; over plain http it can't be, or the browser wouldn't send it at all.
    let secure = if OVER_TLS.load(std::sync::atomic::Ordering::Relaxed) {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{COOKIE}={token}; Path=/; Max-Age={}; HttpOnly; SameSite=Strict{secure}",
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
            token: None,
        });
    }
    // Somebody whose password has since been taken out of the files is nobody: a session is
    // only as good as the password it was begun with.
    let user = token_of(headers)
        .and_then(|token| auth.whose(token))
        .filter(|id| people.hashes.contains_key(id))
        .and_then(|id| people.user(&id).cloned())?;
    Some(Actor {
        owner: user.role.runs_the_home(),
        user: Some(user),
        token: None,
    })
}

/// Lets a request through, or says why not. Over every route.
///
/// A bearer token is that token, even when a cookie came along too. Any other
/// `Authorization` scheme is the page's cookie, so a proxy's `Basic` header doesn't
/// lock the page out. An extension's socket takes only the token made for that extension.
pub async fn guard(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let people = state.0.config.people().await;
    let who = if path == "/api/extension" {
        match super::tokens::extension_actor(&state.0.auth, &people, request.headers()) {
            Ok(who) => Some(who),
            Err(response) => return *response,
        }
    } else if super::tokens::authorization_is_a_token(request.headers()) {
        match super::tokens::bearer_actor(
            &state.0.auth,
            &people,
            request.headers(),
            request.method(),
            &path,
        ) {
            Ok(who) => Some(who),
            Err(response) => return *response,
        }
    } else {
        let who = actor(&state.0.auth, &people, request.headers());
        let needed = needs(request.method(), &path);
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
            // carry the header only Irori's own page sends (see `UI_HEADER`). A bearer token
            // is a secret the program was given, so it doesn't.
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
                    format!(
                        "a change needs the `{UI_HEADER}: 1` header, which only the page sends"
                    ),
                );
            }
        }
        who
    };
    let from = request
        .extensions()
        .get::<axum::extract::ConnectInfo<crate::tls::ClientAddr>>()
        .map(|info| info.0.0.ip());
    request.extensions_mut().insert(Client(from));
    // Open addresses get whoever it is too, or nobody: the session endpoints read it.
    request.extensions_mut().insert(who.unwrap_or(Actor {
        user: None,
        owner: false,
        token: None,
    }));
    next.run(request).await
}

/// What the page boots from. It says nothing of who lives here: a home may be reached from
/// further away than its own network, and the names of its people are not for whoever asks.
#[derive(Debug, Serialize)]
struct SessionView {
    /// Whether Irori asks who is there.
    locked: bool,
    /// Who this browser is signed in as.
    user: Option<User>,
    /// Whether this request may change how the home is set up.
    owner: bool,
    setup: Setup,
}

#[derive(Debug, Serialize)]
struct Setup {
    /// Whether the home has an owner who can sign in. Until it does, the welcome is shown.
    owner: bool,
    /// Whether the home has a time zone.
    place: bool,
}

async fn view(state: &AppState, who: &Actor) -> SessionView {
    let people = state.0.config.people().await;
    let home = state.0.core.place();
    SessionView {
        locked: people.locked(),
        user: who.user.clone(),
        owner: who.owner,
        setup: Setup {
            owner: people.owned(),
            place: home.time_zone.is_some(),
        },
    }
}

pub async fn session(State(state): State<AppState>, Extension(who): Extension<Actor>) -> Response {
    Json(view(&state, &who).await).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignIn {
    /// Their name as they type it, or their id.
    user: String,
    password: String,
}

/// Never the password.
impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignIn").field("user", &self.user).finish()
    }
}

fn too_many(wait: Duration) -> Response {
    refused(
        StatusCode::TOO_MANY_REQUESTS,
        format!(
            "too many wrong passwords; try again in {} seconds",
            wait.as_secs().max(1)
        ),
    )
}

/// Whether `password` is the one `hash` was made from, worked out off the async runtime. With
/// no hash to check against the work is done all the same, so how long the answer takes says
/// nothing about whether there was one.
async fn right_for(password: String, hash: Option<String>) -> bool {
    tokio::task::spawn_blocking(move || match hash {
        Some(hash) => right_password(&password, &hash),
        None => {
            let _ = hash_password(&password);
            false
        }
    })
    .await
    .unwrap_or(false)
}

/// Checks the password somebody already has, before they change it: counted like a sign-in,
/// so a session left open isn't a way to guess at leisure. Answers the refusal, if there is one.
pub async fn check_current(
    auth: &Auth,
    trier: &Trier,
    password: Option<String>,
    hash: String,
) -> Option<Response> {
    let Some(password) = password else {
        return Some(refused(
            StatusCode::FORBIDDEN,
            "type your current password to change it".to_owned(),
        ));
    };
    if let Err(wait) = auth.try_now(trier) {
        return Some(too_many(wait));
    }
    if right_for(password, Some(hash)).await {
        auth.got_in(trier);
        None
    } else {
        Some(refused(
            StatusCode::FORBIDDEN,
            "that isn't your current password".to_owned(),
        ))
    }
}

/// Signs somebody in. A wrong name and a wrong password get the same answer, after the same
/// work, so neither says which it was.
pub async fn sign_in(
    State(state): State<AppState>,
    Extension(Client(from)): Extension<Client>,
    Json(ask): Json<SignIn>,
) -> Response {
    let auth = &state.0.auth;
    let people = state.0.config.people().await;
    // By name, however it's capitalised, or by id. Nobody is offered a list to pick from.
    let typed = ask.user.trim().to_lowercase();
    let found = people
        .users
        .iter()
        .find(|user| user.id.as_str() == typed || user.name.as_str().trim().to_lowercase() == typed)
        .cloned();
    let trier: Trier = (found.as_ref().map(|user| user.id.clone()), from);
    if let Err(wait) = auth.try_now(&trier) {
        return too_many(wait);
    }
    let hash = found
        .as_ref()
        .and_then(|user| people.hashes.get(&user.id))
        .cloned();
    let right = right_for(ask.password, hash).await;
    if right {
        auth.got_in(&trier);
    }
    let Some(user) = found.filter(|_| right) else {
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
        token: None,
    };
    let mut response = Json(view(&state, &who).await).into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, cookie(&token, SESSION_LIFE));
    response
}

pub async fn sign_out(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = token_of(&headers) {
        let hash = fingerprint(token);
        state.0.auth.end(token);
        state.0.sockets.close_session(&hash);
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
    password: String,
}

impl std::fmt::Debug for FirstOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirstOwner")
            .field("name", &self.name)
            .finish()
    }
}

/// Sets up the home's owner, with the password that locks it. Open to anyone who can reach
/// Irori, and only while no owner has a password: this is the first run, and whoever is setting
/// the home up is who is there. After it, people are added by an owner.
///
/// It is also the way back for an owner who forgot theirs: with their line taken out of
/// `secrets.toml` no owner has a password, whoever else still does, and this sets a new one.
///
/// A home from before passwords were required may already have an owner without one. They
/// are the one set up: they keep their id and take the name and password given.
pub async fn set_up(State(state): State<AppState>, Json(ask): Json<FirstOwner>) -> Response {
    // Looked at before any hashing: this address is open to anyone, and a home that is
    // already locked mustn't be made to do slow work for whoever asks.
    const DONE: &str = "this home already has an owner; people are added in Settings";
    if state.0.config.people().await.owned() {
        return refused(StatusCode::UNPROCESSABLE_ENTITY, DONE.to_owned());
    }
    let hash = match password_hash(Some(ask.password)).await {
        Ok(Some(hash)) => hash,
        Ok(None) => return refused(StatusCode::UNPROCESSABLE_ENTITY, NEEDS_PASSWORD.to_owned()),
        Err(why) => return refused(StatusCode::UNPROCESSABLE_ENTITY, why),
    };
    let Some(fresh) = irori_types::user_id_from(ask.name.as_str()) else {
        return refused(
            StatusCode::UNPROCESSABLE_ENTITY,
            "that name has no letters or numbers to make an id from".to_owned(),
        );
    };
    // Before the owner is written. The home is still open here, and an owner who keeps
    // their id would honour every old token the moment they have a password. If the
    // database can't forget them, nothing else happens.
    if let Err(why) = state.0.auth.tokens.revoke_all() {
        return refused(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("couldn't write it down: {why}"),
        );
    }
    state.0.sockets.close_all_tokens();
    let name = ask.name;
    let made = state
        .0
        .config
        .edit_people(&state.0.core, |people| {
            if people.owned() {
                return Err(Refused(DONE.to_owned()));
            }
            let owner = people
                .users
                .iter()
                .position(|user| user.role.runs_the_home());
            let user = match owner {
                Some(at) => {
                    people.users[at].name = name.clone();
                    people.users[at].clone()
                }
                None => {
                    let user = User {
                        id: fresh.clone(),
                        name: name.clone(),
                        role: Role::Owner,
                    };
                    people.users.push(user.clone());
                    people.users.sort_by(|a, b| a.id.cmp(&b.id));
                    user
                }
            };
            people.hashes.insert(user.id.clone(), hash);
            Ok(user)
        })
        .await;
    let user = match made {
        Ok(user) => user,
        Err(error) => return edit_failed(error),
    };
    tracing::info!(user = %user.id, "the home has an owner, and asks who is there");
    // Every sign-in from before this is over. Setting the owner up is also how a forgotten
    // password is put right (its line taken out of the files, then this), and a browser that
    // was signed in under the old one, a lost phone say, must not still be. An open home's
    // sockets had no sign-in at all; they close too, because the home now asks who is there.
    state.0.auth.forget(|_, _| false);
    state.0.sockets.close_open_and_sessions();
    // The home is locked from here on, so the person who just set it up is signed in rather
    // than shown the door.
    let token = state.0.auth.begin(&user.id);
    let who = Actor {
        user: Some(user),
        owner: true,
        token: None,
    };
    let mut response = (StatusCode::CREATED, Json(view(&state, &who).await)).into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, cookie(&token, SESSION_LIFE));
    response
}

/// What is said when somebody is to be let in with no password.
pub const NEEDS_PASSWORD: &str =
    "everybody in the home needs a password: it is what Irori tells people apart by";

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
            (Method::GET, "/api/extensions/demo/icon.svg", Nothing),
            (Method::GET, "/api/home", Use),
            (Method::GET, "/api/users", Use),
            (Method::GET, "/api/system", Use),
            (Method::GET, "/api/history/light.hall", Use),
            (Method::GET, "/api/extensions/demo/log", Use),
            (Method::POST, "/api/command", Use),
            (Method::POST, "/api/assistant/turns", Use),
            (Method::POST, "/api/assistant/turns/general/stop", Use),
            (Method::PATCH, "/api/users/nico", Use),
            (Method::POST, "/api/users", Run),
            (Method::DELETE, "/api/users/nico", Run),
            (Method::PUT, "/api/place", Run),
            (Method::POST, "/api/restart", Run),
            (Method::PUT, "/api/assistant", Run),
            (Method::POST, "/api/apps/automations/rpc", Run),
            (Method::PUT, "/api/extensions/mqtt/secrets", Run),
            (Method::GET, "/api/ws", Use),
            // HEAD is not a read. A token still needs the home's scopes for this path.
            (Method::HEAD, "/api/home", Run),
            (Method::GET, "/api/tokens", Run),
            (Method::POST, "/api/tokens", Run),
            (Method::DELETE, "/api/tokens/tablet", Run),
            (Method::GET, "/api/extension", Run),
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
