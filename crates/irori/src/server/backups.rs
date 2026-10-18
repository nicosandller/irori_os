//! `/api/backups` (`docs/specs/backup.md` §7).
//!
//! A backup holds password hashes and secrets, so none of this is in the auth table's read list:
//! only an owner's session, with the page's header, gets in, and a token never does. The two
//! `/api/setup/backup/…` routes are the exception, and exist for one moment only: a home with no
//! owner yet may be restored instead of being set up, which is the same trust the first owner
//! is given.

// A handler's refusal is a whole response, passed up with `?`-style early returns; boxing each
// one would only add noise to code that answers a request and ends.
#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::Ordering;

use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt as _;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt as _;

use super::{AppState, refused};
use crate::backup::{
    self, Entry, Parts, Restored,
    schedule::{self, Status},
    settings::{self, Settings},
};

/// The most a backup file may be when it is sent to Irori.
const MOST_UPLOADED: u64 = 16 << 30;

pub(super) fn routes() -> Router<AppState> {
    let upload = || axum::extract::DefaultBodyLimit::disable();
    Router::new()
        .route("/api/backups", get(overview).post(make))
        .route("/api/backups/upload", post(send).layer(upload()))
        .route("/api/backups/settings", axum::routing::put(save_settings))
        .route("/api/backups/key", get(show_key).post(set_key))
        .route("/api/backups/restore/cancel", post(cancel_restore))
        .route("/api/backups/restore/seen", post(restore_seen))
        .route(
            "/api/backups/destinations/{place}/test",
            post(test_destination),
        )
        .route("/api/backups/destinations/{place}", get(browse_destination))
        .route(
            "/api/backups/destinations/{place}/fetch",
            post(fetch_from_destination),
        )
        .route("/api/backups/{name}", get(download).delete(remove))
        .route("/api/backups/{name}/verify", post(verify))
        .route("/api/backups/{name}/restore", post(restore))
        .route("/api/setup/backup/upload", post(setup_send).layer(upload()))
        .route("/api/setup/backup/restore", post(setup_restore))
        .route("/api/setup/backup/verify", post(setup_verify))
}

#[derive(Debug, Clone)]
struct Places {
    config: PathBuf,
    data: PathBuf,
}

impl Places {
    fn dir(&self) -> PathBuf {
        backup::dir(&self.data)
    }
}

async fn places(state: &AppState) -> Places {
    Places {
        config: state.0.config.dir().await,
        data: state
            .0
            .db
            .path
            .parent()
            .map(FsPath::to_path_buf)
            .unwrap_or_default(),
    }
}

/// Runs file work off the async runtime.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Response> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| refused(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// The key typed, else the one this Irori has on file, which opens whatever it sealed itself.
fn key_to_try(config: &FsPath, typed: Option<String>) -> Option<String> {
    typed
        .filter(|k| !k.trim().is_empty())
        .or_else(|| settings::secret(config, settings::KEY))
}

fn failed(error: impl std::fmt::Display) -> Response {
    refused(StatusCode::UNPROCESSABLE_ENTITY, error.to_string())
}

fn failed_anyhow(error: &anyhow::Error) -> Response {
    // Wrong key, damaged, refused: the sentence is for the person, and the chain has the why.
    refused(StatusCode::UNPROCESSABLE_ENTITY, format!("{error:#}"))
}

// ----- what the page shows -----

#[derive(Debug, Serialize)]
struct EntryView {
    #[serde(flatten)]
    entry: Entry,
    /// The destinations that also hold it.
    copies: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SettingsView {
    #[serde(flatten)]
    settings: Settings,
    /// Whether each destination's secret is on file. The secret itself is never sent.
    secrets_set: BTreeMap<String, bool>,
}

#[derive(Debug, Serialize)]
struct Overview {
    backups: Vec<EntryView>,
    settings: SettingsView,
    status: Status,
    /// Whether there is a recovery key on file. The page asks for the key itself separately.
    key_set: bool,
    /// A restore is staged and waiting for the restart.
    restore_pending: bool,
    /// What the last restore did, until it is dismissed.
    last_restore: Option<Restored>,
    /// Free room on the volume the backups are written to, in bytes.
    free_bytes: Option<u64>,
}

fn overview_of(at: &Places) -> Overview {
    let status = schedule::status(&at.data);
    let settings = settings::load(&at.config);
    let secrets_set = settings
        .destination
        .iter()
        .filter_map(|d| {
            d.secret_name().map(|s| {
                (
                    d.name().to_owned(),
                    settings::secret(&at.config, &s).is_some(),
                )
            })
        })
        .collect();
    Overview {
        backups: backup::list(&at.dir())
            .into_iter()
            .map(|entry| EntryView {
                copies: status.copies.get(&entry.name).cloned().unwrap_or_default(),
                entry,
            })
            .collect(),
        settings: SettingsView {
            settings,
            secrets_set,
        },
        status,
        key_set: settings::secret(&at.config, settings::KEY).is_some(),
        restore_pending: backup::pending(&at.data),
        last_restore: backup::last_restore(&at.data),
        free_bytes: free_bytes(&at.data),
    }
}

fn free_bytes(path: &FsPath) -> Option<u64> {
    let v = rustix::fs::statvfs(path).ok()?;
    Some(v.f_bavail.saturating_mul(v.f_frsize))
}

async fn overview(State(state): State<AppState>) -> Response {
    let at = places(&state).await;
    match blocking(move || overview_of(&at)).await {
        Ok(view) => Json(view).into_response(),
        Err(response) => response,
    }
}

async fn make(State(state): State<AppState>) -> Response {
    let at = places(&state).await;
    let made = blocking(move || schedule::run_once(&at.config, &at.data)).await;
    match made {
        Ok(Ok(file)) => {
            tracing::info!(file = %file.display(), "backup written");
            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (
                StatusCode::CREATED,
                Json(serde_json::json!({ "name": name })),
            )
                .into_response()
        }
        Ok(Err(error)) => refused(StatusCode::INTERNAL_SERVER_ERROR, error),
        Err(response) => response,
    }
}

// ----- one file -----

fn named(at: &Places, name: &str) -> Result<PathBuf, Response> {
    if !backup::is_backup_name(name) {
        return Err(refused(
            StatusCode::NOT_FOUND,
            format!("no backup called `{name}`"),
        ));
    }
    let path = at.dir().join(name);
    if path.is_file() {
        Ok(path)
    } else {
        Err(refused(
            StatusCode::NOT_FOUND,
            format!("no backup called `{name}`"),
        ))
    }
}

async fn download(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let at = places(&state).await;
    let path = match named(&at, &name) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let Ok(file) = tokio::fs::File::open(&path).await else {
        return refused(StatusCode::NOT_FOUND, format!("no backup called `{name}`"));
    };
    let length = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(file));
    (
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (header::CONTENT_LENGTH, length.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        body,
    )
        .into_response()
}

async fn remove(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let at = places(&state).await;
    let path = match named(&at, &name) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let data = at.data.clone();
    let done = blocking(move || {
        std::fs::remove_file(&path)?;
        let mut status = schedule::status(&data);
        status.copies.remove(&name);
        schedule::save_status(&data, &status);
        Ok::<_, std::io::Error>(())
    })
    .await;
    match done {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(error)) => refused(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
        Err(response) => response,
    }
}

#[derive(Debug, Default, Deserialize)]
struct Opening {
    #[serde(default)]
    key: Option<String>,
}

async fn verify(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(ask): Json<Opening>,
) -> Response {
    let at = places(&state).await;
    let path = match named(&at, &name) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let scratch = at.dir();
    let key = key_to_try(&at.config, ask.key);
    match blocking(move || backup::verify(&path, key.as_deref(), &scratch)).await {
        Ok(Ok(verified)) => Json(verified).into_response(),
        Ok(Err(error)) => failed_anyhow(&error),
        Err(response) => response,
    }
}

#[derive(Debug, Deserialize)]
struct Restoring {
    #[serde(default)]
    key: Option<String>,
    #[serde(default = "all_parts")]
    parts: Parts,
}

fn all_parts() -> Parts {
    Parts::ALL
}

/// Opens the backup, checks it, stages it, and starts Irori again to put it in place.
async fn restore(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(ask): Json<Restoring>,
) -> Response {
    let at = places(&state).await;
    let path = match named(&at, &name) {
        Ok(path) => path,
        Err(response) => return response,
    };
    stage_and_restart(&state, at, path, ask).await
}

async fn stage_and_restart(
    state: &AppState,
    at: Places,
    path: PathBuf,
    ask: Restoring,
) -> Response {
    if !(ask.parts.settings || ask.parts.extensions || ask.parts.state || ask.parts.history) {
        return failed("choose something to restore");
    }
    let data = at.data.clone();
    let key = key_to_try(&at.config, ask.key);
    let staged = blocking(move || backup::stage(&path, key.as_deref(), ask.parts, &data)).await;
    match staged {
        Ok(Ok(verified)) => {
            tracing::info!("restore staged; restarting to put it in place");
            // Same as the Restart button: the exec happens once this request has drained, and
            // the restore is applied first thing in the new process, before anything is opened.
            state.0.restarting.store(true, Ordering::SeqCst);
            state.0.restart.notify_one();
            (StatusCode::ACCEPTED, Json(verified)).into_response()
        }
        Ok(Err(error)) => failed_anyhow(&error),
        Err(response) => response,
    }
}

async fn cancel_restore(State(state): State<AppState>) -> Response {
    let at = places(&state).await;
    backup::cancel_pending(&at.data);
    StatusCode::NO_CONTENT.into_response()
}

/// The page has shown what the last restore did.
async fn restore_seen(State(state): State<AppState>) -> Response {
    let at = places(&state).await;
    backup::clear_last_restore(&at.data);
    StatusCode::NO_CONTENT.into_response()
}

// ----- receiving a file -----

/// `?name=…` from a raw query. Names are letters, digits and `-_.`, so nothing is decoded.
fn wanted_name(query: Option<String>) -> Option<String> {
    query?
        .split('&')
        .find_map(|pair| pair.strip_prefix("name=").map(str::to_owned))
}

/// Streams the request body into `<backups>/.upload-….part`, then checks it is a backup and
/// gives it its name. Nothing is kept that isn't one.
async fn receive(at: &Places, name: Option<String>, body: Body, length: Option<u64>) -> Response {
    match receive_named(at, name, body, length).await {
        Ok(name) => created(&name),
        Err(response) => response,
    }
}

fn created(name: &str) -> Response {
    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "name": name })),
    )
        .into_response()
}

async fn receive_named(
    at: &Places,
    name: Option<String>,
    body: Body,
    length: Option<u64>,
) -> Result<String, Response> {
    let dir = at.dir();
    if let Some(length) = length
        && length > MOST_UPLOADED
    {
        return Err(failed("that file is too big to be a backup"));
    }
    if let Some(free) = free_bytes(&at.data)
        && length.is_some_and(|l| l > free)
    {
        return Err(failed("there isn't room for that file here"));
    }
    if std::fs::create_dir_all(&dir).is_err() {
        return Err(refused(
            StatusCode::INTERNAL_SERVER_ERROR,
            "couldn't make the backup directory".to_owned(),
        ));
    }
    let mut suffix = [0_u8; 6];
    let _ = getrandom::fill(&mut suffix);
    let part = dir.join(format!(".upload-{}.part", super::auth::hex(&suffix)));
    let written = async {
        let mut out = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&part)
            .await?;
        let mut stream = body.into_data_stream();
        let mut total = 0_u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(std::io::Error::other)?;
            total += chunk.len() as u64;
            if total > MOST_UPLOADED {
                return Err(std::io::Error::other("that file is too big to be a backup"));
            }
            out.write_all(&chunk).await?;
        }
        out.flush().await
    }
    .await;
    if let Err(error) = written {
        let _ = std::fs::remove_file(&part);
        return Err(failed(format!("the upload didn't finish: {error}")));
    }
    let (dir2, part2) = (dir.clone(), part.clone());
    let finished = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let (sealed, _) =
            backup::inspect(&part2).map_err(|_| "that isn't an Irori backup".to_owned())?;
        let suffix = if sealed { ".tar.gz.enc" } else { ".tar.gz" };
        let given = name
            .filter(|n| backup::is_backup_name(n) && n.ends_with(suffix) && !dir2.join(n).exists());
        let final_name = given.unwrap_or_else(|| {
            format!(
                "{}uploaded-{}{suffix}",
                backup::PREFIX,
                jiff::Timestamp::now().strftime("%Y%m%d-%H%M%S")
            )
        });
        std::fs::rename(&part2, dir2.join(&final_name)).map_err(|e| e.to_string())?;
        Ok(final_name)
    })
    .await;
    match finished {
        Ok(Ok(name)) => Ok(name),
        Ok(Err(why)) => {
            let _ = std::fs::remove_file(&part);
            Err(failed(why))
        }
        Err(error) => {
            let _ = std::fs::remove_file(&part);
            Err(refused(
                StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            ))
        }
    }
}

fn content_length(headers: &axum::http::HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}

async fn send(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: axum::http::HeaderMap,
    body: Body,
) -> Response {
    let at = places(&state).await;
    receive(&at, wanted_name(query), body, content_length(&headers)).await
}

// ----- a home with no owner yet -----

const SPOKEN_FOR: &str = "this home already has an owner; restore from Settings";

async fn unowned(state: &AppState) -> Result<(), Response> {
    if state.0.config.people().await.owned() {
        Err(refused(
            StatusCode::UNPROCESSABLE_ENTITY,
            SPOKEN_FOR.to_owned(),
        ))
    } else {
        Ok(())
    }
}

async fn setup_send(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: axum::http::HeaderMap,
    body: Body,
) -> Response {
    if let Err(response) = unowned(&state).await {
        return response;
    }
    // One at a time: nobody has signed in here, so nobody gets to fill the disk in parallel.
    let Some(_turn) = SetupTurn::take() else {
        return failed("another backup is being sent; wait for it to finish");
    };
    let at = places(&state).await;
    let name = match receive_named(&at, wanted_name(query), body, content_length(&headers)).await {
        Ok(name) => name,
        Err(response) => return response,
    };
    // An owner may have been made while the file was coming in: then it is not wanted.
    if let Err(response) = unowned(&state).await {
        let _ = std::fs::remove_file(at.dir().join(&name));
        return response;
    }
    created(&name)
}

/// Held while a file is being received on a home with no owner yet.
static SETUP_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

struct SetupTurn;

impl SetupTurn {
    fn take() -> Option<Self> {
        SETUP_BUSY
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
            .then_some(Self)
    }
}

impl Drop for SetupTurn {
    fn drop(&mut self) {
        SETUP_BUSY.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, Deserialize)]
struct SetupRestore {
    name: String,
    #[serde(default)]
    key: Option<String>,
}

async fn setup_verify(State(state): State<AppState>, Json(ask): Json<SetupRestore>) -> Response {
    if let Err(response) = unowned(&state).await {
        return response;
    }
    let at = places(&state).await;
    let path = match named(&at, &ask.name) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let scratch = at.dir();
    let key = key_to_try(&at.config, ask.key);
    match blocking(move || backup::verify(&path, key.as_deref(), &scratch)).await {
        Ok(Ok(verified)) => Json(verified).into_response(),
        Ok(Err(error)) => failed_anyhow(&error),
        Err(response) => response,
    }
}

async fn setup_restore(State(state): State<AppState>, Json(ask): Json<SetupRestore>) -> Response {
    if let Err(response) = unowned(&state).await {
        return response;
    }
    let at = places(&state).await;
    let path = match named(&at, &ask.name) {
        Ok(path) => path,
        Err(response) => return response,
    };
    stage_and_restart(
        &state,
        at,
        path,
        Restoring {
            key: ask.key,
            parts: Parts::ALL,
        },
    )
    .await
}

// ----- settings and the key -----

#[derive(Debug, Deserialize)]
struct SaveSettings {
    #[serde(flatten)]
    settings: Settings,
    /// Secrets to set, by destination name. A destination with no entry keeps what it has.
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

async fn save_settings(State(state): State<AppState>, Json(ask): Json<SaveSettings>) -> Response {
    let at = places(&state).await;
    let done = blocking(move || -> Result<Overview, String> {
        ask.settings.check()?;
        for destination in &ask.settings.destination {
            if let (Some(secret), Some(text)) = (
                destination.secret_name(),
                ask.secrets.get(destination.name()),
            ) && !text.is_empty()
            {
                settings::set_secret(&at.config, &secret, text)?;
            }
        }
        // The key is made as encryption is turned on, so a scheduled backup at 03:00 only reads
        // it, and so the page can show it before the first sealed backup exists.
        if ask.settings.encrypt {
            settings::key_or_make(&at.config)?;
        }
        settings::save(&at.config, &ask.settings)?;
        Ok(overview_of(&at))
    })
    .await;
    match done {
        Ok(Ok(view)) => Json(view).into_response(),
        Ok(Err(why)) => failed(why),
        Err(response) => response,
    }
}

async fn show_key(State(state): State<AppState>) -> Response {
    let at = places(&state).await;
    match blocking(move || settings::key_or_make(&at.config)).await {
        Ok(Ok((key, _))) => Json(serde_json::json!({ "key": key })).into_response(),
        Ok(Err(why)) => refused(StatusCode::INTERNAL_SERVER_ERROR, why),
        Err(response) => response,
    }
}

#[derive(Debug, Deserialize)]
struct NewKey {
    /// A passphrase of the person's own. Left out, a new recovery key is made.
    #[serde(default)]
    key: Option<String>,
}

/// Backups already made keep the key they were made with; only new ones use this.
async fn set_key(State(state): State<AppState>, Json(ask): Json<NewKey>) -> Response {
    let at = places(&state).await;
    let key = match ask.key {
        Some(own) if own.trim().chars().count() < 12 => {
            return failed("a passphrase of your own has to be at least 12 characters");
        }
        Some(own) => own,
        None => backup::crypt::generate_key(),
    };
    match blocking(move || settings::set_secret(&at.config, settings::KEY, &key).map(|()| key))
        .await
    {
        Ok(Ok(key)) => Json(serde_json::json!({ "key": key })).into_response(),
        Ok(Err(why)) => refused(StatusCode::INTERNAL_SERVER_ERROR, why),
        Err(response) => response,
    }
}

// ----- other places -----

fn destination_named(
    at: &Places,
    place: &str,
) -> Result<(settings::Destination, Option<String>), Response> {
    let settings = settings::load(&at.config);
    let Some(destination) = settings.destination.into_iter().find(|d| d.name() == place) else {
        return Err(refused(
            StatusCode::NOT_FOUND,
            format!("no destination called `{place}`; save it first"),
        ));
    };
    let secret = destination
        .secret_name()
        .and_then(|s| settings::secret(&at.config, &s));
    Ok((destination, secret))
}

/// Whether the place can be reached and written to.
async fn test_destination(State(state): State<AppState>, Path(place): Path<String>) -> Response {
    let at = places(&state).await;
    let (destination, secret) = match destination_named(&at, &place) {
        Ok(found) => found,
        Err(response) => return response,
    };
    match blocking(move || backup::dest::check(&destination, secret.as_deref())).await {
        Ok(Ok(())) => Json(serde_json::json!({ "ok": true })).into_response(),
        Ok(Err(why)) => failed(why),
        Err(response) => response,
    }
}

/// The backups that are at the place, and which of them are here too.
async fn browse_destination(State(state): State<AppState>, Path(place): Path<String>) -> Response {
    let at = places(&state).await;
    let (destination, secret) = match destination_named(&at, &place) {
        Ok(found) => found,
        Err(response) => return response,
    };
    let here = at.dir();
    match blocking(move || {
        backup::dest::list(&destination, secret.as_deref()).map(|there| {
            there
                .into_iter()
                .map(|r| {
                    let local = here.join(&r.name).exists();
                    serde_json::json!({ "name": r.name, "here": local })
                })
                .collect::<Vec<_>>()
        })
    })
    .await
    {
        Ok(Ok(list)) => Json(list).into_response(),
        Ok(Err(why)) => failed(why),
        Err(response) => response,
    }
}

#[derive(Debug, Deserialize)]
struct Fetch {
    name: String,
}

/// Brings a backup from the place to this machine's list, where it can be checked and restored.
async fn fetch_from_destination(
    State(state): State<AppState>,
    Path(place): Path<String>,
    Json(ask): Json<Fetch>,
) -> Response {
    let at = places(&state).await;
    if !backup::is_backup_name(&ask.name) {
        return refused(
            StatusCode::NOT_FOUND,
            format!("no backup called `{}`", ask.name),
        );
    }
    let (destination, secret) = match destination_named(&at, &place) {
        Ok(found) => found,
        Err(response) => return response,
    };
    let to = at.dir().join(&ask.name);
    if to.exists() {
        return Json(serde_json::json!({ "name": ask.name })).into_response();
    }
    let dir = at.dir();
    let name = ask.name.clone();
    match blocking(move || {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        backup::dest::get(&destination, secret.as_deref(), &name, &to)
    })
    .await
    {
        Ok(Ok(())) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "name": ask.name })),
        )
            .into_response(),
        Ok(Err(why)) => failed(why),
        Err(response) => response,
    }
}
