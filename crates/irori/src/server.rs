//! HTTP server: the public API (`docs/specs/api.md`), and the embedded UI. Who may ask for
//! what is `server/auth.rs`: a home with no passwords is open to whoever can reach it, which
//! is why `serve` binds loopback unless told otherwise. A program that isn't the page sends a
//! bearer token. The page signs in with a cookie.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use irori_core::{CallError, Command, Core, Event, ExtensionHost, ExtensionOverview, Reach};
use irori_types::{
    Area, AreaId, ContextId, Description, Device, DeviceId, Entity, EntityId, EntityState,
    ExtensionId, Floor, FloorId, Floorplan, Name, Origin, Outcome, Placement, Timestamp, UserId,
};
use tokio::sync::broadcast;

use crate::build_info::{BuildInfo, VERSION};
use crate::config::{Config, EditError, Refused};
use crate::db::Database;
use crate::history::History;
use crate::syslog;

mod activity;
mod apps;
#[cfg(feature = "assist")]
mod assistant;
mod auth;
mod backups;
mod live;
mod people;
mod tokens;
mod trail;

/// Who commands are attributed to in a home where nobody has been set up.
static UNAUTHENTICATED: LazyLock<UserId> =
    LazyLock::new(|| UserId::try_from("unauthenticated").expect("a valid user id"));

#[derive(Debug, Clone)]
pub struct AppState(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    started: Instant,
    /// A different value on every boot, so a page that spots it changing knows the restart it
    /// asked for happened. Uptime can't say that: a machine that boots and is opened within a
    /// minute would make a fresh-ish uptime look like a restart, and a slow restart could pass
    /// any freshness bound. The generation survives an exec (the process id is the same), so it
    /// has to be made anew inside this constructor rather than keyed off the pid.
    boot_id: String,
    db: Database,
    build: BuildInfo,
    core: Core,
    config: Config,
    host: ExtensionHost,
    history: History,
    /// Who did what (`docs/specs/access.md` §3).
    audit: crate::audit::Audit,
    /// What this process has said lately, kept by the log subscriber's writer (`syslog`) and
    /// served to the Settings page. Held here rather than reached for globally so a test can put
    /// lines in it without standing up a subscriber.
    log: Arc<syslog::Log>,
    /// Waking this asks `serve` (crates/irori/src/main.rs) to shut down and start this binary
    /// again. The atomic records that the shutdown was a requested restart: `serve` reads it
    /// once the server has stopped and re-execs itself instead of just stopping.
    restart: Arc<tokio::sync::Notify>,
    restarting: Arc<AtomicBool>,
    /// When a restart was last *accepted* this boot, for `RESTART_COOLDOWN`. Fresh (`None`) in
    /// every new process, which is the point: the window resets with the boot, so the cap is one
    /// restart per cooldown per boot rather than a permanent lockout.
    last_restart: Mutex<Option<Instant>>,
    /// Who is signed in, and who has been getting their password wrong.
    auth: auth::Auth,
    /// Live sockets and extension sockets, so a sign-out or a revocation can close them.
    sockets: live::Sockets,
    /// The questions the assistant is in the middle of answering, by conversation.
    #[cfg(feature = "assist")]
    turns: crate::assistant::Turns,
    /// Whether a model is ready, as anybody who is not an owner is told.
    #[cfg(feature = "assist")]
    readiness: crate::assistant::Readiness,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Database,
        core: Core,
        config: Config,
        host: ExtensionHost,
        history: History,
        audit: crate::audit::Audit,
        log: Arc<syslog::Log>,
        restart: Arc<tokio::sync::Notify>,
        restarting: Arc<AtomicBool>,
    ) -> Self {
        Self(Arc::new(Inner {
            started: Instant::now(),
            boot_id: boot_id(),
            auth: auth::Auth::open(&db),
            sockets: live::Sockets::default(),
            db,
            build: BuildInfo::current(),
            core,
            config,
            host,
            history,
            audit,
            log,
            restart,
            restarting,
            last_restart: Mutex::new(None),
            #[cfg(feature = "assist")]
            turns: crate::assistant::Turns::default(),
            #[cfg(feature = "assist")]
            readiness: crate::assistant::Readiness::default(),
        }))
    }
}

/// A fresh value after every boot: 16 random bytes from the OS, in hex. It must be
/// genuinely random rather than, say, the wall clock: the same process image re-execs on a
/// restart, so a clock that froze or stepped back could hand the new boot the old boot's id,
/// and the page would then never see the change that tells it the restart happened.
fn boot_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .expect("the OS must be able to hand over sixteen random bytes for the boot id");
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(32);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Says whether this server is serving over https, so its cookies are marked for https only.
pub fn serve_cookies_over_tls(tls: bool) {
    auth::over_tls(tls);
}

pub fn router(state: AppState) -> Router {
    let router = Router::new()
        .route("/api/health", get(health))
        // Who is there, and setting the home up the first time (`server/auth.rs`).
        .route(
            "/api/session",
            get(auth::session)
                .post(auth::sign_in)
                .delete(auth::sign_out),
        )
        .route("/api/setup", post(auth::set_up))
        .route("/api/users", get(people::users).post(people::add_user))
        // Who did what: an owner reads it, and no token does (`server/activity.rs`).
        .route("/api/audit", get(activity::list))
        .route(
            "/api/users/{id}",
            patch(people::edit_user).delete(people::remove_user),
        )
        .route("/api/place", get(people::place).put(people::set_place))
        // The public API (`docs/specs/api.md`). The page and other programs use the same paths.
        // Everything the Devices page shows, in one response; the rest are the same data split up.
        .route("/api/home", get(home))
        .route("/api/command", post(command))
        .route("/api/ws", get(live::feed))
        .route("/api/extension", get(live::extension))
        .route("/api/tokens", get(tokens::list).post(tokens::create))
        .route("/api/tokens/{id}", axum::routing::delete(tokens::revoke))
        .route(
            "/api/devices",
            get(
                |State(s): State<AppState>, Extension(reach): Extension<Reach>| async move {
                    Json(s.0.core.seen(&reach).devices())
                },
            ),
        )
        .route(
            "/api/entities",
            get(
                |State(s): State<AppState>, Extension(reach): Extension<Reach>| async move {
                    Json(s.0.core.seen(&reach).entities())
                },
            ),
        )
        .route(
            "/api/states",
            get(
                |State(s): State<AppState>, Extension(reach): Extension<Reach>| async move {
                    Json(s.0.core.seen(&reach).states())
                },
            ),
        )
        .route("/api/history/{entity_id}/summary", get(entity_summary))
        .route("/api/history/{entity_id}", get(entity_history))
        .route("/api/recorder", get(recorder_settings).put(set_recorder))
        .route("/api/system", get(host_info))
        .route("/api/system/usage", get(usage))
        .route("/api/system/log", get(system_log))
        .route("/api/serial-ports", get(serial_ports))
        .route("/api/restart", post(restart))
        .route("/api/network", get(home_network))
        // How the extensions behind the home are faring is the owner's business, and it names
        // the devices they have found: a person with only some rooms is told of none.
        .route(
            "/api/extensions",
            get(
                |State(s): State<AppState>, Extension(reach): Extension<Reach>| async move {
                    Json(if reach.is_whole() {
                        s.0.core.extensions()
                    } else {
                        BTreeMap::new()
                    })
                },
            ),
        )
        // What a person has said about their home (`docs/specs/config.md`). These write files.
        .route("/api/areas", get(areas).post(add_area))
        .route("/api/floors", get(floors).post(add_floor))
        .route("/api/floors/{id}", patch(edit_floor).delete(remove_floor))
        .route("/api/areas/{id}", patch(edit_area).delete(remove_area))
        .route("/api/floorplan", get(floorplan).put(save_floorplan))
        .route(
            "/api/devices/{id}",
            patch(edit_device).delete(remove_device),
        )
        .route("/api/entities/{id}", patch(edit_entity))
        .route("/api/entities/{id}/picture", get(camera_picture))
        .route("/api/extensions/{id}/secrets", put(give_secret))
        .route(
            "/api/extensions/{id}/settings",
            post(set_extension_settings),
        )
        .route(
            "/api/extensions/{id}/actions/{action_id}",
            post(trigger_extension_action).delete(stop_extension_action),
        )
        .route("/api/catalog", get(catalog))
        .route("/api/extensions/{id}/install", post(install_official))
        .route("/api/extensions/install", post(install_url))
        .route("/api/extensions/{id}", axum::routing::delete(uninstall))
        .route("/api/helpers", get(list_helpers))
        .route("/api/helpers/{table}", post(add_helper))
        .route("/api/helpers/{table}/check", post(check_helper))
        .route("/api/helpers/{table}/preview", post(preview_helper))
        .route(
            "/api/helpers/{table}/{id}",
            axum::routing::patch(edit_helper).delete(remove_helper),
        )
        .route("/api/extensions/{id}/icon.svg", get(extension_icon))
        .route("/api/extensions/{id}/log", get(extension_log))
        // Extensions' own pages and their engines (`docs/specs/automations.md` §B3).
        .route("/api/apps", get(apps::list))
        .route("/api/apps/{id}/rpc", post(apps::rpc))
        // Their files. Not under `/apps/`: those addresses are the shell's own pages that show
        // them, and must reach the shell when reloaded.
        .route("/pages/{id}/", get(apps::index))
        .route("/pages/{id}/{*path}", get(apps::file));
    #[cfg(feature = "assist")]
    let router = router
        .route("/api/assistant", get(assistant::get).put(assistant::put))
        // A question can carry a picture of a floorplan, which is far more than the two
        // megabytes any other request is allowed.
        .route(
            "/api/assistant/turns",
            post(assistant::turns)
                .layer(axum::extract::DefaultBodyLimit::max(assistant::MOST_ASKED)),
        )
        .route("/api/assistant/turns/{scope}", get(assistant::follow))
        .route("/api/assistant/turns/{scope}/stop", post(assistant::stop))
        .route("/api/assistant/check", post(assistant::check))
        .route("/api/assistant/pull", post(assistant::pull))
        .route("/api/assistant/forget", post(assistant::forget))
        .route("/api/assistant/install", post(assistant::install))
        .route("/api/assistant/uninstall", post(assistant::uninstall))
        .route("/api/assistant/log", get(assistant::model_log))
        .route("/api/assistant/load", post(assistant::load))
        .route("/api/assistant/unload", post(assistant::unload))
        .route(
            "/api/assistant/transcript/{scope}",
            get(assistant::transcript).delete(assistant::clear),
        );
    router
        .merge(backups::routes())
        .fallback(get(ui::serve))
        // Over everything, the page included: one place decides who may ask for what.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::guard,
        ))
        .with_state(state)
}

/// Everything the Devices page shows, in one response: what exists, what it's doing, and how the
/// extensions behind it are faring. `GET /api/home` and the first frame on `/api/ws` are this.
#[derive(Debug, Serialize)]
struct HomeView {
    devices: Vec<Device>,
    entities: Vec<Entity>,
    states: Vec<EntityState>,
    extensions: BTreeMap<ExtensionId, ExtensionOverview>,
    areas: Vec<Area>,
    /// The levels of the home, lowest first. Empty for a home nobody has divided into floors.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    floors: Vec<Floor>,
    /// Devices a person keeps out of the home, so they can be let back in.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    held: Vec<irori_core::HeldDevice>,
    /// The home as it's drawn. Left out entirely while nobody has drawn one, which is most
    /// homes: the Floorplan page then knows to offer a blank canvas rather than an empty plan.
    #[serde(skip_serializing_if = "Floorplan::is_empty")]
    floorplan: Floorplan,
    /// Updates waiting their turn to install, behind another on the same protocol.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    installs: Vec<irori_core::QueuedInstall>,
}

impl HomeView {
    /// The home as `reach` has it.
    ///
    /// Every field is named, so one added to the view will not compile until somebody has said
    /// what a person with only some rooms is shown of it. The lists come from [`irori_core::Seen`],
    /// which has nothing else to give.
    fn of(core: &Core, reach: &Reach) -> HomeView {
        let seen = core.seen(reach);
        let whole = reach.is_whole();
        HomeView {
            devices: seen.devices(),
            entities: seen.entities(),
            states: seen.states(),
            // What the extensions have found and are doing is the owner's.
            extensions: if whole {
                core.extensions()
            } else {
                BTreeMap::new()
            },
            areas: seen.areas(),
            floors: seen.floors(),
            // Devices kept out of the home are the owner's to let back in.
            held: if whole {
                core.held_devices()
            } else {
                Vec::new()
            },
            floorplan: seen.floorplan(),
            installs: core
                .queued_installs()
                .into_iter()
                .filter(|install| reach.sees_entity(&install.entity_id))
                .collect(),
        }
    }
}

fn snapshot(state: &AppState, reach: &Reach) -> HomeView {
    HomeView::of(&state.0.core, reach)
}

async fn home(State(state): State<AppState>, Extension(reach): Extension<Reach>) -> Json<HomeView> {
    Json(snapshot(&state, &reach))
}

/// The machine running Irori, for the Settings page's System menu. Read from the OS each time
/// it's asked, rather than kept: a Settings page check that cached could shrug at a disk that
/// filled or a machine that was swapped out from under it.
async fn host_info(State(state): State<AppState>) -> Json<crate::host_info::HostView> {
    Json(crate::host_info::read(&state.0.db.path))
}

/// What is using the machine: the heaviest processes, and what the data directory is made of.
/// Slower than `host_info` — it watches every process for a moment — so the page asks for it
/// only while a meter is open, and it runs off the async runtime.
async fn usage(State(state): State<AppState>) -> Json<crate::usage::UsageView> {
    let database = state.0.db.path.clone();
    let view = tokio::task::spawn_blocking(move || crate::usage::read(&database))
        .await
        .unwrap_or_else(|_| crate::usage::UsageView {
            processes: Vec::new(),
            storage: Vec::new(),
        });
    Json(view)
}

/// Serial devices plugged into this machine right now, for a settings field the schema marks
/// `"format": "serial-port"` (a Zigbee dongle, say) to offer as a live-updated list of
/// candidates, alongside the plain text box a device path always was. Read fresh each time, the
/// same reasoning as `host_info`: a device plugged in or removed while the form is open should
/// show up without reopening it.
async fn serial_ports() -> Json<Vec<String>> {
    Json(crate::serial::list())
}

/// The header only Irori's own page sends, on every change it asks for. It is not who you are:
/// that is the session cookie, checked for every address by `auth::guard`. It is there because
/// a browser sends the cookie along with a request whoever wrote the page that made it. A
/// cross-site `<form>` POST carries no headers of its own, and a fetch with a custom header is
/// stopped by the browser's CORS preflight, so a website somebody happens to have open can't
/// change the home on their cookie.
///
/// The guard asks for it on every change once the home is locked. Restart asks for it always,
/// a home with no owner yet included, where anything that can reach the server could send it;
/// what that buys there is capped by `RESTART_COOLDOWN`.
const UI_HEADER: &str = "x-irori-ui";

/// How long after an accepted restart the endpoint answers 429 instead of accepting another
/// one. A restart is an outage, so a caller who can reach this route at all should not be able to hammer it into a permanent one; this
/// bounds how often a boot can be taken down, and costs nothing for the real page — the button
/// is disabled while the restart is in flight anyway.
const RESTART_COOLDOWN: Duration = Duration::from_secs(30);

/// Restarts Irori, at the Settings page's request: wake the shutdown in `serve`
/// (crates/irori/src/main.rs), which stops the server and extensions cleanly and then starts
/// this very binary again. The button is on the Settings page because that's where someone who
/// can change how Irori runs is looking. A bare POST — the page's header missing — is refused
/// with what a caller would need to know to form a valid one, and so is a second request
/// inside `RESTART_COOLDOWN`.
async fn restart(State(state): State<AppState>, request: axum::extract::Request) -> Response {
    if !request
        .headers()
        .get(UI_HEADER)
        .is_some_and(|value| value == "1")
    {
        return refused(
            StatusCode::FORBIDDEN,
            format!("restart needs the `{UI_HEADER}: 1` header, which only the page sends"),
        );
    }
    {
        // A tiny critical section — read a clock, maybe write it — held across no await, so a
        // plain mutex rather than anything async.
        let mut last = state
            .0
            .last_restart
            .lock()
            .expect("a restart clock poisoned by a panic nobody handles");
        if last.is_some_and(|at| at.elapsed() < RESTART_COOLDOWN) {
            return refused(
                StatusCode::TOO_MANY_REQUESTS,
                "a restart was just asked for; Irori is still getting back up".to_owned(),
            );
        }
        *last = Some(Instant::now());
    }
    state.0.restarting.store(true, Ordering::SeqCst);
    tracing::info!("restart requested; shutting down");
    // notify_one, not notify_waiters: a non-retaining wake that finds no waiter is lost, and a
    // restart can be asked for before the graceful shutdown future has registered its waiter (a
    // request can reach the handler before the accept loop's first poll of it). notify_one keeps
    // the notification around until the waiter registers, so the shutdown always lands.
    state.0.restart.notify_one();
    // Accepted, not No Content: the restart itself happens a moment later, once this request
    // has drained.
    StatusCode::ACCEPTED.into_response()
}

/// The last day of an entity's changes, for the expandable table under its row on the Devices
/// page. Read from the recorder, so a restart still has it. Older than a day stays in the
/// database and is not part of this answer.
#[derive(Debug, Serialize)]
struct HistoryView {
    entity: EntityId,
    /// The changes, oldest first. Empty when the entity has no stored change in the last day.
    states: Vec<EntityState>,
    /// Who sent the command a change answered, by the `parent_id` of its context. Only a
    /// command a person or a program sent is here: a switch pressed on the wall, or a flow,
    /// is not. Whoever has the whole home is told who; a person with some rooms is told their
    /// own name, and "someone" for the rest.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    by: BTreeMap<ContextId, String>,
}

/// How many calls one answer looks up the sender of.
const HISTORY_BY_AT_MOST: usize = 500;

async fn entity_history(
    State(state): State<AppState>,
    Extension(reach): Extension<Reach>,
    Extension(who): Extension<auth::Actor>,
    Path(entity_id): Path<EntityId>,
) -> Response {
    // Outside a person's rooms it is answered as an entity that was never there.
    if state.0.core.seen(&reach).state(&entity_id).is_none() {
        return refused(
            StatusCode::NOT_FOUND,
            format!("there's no entity `{entity_id}`"),
        );
    }
    let states = state.0.history.for_entity(&entity_id);
    // The calls the changes answered, found in the activity log. At most as many as there are
    // changes in a day worth drawing.
    let calls: Vec<ContextId> = states
        .iter()
        .filter_map(|change| change.context.parent_id.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        // The newest: ids are in time order, and the changes people look at are the last ones.
        .rev()
        .take(HISTORY_BY_AT_MOST)
        .collect();
    let audit = state.0.audit.clone();
    let found = tokio::task::spawn_blocking(move || audit.who_for(&calls))
        .await
        .unwrap_or_default();
    let me = who.user.as_ref().map(|user| user.id.clone());
    let by = found
        .into_iter()
        .map(|(call, actor)| {
            let said = if reach.is_whole() || (me.is_some() && actor.person() == me.as_ref()) {
                actor.to_string()
            } else {
                "someone".to_owned()
            };
            (call, said)
        })
        .collect();
    Json(HistoryView {
        entity: entity_id.clone(),
        states,
        by,
    })
    .into_response()
}

/// How long history is kept. `summary_days` is absent when hourly summaries are kept.
#[derive(Debug, Serialize, Deserialize)]
struct RecorderBody {
    retain_days: u32,
    #[serde(default)]
    summary_days: Option<u32>,
}

async fn recorder_settings(State(state): State<AppState>) -> Json<RecorderBody> {
    let section = state.0.config.recorder().await;
    Json(RecorderBody {
        retain_days: section.retain_days,
        summary_days: section.summary_days,
    })
}

/// Replaces `[recorder]` and applies it to the running recorder. The rest of `irori.toml` is
/// not touched.
async fn set_recorder(State(state): State<AppState>, Json(body): Json<RecorderBody>) -> Response {
    let section = irori_config::RecorderSection {
        retain_days: body.retain_days,
        summary_days: body.summary_days,
    };
    match state
        .0
        .config
        .edit_recorder(&state.0.history, section)
        .await
    {
        Ok(()) => Json(body).into_response(),
        Err(EditError::Refused(why)) => refused(StatusCode::UNPROCESSABLE_ENTITY, why.to_string()),
        Err(EditError::Io(error)) => {
            tracing::error!(%error, "couldn't write the config directory");
            refused(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("couldn't write the config directory: {error}"),
            )
        }
    }
}

#[derive(Debug, Serialize)]
struct SummaryView {
    entity: EntityId,
    points: Vec<SummaryPointView>,
}

#[derive(Debug, Serialize)]
struct SummaryPointView {
    start: Timestamp,
    value: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max: Option<f64>,
}

/// Hourly summaries for one entity, from `since`. The detailed history endpoint is the last
/// day of changes; this is the longer chart.
async fn entity_summary(
    State(state): State<AppState>,
    Extension(reach): Extension<Reach>,
    Path(entity_id): Path<EntityId>,
    RawQuery(query): RawQuery,
) -> Response {
    if state.0.core.seen(&reach).state(&entity_id).is_none() {
        return refused(
            StatusCode::NOT_FOUND,
            format!("there's no entity `{entity_id}`"),
        );
    }
    let Some(since_text) = query_value(query.as_deref(), "since") else {
        return refused(
            StatusCode::UNPROCESSABLE_ENTITY,
            "say how far back, as since=2026-01-01T00:00:00Z".to_owned(),
        );
    };
    let since = match since_text.parse::<Timestamp>() {
        Ok(since) => since,
        Err(error) => return refused(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
    };
    let points = state
        .0
        .history
        .summaries(&entity_id, since)
        .into_iter()
        .map(|point| SummaryPointView {
            start: point.start,
            value: point.value,
            min: point.min,
            max: point.max,
        })
        .collect();
    Json(SummaryView {
        entity: entity_id,
        points,
    })
    .into_response()
}

/// One query parameter, percent-decoded. Absent when the query doesn't name it.
fn query_value(query: Option<&str>, name: &str) -> Option<String> {
    let query = query?;
    let prefix = format!("{name}=");
    query.split('&').find_map(|pair| {
        let value = pair.strip_prefix(&prefix)?;
        Some(percent_decode(value))
    })
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

// --- Rooms, names, and where things live ---------------------------------------------------
//
// Each of these changes a file in the config directory and then tells the core
// (`docs/specs/config.md`). None of them touch what a protocol reports: taking a name away
// gives the protocol's name back, rather than leaving whatever was on screen.

async fn areas(
    State(state): State<AppState>,
    Extension(reach): Extension<Reach>,
) -> Json<Vec<Area>> {
    Json(state.0.core.seen(&reach).areas())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AreaRequest {
    name: Name,
    #[serde(default)]
    floor: Option<FloorId>,
}

/// A change to a room: a new name, a floor (`null` for none), or both.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AreaEdit {
    #[serde(default)]
    name: Option<Name>,
    #[serde(default, deserialize_with = "patched")]
    floor: Patch<FloorId>,
}

/// A room on a floor that isn't there is refused; a room with no floor is fine.
fn floor_exists(settings: &irori_types::Settings, floor: Option<&FloorId>) -> Result<(), Refused> {
    match floor {
        Some(floor) if settings.floor(floor).is_none() => {
            Err(Refused(format!("there's no floor `{floor}`")))
        }
        _ => Ok(()),
    }
}

/// Makes a room. Two rooms may share a name — homes have two bathrooms — so the id, not the
/// name, is what has to be unique.
async fn add_area(State(state): State<AppState>, Json(request): Json<AreaRequest>) -> Response {
    let made = state
        .0
        .config
        .edit(&state.0.core, |settings| {
            floor_exists(settings, request.floor.as_ref())?;
            let area = Area {
                id: irori_core::new_area_id(&request.name, &settings.areas),
                name: request.name.clone(),
                floor_id: request.floor.clone(),
            };
            settings.areas.push(area.clone());
            settings.areas.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(area)
        })
        .await;
    match made {
        Ok(area) => (StatusCode::CREATED, Json(area)).into_response(),
        Err(e) => edit_failed(e),
    }
}

async fn edit_area(
    State(state): State<AppState>,
    Path(id): Path<AreaId>,
    Json(request): Json<AreaEdit>,
) -> Response {
    let renamed = state
        .0
        .config
        .edit(&state.0.core, |settings| {
            if let Some(floor) = &request.floor {
                floor_exists(settings, floor.as_ref())?;
            }
            let area = settings
                .areas
                .iter_mut()
                .find(|area| area.id == id)
                .ok_or_else(|| Refused(format!("there's no room `{id}`")))?;
            if let Some(name) = &request.name {
                area.name = name.clone();
            }
            if let Some(floor) = &request.floor {
                area.floor_id = floor.clone();
            }
            Ok(area.clone())
        })
        .await;
    match renamed {
        Ok(area) => Json(area).into_response(),
        Err(e) => edit_failed(e),
    }
}

/// Removes a room. Devices that were in it are left unplaced, and what was said about them is
/// kept: making the room again puts them back.
async fn remove_area(
    State(state): State<AppState>,
    Extension(trail): Extension<trail::Trail>,
    Path(id): Path<AreaId>,
) -> Response {
    // whole-home: removing a room is an owner's
    if !state.0.core.areas().iter().any(|area| area.id == id) {
        return refused(StatusCode::BAD_REQUEST, format!("there's no room `{id}`"));
    }
    // Everybody who had the room loses it first. A room made again under the same name gets the
    // same id, so a list left behind would give the old access back to whoever it names; and if
    // the second write fails, somebody has one room fewer than they should, never one more.
    let stripped = state
        .0
        .config
        .edit_people(&state.0.core, |people| {
            let mut had_it = Vec::new();
            for user in &mut people.users {
                if let Some(rooms) = &mut user.areas
                    && rooms.contains(&id)
                {
                    rooms.retain(|room| room != &id);
                    had_it.push((user.id.clone(), user.name.as_str().to_owned()));
                }
            }
            Ok(had_it)
        })
        .await;
    match stripped {
        // Their open pages were shown the home with that room in it. Closed once the list is
        // written, so the page that reconnects is served the new one.
        Ok(had_it) => {
            for (user, _) in &had_it {
                state.0.sockets.close_user_sessions(user, None);
            }
            // Said in the log, which an owner reads: whose rooms these were.
            if !had_it.is_empty() {
                let names: Vec<&str> = had_it.iter().map(|(_, name)| name.as_str()).collect();
                trail.detail(format!("no longer a room of {}", names.join(", ")));
            }
        }
        Err(e) => return edit_failed(e),
    }
    let removed = state
        .0
        .config
        .edit(&state.0.core, |settings| {
            let before = settings.areas.len();
            settings.areas.retain(|area| area.id != id);
            if settings.areas.len() == before {
                return Err(Refused(format!("there's no room `{id}`")));
            }
            Ok(())
        })
        .await;
    match removed {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => edit_failed(e),
    }
}

async fn floorplan(
    State(state): State<AppState>,
    Extension(reach): Extension<Reach>,
) -> Json<Floorplan> {
    Json(state.0.core.seen(&reach).floorplan())
}

/// Replaces the whole plan.
///
/// The whole thing at once, not a wall at a time: the editor holds a working copy while somebody
/// draws and sends it when they press Save, so a half-finished room never reaches the file, and
/// Cancel is simply not sending. It is also what makes the page's undo trivially correct — there
/// is nothing on the server to undo.
/// What saving a plan did beyond writing it down.
#[derive(Debug, Serialize)]
struct PlanSaved {
    /// Devices the plan moved into the room they are standing in.
    placed: usize,
}

async fn save_floorplan(State(state): State<AppState>, Json(plan): Json<Floorplan>) -> Response {
    // Which devices the home has heard of, including the ones it is holding back. Read before
    // the edit, because the edit only gets to see what the config directory says.
    let known: std::collections::BTreeSet<DeviceId> = state
        .0
        .core
        .devices()
        .into_iter()
        .map(|device| device.id)
        .chain(state.0.core.held_devices().into_iter().map(|held| held.id))
        .collect();
    let saved = state
        .0
        .config
        .edit(&state.0.core, |settings| {
            plan.check().map_err(|why| Refused(why.to_string()))?;
            // A plan written by hand, or by the assistant, may keep only copies of a device;
            // one of them is then where the device is, as the page would have made it.
            let mut plan = plan.clone();
            plan.keep_originals();
            settings.floorplan = plan;
            Ok(place_devices(settings, &known))
        })
        .await;
    match saved {
        Ok(placed) => Json(PlanSaved { placed }).into_response(),
        Err(e) => edit_failed(e),
    }
}

/// Puts each device that was drawn inside a room into that room, and says how many moved.
///
/// Dragging a device onto the kitchen floor is a person saying where it is, so it may answer the
/// question `devices.toml` asks — a plan that knew and didn't say would be a drawing rather than
/// part of the home. Two things it must not do.
///
/// It must not overrule a **deliberate** "not in a room" (`area = false`,
/// `docs/specs/config.md` §3.2). That answer exists precisely so that a guess — the device's own
/// `suggested_area` — can't put it back, and a dot standing on a floor is another guess.
///
/// And it must not place a device into a room that isn't there: rooms are made in `areas.toml`,
/// and a plan naming one that has since been deleted is kept rather than obeyed (§6).
///
/// `known` is every device the home has heard of, the ones it is holding back included. A plan
/// can name a device that no longer exists — entries outlive what they point at (§4) — and
/// writing a `devices.toml` entry for one would turn a stale drawing into a settings entry for a
/// device nobody has. A device that already has an entry counts as known: something has been
/// said about it, so there is nothing to invent.
fn place_devices(
    settings: &mut irori_types::Settings,
    known: &std::collections::BTreeSet<DeviceId>,
) -> usize {
    let rooms: Vec<(DeviceId, AreaId)> = settings
        .floorplan
        .floors
        .values()
        .flat_map(|level| {
            // A copy is a second marker for a device that is somewhere else, and a helper is no
            // device at all: only an original marker for a device says which room it is in.
            level
                .devices
                .iter()
                .filter(|placed| !placed.copy)
                .filter_map(|placed| {
                    let device = placed.device.as_ref().filter(|_| placed.entity.is_none())?;
                    if !known.contains(device) && !settings.devices.contains_key(device) {
                        return None;
                    }
                    let room = level.areas.iter().find(|area| area.contains(placed.at))?;
                    settings.area(&room.area)?;
                    Some((device.clone(), room.area.clone()))
                })
        })
        .collect();

    let mut moved = 0;
    for (device, room) in rooms {
        let settings = settings.devices.entry(device).or_default();
        if settings.area == Placement::Nowhere || settings.area == Placement::In(room.clone()) {
            continue;
        }
        settings.area = Placement::In(room);
        moved += 1;
    }
    moved
}

async fn floors(
    State(state): State<AppState>,
    Extension(reach): Extension<Reach>,
) -> Json<Vec<Floor>> {
    Json(state.0.core.seen(&reach).floors())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FloorRequest {
    name: Name,
    /// 0 is the entrance level; negative is below ground.
    #[serde(default)]
    level: i8,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FloorEdit {
    #[serde(default)]
    name: Option<Name>,
    #[serde(default)]
    level: Option<i8>,
}

async fn add_floor(State(state): State<AppState>, Json(request): Json<FloorRequest>) -> Response {
    let made = state
        .0
        .config
        .edit(&state.0.core, |settings| {
            let floor = Floor {
                id: irori_core::new_floor_id(&request.name, &settings.floors),
                name: request.name.clone(),
                level: request.level,
            };
            settings.floors.push(floor.clone());
            settings.floors.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(floor)
        })
        .await;
    match made {
        Ok(floor) => (StatusCode::CREATED, Json(floor)).into_response(),
        Err(e) => edit_failed(e),
    }
}

async fn edit_floor(
    State(state): State<AppState>,
    Path(id): Path<FloorId>,
    Json(request): Json<FloorEdit>,
) -> Response {
    let edited = state
        .0
        .config
        .edit(&state.0.core, |settings| {
            let floor = settings
                .floors
                .iter_mut()
                .find(|floor| floor.id == id)
                .ok_or_else(|| Refused(format!("there's no floor `{id}`")))?;
            if let Some(name) = &request.name {
                floor.name = name.clone();
            }
            if let Some(level) = request.level {
                floor.level = level;
            }
            Ok(floor.clone())
        })
        .await;
    match edited {
        Ok(floor) => Json(floor).into_response(),
        Err(e) => edit_failed(e),
    }
}

/// Removes a floor. Its rooms stay, on no floor; what they said about it is kept, so making the
/// floor again puts them back — as with rooms and their devices.
async fn remove_floor(State(state): State<AppState>, Path(id): Path<FloorId>) -> Response {
    let removed = state
        .0
        .config
        .edit(&state.0.core, |settings| {
            let before = settings.floors.len();
            settings.floors.retain(|floor| floor.id != id);
            if settings.floors.len() == before {
                return Err(Refused(format!("there's no floor `{id}`")));
            }
            Ok(())
        })
        .await;
    match removed {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => edit_failed(e),
    }
}

/// A field that can be set, cleared, or left alone: absent means "don't touch", `null` means
/// "clear it", and a value means "make it this".
type Patch<T> = Option<Option<T>>;

/// serde reads a plain `Option<Option<T>>` as `None` for both an absent field and a `null` one,
/// which loses exactly the distinction a PATCH needs.
fn patched<'de, T, D>(deserializer: D) -> Result<Patch<T>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

/// Where to put a device, in a PATCH body: `"hall"` for a room, `false` for deliberately no
/// room, `null` to go back to having said nothing (which lets the device's own suggestion stand
/// in again). Absent leaves it alone.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum WhereTo {
    In(AreaId),
    /// `false`. `true` would mean "yes, a room" without saying which, so it's refused.
    Nowhere(bool),
}

impl WhereTo {
    fn placement(&self) -> Result<Placement, Refused> {
        match self {
            WhereTo::In(area) => Ok(Placement::In(area.clone())),
            WhereTo::Nowhere(false) => Ok(Placement::Nowhere),
            WhereTo::Nowhere(true) => Err(Refused(
                "`area: true` doesn't say which room; send a room's id, `false` for no room, or \
                 null to leave it to the device"
                    .to_owned(),
            )),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceEdit {
    #[serde(default, deserialize_with = "patched")]
    name: Patch<Name>,
    #[serde(default, deserialize_with = "patched")]
    description: Patch<Description>,
    #[serde(default, deserialize_with = "patched")]
    area: Patch<WhereTo>,
    /// `true` adds a found device to the home ("+ Add device"). Taking one out is `DELETE`.
    #[serde(default)]
    added: Option<bool>,
}

async fn edit_device(
    State(state): State<AppState>,
    Path(id): Path<DeviceId>,
    Json(request): Json<DeviceEdit>,
) -> Response {
    let core = &state.0.core;
    // A found device isn't in the registry, but it's still one a person can add.
    // whole-home: adding or editing a device is an owner's
    let known = core.devices().iter().any(|device| device.id == id)
        || core.held_devices().iter().any(|device| device.id == id);
    if !known {
        return refused(StatusCode::NOT_FOUND, format!("there's no device `{id}`"));
    }
    let edited = state
        .0
        .config
        .edit(core, |settings| {
            let area = match request.area.clone() {
                None => None,
                // `null`: nobody has said, so the device's own suggestion may stand in again.
                Some(None) => Some(Placement::Unsaid),
                Some(Some(where_to)) => Some(where_to.placement()?),
            };
            if let Some(Placement::In(area)) = &area
                && settings.area(area).is_none()
            {
                return Err(Refused(format!("there's no room `{area}`")));
            }
            let device = settings.devices.entry(id.clone()).or_default();
            if let Some(name) = request.name.clone() {
                device.name = name;
            }
            if let Some(description) = request.description.clone() {
                device.description = description;
            }
            if let Some(added) = request.added {
                device.added = added;
            }
            if let Some(area) = area {
                device.area = area;
            }
            Ok(())
        })
        .await;
    match edited {
        // The device as it now is, so the page doesn't have to guess what the change produced.
        // whole-home: the owner's edit answers with the device
        Ok(()) => Json(core.devices().into_iter().find(|device| device.id == id)).into_response(),
        Err(e) => edit_failed(e),
    }
}

/// How many seconds after an unpair went unanswered to keep watching for it happening anyway.
/// As long as the Zigbee extension keeps an unanswered unpair itself.
const LATE_UNPAIR_CHECKS: u32 = 120;

/// Removes a device from the home: its name, room, entities, floorplan spot and history go, and
/// it's back among what its protocol has found, to be added again from "+ Add device" if wanted
/// (`docs/specs/config.md` §3.2).
///
/// A device whose protocol keeps a network of its own (Zigbee) is unpaired from it first, and
/// then it's gone altogether rather than found: it has to join again to come back. That one can
/// be removed while it's only found, too, which is the way to undo pairing the wrong thing. If
/// the unpair is refused nothing else is touched, and the answer says `unpair_failed` so the
/// page can offer `?force=true`. Forced, a device in the home leaves it whatever its network
/// says, so removing never becomes impossible.
async fn remove_device(
    State(state): State<AppState>,
    Path(id): Path<DeviceId>,
    RawQuery(query): RawQuery,
) -> Response {
    // For a device that didn't answer a plain unpair: drop it from its network anyway.
    let force = query.as_deref() == Some("force=true");
    let core = &state.0.core;
    // whole-home: removing a device is an owner's
    let in_home = core.devices().iter().any(|device| device.id == id);
    let found = core.held_devices().iter().any(|device| device.id == id);
    let unpairs = core.unpairs(&id);
    if !in_home && !(found && unpairs) {
        return refused(StatusCode::NOT_FOUND, format!("there's no device `{id}`"));
    }
    if !unpairs {
        return match state.0.config.forget_device(core, &id).await {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(e) => edit_failed(e),
        };
    }
    // Read before the unpair: afterwards the core no longer knows what the device had.
    let owned = core.device_entity_keys(&id).unwrap_or_default();
    let unpaired = core.unpair_device(&id, force).await;
    // Removing a device from the home always has a way through. Forced, it leaves the home
    // even when its network can't be asked at all — the extension stopped, the dongle
    // unplugged — which is exactly when someone wants to clear devices out. It's then found
    // again if its protocol still knows it, as any removed device used to be.
    if let Err(why) = &unpaired
        && force
        && in_home
    {
        tracing::warn!(device = %id, %why, "removed from the home without being unpaired");
    } else if let Err(why) = unpaired {
        // Giving up on the answer isn't the network giving up on the request. If it lets the
        // device go after all, the protocol takes it out of the core, and what Irori wrote
        // down about it would be left behind with no device to remove it through. So for a
        // while longer this watches for it going, and tidies up then.
        if in_home && matches!(why, irori_core::CallActionError::Timeout) {
            let (state, id, owned) = (state.clone(), id.clone(), owned.clone());
            tokio::spawn(async move {
                let core = &state.0.core;
                for _ in 0..LATE_UNPAIR_CHECKS {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    // whole-home: removing a device is an owner's
                    let gone = !core.devices().iter().any(|device| device.id == id)
                        && !core.held_devices().iter().any(|device| device.id == id);
                    if gone {
                        if let Err(e) = state.0.config.forget_unpaired(core, &id, owned).await {
                            tracing::warn!(device = %id, error = ?e, "couldn't forget a device unpaired late");
                        }
                        return;
                    }
                }
            });
        }
        #[derive(Debug, Serialize)]
        struct UnpairFailed {
            error: String,
            code: &'static str,
        }
        let failed = UnpairFailed {
            error: why.to_string(),
            code: "unpair_failed",
        };
        return (StatusCode::CONFLICT, Json(failed)).into_response();
    }
    if !in_home {
        // Only found, so there was nothing of Irori's to remove: unpairing was the whole job.
        // Still there means its protocol had nothing to unpair (Zigbee's own controller).
        let still = core.held_devices().iter().any(|device| device.id == id);
        return if still {
            refused(
                StatusCode::CONFLICT,
                "It isn't a device that pairs, so there's nothing to unpair. It stays listed \
                 here until it's added."
                    .to_owned(),
            )
        } else {
            StatusCode::NO_CONTENT.into_response()
        };
    }
    match state.0.config.forget_unpaired(core, &id, owned).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => edit_failed(e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntityEdit {
    #[serde(default, deserialize_with = "patched")]
    name: Patch<Name>,
}

async fn edit_entity(
    State(state): State<AppState>,
    Path(id): Path<EntityId>,
    Json(request): Json<EntityEdit>,
) -> Response {
    let core = &state.0.core;
    let Some(key) = core.entity_key(&id) else {
        return refused(StatusCode::NOT_FOUND, format!("there's no entity `{id}`"));
    };
    // A helper's name lives where the helper is defined. Writing it to entities.toml as well
    // would give it two names, one in each file (ROADMAP D36).
    if key.protocol.as_str() == HELPERS.as_str()
        && let Some((kind, helper, main)) =
            irori_helpers::HelperKind::of_unique(key.unique_id.as_str())
    {
        // A button's event is named after its button: there's one name to change.
        if !main {
            return refused(
                StatusCode::BAD_REQUEST,
                format!("it's named after its {}; rename that", kind.word),
            );
        }
        let Some(Some(name)) = request.name.clone() else {
            return refused(
                StatusCode::BAD_REQUEST,
                "a helper always has a name; send the new one".to_owned(),
            );
        };
        let helper = helper.to_owned();
        let renamed = state
            .0
            .config
            .edit_extension(core, &HELPERS, |file| {
                let entry = file
                    .get_mut(kind.table)
                    .and_then(|table| table.get_mut(&helper))
                    .and_then(serde_json::Value::as_object_mut)
                    .ok_or_else(|| Refused(format!("there's no {} `{helper}`", kind.word)))?;
                entry.insert("name".into(), serde_json::json!(name.as_str()));
                Ok(())
            })
            .await;
        return match renamed {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(e) => edit_failed(e),
        };
    }
    let edited = state
        .0
        .config
        .edit(core, |settings| {
            if let Some(name) = request.name.clone() {
                settings.entities.entry(key).or_default().name = name;
            }
            Ok(())
        })
        .await;
    match edited {
        // whole-home: the owner's edit answers with the entity
        Ok(()) => Json(core.entities().into_iter().find(|entity| entity.id == id)).into_response(),
        Err(e) => edit_failed(e),
    }
}

/// The helpers extension's id: its helpers are kept in `extensions/helpers.toml`.
static HELPERS: LazyLock<ExtensionId> =
    LazyLock::new(|| ExtensionId::try_from("helpers").expect("a valid extension id"));

/// The kind of helper a path names (`toggles`, `numbers`…), or the refusal for one there isn't.
fn helper_kind(table: &str) -> Result<&'static irori_helpers::HelperKind, Box<Response>> {
    irori_helpers::HelperKind::of_table(table).ok_or_else(|| {
        let kinds: Vec<&str> = irori_helpers::KINDS.iter().map(|kind| kind.table).collect();
        Box::new(refused(
            StatusCode::NOT_FOUND,
            format!(
                "there are no helpers called `{table}`; there are: {}",
                kinds.join(", ")
            ),
        ))
    })
}

/// Every helper, as it's written in `extensions/helpers.toml`: its kind, its id, its entity,
/// and its settings.
async fn list_helpers(State(state): State<AppState>) -> Response {
    let file = state.0.config.extension_settings(&HELPERS).await;
    let mut helpers = Vec::new();
    for kind in irori_helpers::KINDS {
        let Some(table) = file.get(kind.table).and_then(serde_json::Value::as_object) else {
            continue;
        };
        for (id, settings) in table {
            let entities = irori_types::ObjectId::try_from(id.as_str())
                .ok()
                .and_then(|id| kind.check(&id, settings).ok())
                .unwrap_or_default();
            let mut helper = serde_json::json!({
                "kind": kind.table,
                "id": id,
                "entity_id": entities.first(),
                "entities": entities,
            });
            if let (Some(helper), Some(settings)) = (helper.as_object_mut(), settings.as_object()) {
                for (key, value) in settings {
                    helper.entry(key.clone()).or_insert_with(|| value.clone());
                }
            }
            helpers.push(helper);
        }
    }
    Json(helpers).into_response()
}

/// Makes a helper: `POST /api/helpers/numbers {"name": "Evening brightness", "min": 0, "max":
/// 100}`. Its id comes from the name and never changes; its entity is `<kind>.<id>`.
async fn add_helper(
    State(state): State<AppState>,
    Path(table): Path<String>,
    Json(settings): Json<serde_json::Map<String, serde_json::Value>>,
) -> Response {
    let kind = match helper_kind(&table) {
        Ok(kind) => kind,
        Err(refusal) => return *refusal,
    };
    let name = match settings.get("name").map(Name::deserialize) {
        Some(Ok(name)) => name,
        Some(Err(e)) => return refused(StatusCode::BAD_REQUEST, format!("`name`: {e}")),
        None => {
            return refused(
                StatusCode::BAD_REQUEST,
                format!("a {} needs a `name`", kind.word),
            );
        }
    };
    let taken: Vec<String> = state
        .0
        .core
        .entities()
        .iter()
        .map(|entity| entity.id.to_string())
        .collect();
    let settings = serde_json::Value::Object(settings);
    let made = state
        .0
        .config
        .edit_extension(&state.0.core, &HELPERS, |file| {
            let helpers = file
                .entry(kind.table)
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
                .ok_or_else(|| {
                    Refused(format!(
                        "`{}` in extensions/helpers.toml isn't a table",
                        kind.table
                    ))
                })?;
            let (id, entities) =
                free_helper_id(kind, &name, &settings, helpers, &taken).map_err(Refused)?;
            helpers.insert(id.clone(), settings.clone());
            irori_helpers::check_settings(file).map_err(Refused)?;
            Ok((id, entities))
        })
        .await;
    match made {
        Ok((id, entities)) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "kind": kind.table,
                "id": id,
                "entity_id": entities.first(),
                "entities": entities,
            })),
        )
            .into_response(),
        Err(e) => edit_failed(e),
    }
}

/// The id a new helper called `name` gets: one no helper of its kind has, and whose entities
/// nothing else in the home is (`name`, then `name_2`, …). Making one and checking one both
/// ask this, so a check says what Create will make.
fn free_helper_id(
    kind: &irori_helpers::HelperKind,
    name: &Name,
    settings: &serde_json::Value,
    helpers: &serde_json::Map<String, serde_json::Value>,
    taken: &[String],
) -> Result<(String, Vec<String>), String> {
    let base = irori_core::new_area_id(name, &[]).to_string();
    for n in 1..=1000 {
        let id = if n == 1 {
            base.clone()
        } else {
            format!("{base}_{n}")
        };
        if helpers.contains_key(&id) {
            continue;
        }
        let Ok(object) = irori_types::ObjectId::try_from(id.as_str()) else {
            continue;
        };
        // Wrong whatever it's called: say so rather than try a thousand ids.
        let entities = kind.check(&object, settings)?;
        if !entities.iter().any(|entity| taken.contains(entity)) {
            return Ok((id, entities));
        }
    }
    Err(format!("no id is free for `{name}`"))
}

/// What a helper would be like, before it's made: its entity, and the states it would have
/// had over the last day (one worked out from other entities, played over their history) or
/// would start in (one a person sets). The same body as making one; a name isn't needed yet.
/// For the builder to draw and to try.
async fn preview_helper(
    State(state): State<AppState>,
    Path(table): Path<String>,
    Json(mut settings): Json<serde_json::Map<String, serde_json::Value>>,
) -> Response {
    let kind = match helper_kind(&table) {
        Ok(kind) => kind,
        Err(refusal) => return *refusal,
    };
    let name = settings
        .get("name")
        .and_then(|name| name.as_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("Preview")
        .to_owned();
    settings.insert("name".into(), serde_json::json!(name));
    let Ok(parsed) = name.parse::<Name>() else {
        return refused(StatusCode::BAD_REQUEST, format!("`{name}` can't be a name"));
    };
    let id = irori_core::new_area_id(&parsed, &[]).to_string();
    let Ok(object_id) = irori_types::ObjectId::try_from(id.as_str()) else {
        return refused(
            StatusCode::BAD_REQUEST,
            format!("`{name}` can't be made into an id"),
        );
    };
    let core = &state.0.core;
    // whole-home: previewing a helper is an owner's
    let entities = core.entities();
    let history = state.0.history.clone();
    let live = core.clone();
    let time_zone = core.place().time_zone;
    let now = core.now();
    let settings = serde_json::Value::Object(settings);
    // The recorder answers a range; the reading that held as it began came a little before.
    let changes = move |entity: &irori_types::EntityId, since: irori_types::Timestamp| {
        let seeded = irori_types::Timestamp::from_jiff(
            since.as_jiff() - jiff::SignedDuration::from_hours(6),
        );
        let mut changes = irori_core::HistorySource::changes(&history, entity, seeded);
        // And where it is now, which the recorder may not have written yet.
        if let Some(current) = live.state(entity)
            && changes
                .last()
                .is_none_or(|last| last.last_updated < current.last_updated)
        {
            changes.push(current);
        }
        changes
    };
    // Each read of the history waits off the runtime (`history.rs`).
    let preview = kind.preview(
        &object_id,
        &settings,
        &entities,
        &changes,
        now,
        time_zone.as_ref().map(|zone| zone.as_str()),
    );
    let preview = match preview {
        Ok(preview) => preview,
        Err(why) => return refused(StatusCode::BAD_REQUEST, why),
    };
    let Ok(entity_id) = irori_types::EntityId::try_from(preview.entity_id.as_str()) else {
        return refused(
            StatusCode::BAD_REQUEST,
            "it makes no entity a page can show".to_owned(),
        );
    };
    let context = core.new_context(irori_types::Origin::System);
    let states: Vec<irori_types::EntityState> = preview
        .said
        .into_iter()
        .map(|said| {
            let mut attributes = irori_types::Attributes::new();
            if let Some(problem) = said.problem
                && let Ok(key) = irori_types::AttributeKey::try_from("problem")
            {
                attributes.insert(key, serde_json::json!(problem));
            }
            irori_types::EntityState {
                entity_id: entity_id.clone(),
                availability: irori_types::Availability::Available,
                state: said.state,
                attributes,
                last_changed: said.at,
                last_updated: said.at,
                last_reported: said.at,
                context: context.clone(),
            }
        })
        .collect();
    Json(serde_json::json!({
        "entity": {
            "id": entity_id,
            "protocol": HELPERS.as_str(),
            "unique_id": format!("preview-{id}"),
            "name": name,
            "capabilities": preview.capabilities,
        },
        "states": states,
        "computed": preview.computed,
    }))
    .into_response()
}

/// What a helper would be, without making it: the entities it would become, or what's wrong
/// with it, in the words making it would refuse with. `POST /api/helpers/thresholds/check`
/// with the same body as making one. For a form to say as it's filled in.
async fn check_helper(
    State(state): State<AppState>,
    Path(table): Path<String>,
    Json(settings): Json<serde_json::Map<String, serde_json::Value>>,
) -> Response {
    let kind = match helper_kind(&table) {
        Ok(kind) => kind,
        Err(refusal) => return *refusal,
    };
    let name = match settings.get("name").map(Name::deserialize) {
        Some(Ok(name)) => name,
        Some(Err(e)) => return refused(StatusCode::BAD_REQUEST, format!("`name`: {e}")),
        None => {
            return refused(
                StatusCode::BAD_REQUEST,
                format!("a {} needs a name", kind.word),
            );
        }
    };
    let taken: Vec<String> = state
        .0
        .core
        .entities()
        .iter()
        .map(|entity| entity.id.to_string())
        .collect();
    let settings = serde_json::Value::Object(settings);
    let mut file = state.0.config.extension_settings(&HELPERS).await;
    let Some(helpers) = file
        .entry(kind.table)
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
    else {
        return refused(
            StatusCode::BAD_REQUEST,
            format!("`{}` in extensions/helpers.toml isn't a table", kind.table),
        );
    };
    // The id Create would give it, and the plain one it would have liked, to say why not.
    let (id, entities) = match free_helper_id(kind, &name, &settings, helpers, &taken) {
        Ok(found) => found,
        Err(why) => return refused(StatusCode::BAD_REQUEST, why),
    };
    let plain = irori_core::new_area_id(&name, &[]).to_string();
    let taken: Vec<String> = if id == plain {
        Vec::new()
    } else {
        irori_types::ObjectId::try_from(plain.as_str())
            .ok()
            .and_then(|object| kind.check(&object, &settings).ok())
            .unwrap_or_default()
    };
    // With the rest of the file, for what one helper alone can't show (a ring).
    helpers.insert(id.clone(), settings);
    if let Err(why) = irori_helpers::check_settings(&file) {
        return refused(StatusCode::BAD_REQUEST, why);
    }
    Json(serde_json::json!({
        "kind": kind.table,
        "id": id,
        "entity_id": entities.first(),
        "entities": entities,
        "taken": taken,
    }))
    .into_response()
}

/// Changes a helper's settings: the fields sent replace what it had, and `null` takes one
/// away. `PATCH /api/helpers/numbers/evening_brightness {"max": 80}`. Its id and its entity
/// stay; a value it can no longer have goes back to its `initial`.
async fn edit_helper(
    State(state): State<AppState>,
    Path((table, id)): Path<(String, String)>,
    Json(changes): Json<serde_json::Map<String, serde_json::Value>>,
) -> Response {
    let kind = match helper_kind(&table) {
        Ok(kind) => kind,
        Err(refusal) => return *refusal,
    };
    let edited = state
        .0
        .config
        .edit_extension(&state.0.core, &HELPERS, |file| {
            let entry = file
                .get_mut(kind.table)
                .and_then(|table| table.get_mut(&id))
                .and_then(serde_json::Value::as_object_mut)
                .ok_or_else(|| Refused(format!("there's no {} `{id}`", kind.word)))?;
            for (key, value) in &changes {
                if value.is_null() {
                    entry.remove(key);
                } else {
                    entry.insert(key.clone(), value.clone());
                }
            }
            let object_id =
                irori_types::ObjectId::try_from(id.as_str()).map_err(|e| Refused(e.to_string()))?;
            kind.check(&object_id, &serde_json::Value::Object(entry.clone()))
                .map_err(Refused)?;
            irori_helpers::check_settings(file).map_err(Refused)
        })
        .await;
    match edited {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => edit_failed(e),
    }
}

async fn remove_helper(
    State(state): State<AppState>,
    Path((table, id)): Path<(String, String)>,
) -> Response {
    let kind = match helper_kind(&table) {
        Ok(kind) => kind,
        Err(refusal) => return *refusal,
    };
    let removed = state
        .0
        .config
        .edit_extension(&state.0.core, &HELPERS, |file| {
            let gone = file
                .get_mut(kind.table)
                .and_then(serde_json::Value::as_object_mut)
                .and_then(|helpers| helpers.remove(&id));
            gone.map(|_| ())
                .ok_or_else(|| Refused(format!("there's no {} `{id}`", kind.word)))
        })
        .await;
    match removed {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => edit_failed(e),
    }
}

/// An extension's icon. Served with a policy that forbids everything an image doesn't need, so
/// an icon from a third-party extension can't run script even when opened on its own — and the
/// page only ever shows it through `<img>`, where it couldn't anyway.
async fn extension_icon(State(state): State<AppState>, Path(id): Path<ExtensionId>) -> Response {
    match state.0.core.extension_icon(&id) {
        Some(svg) => (
            [
                (axum::http::header::CONTENT_TYPE, "image/svg+xml"),
                (
                    axum::http::header::CONTENT_SECURITY_POLICY,
                    "default-src 'none'; style-src 'unsafe-inline'; sandbox",
                ),
                (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                // Compiled into this binary, so it can't change while this build runs.
                (axum::http::header::CACHE_CONTROL, "max-age=3600"),
            ],
            svg,
        )
            .into_response(),
        None => refused(StatusCode::NOT_FOUND, format!("`{id}` has no icon")),
    }
}

/// What a camera sees now, as its protocol draws it (`docs/specs/api.md`). Never cached, and
/// served so that opening it on its own can't run anything: a protocol may send an SVG.
async fn camera_picture(
    State(state): State<AppState>,
    Extension(reach): Extension<Reach>,
    Path(id): Path<EntityId>,
) -> Response {
    if id.kind() != irori_types::EntityKind::Camera {
        return refused(StatusCode::NOT_FOUND, format!("`{id}` isn't a camera"));
    }
    if state.0.core.seen(&reach).state(&id).is_none() {
        return refused(StatusCode::NOT_FOUND, format!("there's no camera `{id}`"));
    }
    match state.0.core.camera_picture(&id).await {
        Ok(picture) => (
            [
                (axum::http::header::CONTENT_TYPE, picture.content_type),
                (
                    axum::http::header::CONTENT_SECURITY_POLICY,
                    "default-src 'none'; style-src 'unsafe-inline'; sandbox".to_owned(),
                ),
                (
                    axum::http::header::X_CONTENT_TYPE_OPTIONS,
                    "nosniff".to_owned(),
                ),
                (axum::http::header::CACHE_CONTROL, "no-store".to_owned()),
            ],
            picture.bytes,
        )
            .into_response(),
        Err(irori_core::CallActionError::Timeout) => refused(
            StatusCode::GATEWAY_TIMEOUT,
            format!("`{id}` didn't send a picture in time"),
        ),
        Err(irori_core::CallActionError::NotRunning(extension)) => refused(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("`{extension}` isn't running, so `{id}` can't send a picture"),
        ),
        Err(irori_core::CallActionError::Failed(why)) => {
            refused(StatusCode::BAD_GATEWAY, format!("`{id}`: {why}"))
        }
    }
}

/// What an extension has lately said for itself: the tail of its own stderr, oldest line first.
///
/// Every failure a person can see should be one they can act on, and an extension's own output
/// is usually the only place the real reason is written down (`docs/specs/extensions.md` §8).
/// The card's reason line carries the last of these; this is the rest of them, for when one line
/// isn't enough.
///
/// Always a 200, even for an extension that has said nothing or isn't installed: "nothing to
/// show" is an answer, and a 404 here would make the page decide whether an empty log is an
/// error. A built-in has no process of its own and so never has anything here.
async fn extension_log(State(state): State<AppState>, Path(id): Path<ExtensionId>) -> Response {
    axum::Json(serde_json::json!({ "lines": state.0.core.log(&id) })).into_response()
}

/// Every running protocol's networks (`docs/specs/protocols.md` §6.8), for the Devices page's
/// network map, which asks again every few seconds while it's showing. Each network says which
/// extension drew it; the page joins them into one home.
async fn home_network(State(state): State<AppState>) -> Json<HomeNetwork> {
    Json(HomeNetwork {
        networks: state
            .0
            .core
            .networks()
            .into_iter()
            .flat_map(|(extension, networks)| {
                networks.into_iter().map(move |network| DrawnNetwork {
                    extension: extension.clone(),
                    network,
                })
            })
            .collect(),
    })
}

/// What `GET /api/network` answers.
#[derive(Debug, Serialize)]
struct HomeNetwork {
    networks: Vec<DrawnNetwork>,
}

/// One network, and the extension that drew it.
#[derive(Debug, Serialize)]
struct DrawnNetwork {
    extension: ExtensionId,
    #[serde(flatten)]
    network: irori_types::Network,
}

/// What Irori itself has said lately: the tail of this process's own log, oldest line first.
///
/// The same window an extension gets (`extension_log`), for the words Irori writes rather than
/// the ones it forwards — which is where a refused write, a taken port, an unauthenticated bind,
/// or a config edit that can't apply ends up. Extension output is in here too, tagged with the
/// extension it came from, because the core logs a line for each (`irori_core`'s `log_line`).
///
/// Deliberately a window and not an archive: the last [`syslog::LINES_KEPT`] lines of this
/// process, in memory, gone on restart. Irori normally writes the whole log to stdout as well —
/// `journalctl -u irori`, `docker logs`, or the terminal it was started from — and this is what
/// makes the same lines reachable from the page when nothing is watching that.
///
/// Always a 200, and the same `{"lines": …}` shape as an extension's, so one window component
/// reads both. "Nothing to show" is an answer: it means nothing has been logged yet, which is
/// what a `log_level` above `info` looks like.
async fn system_log(State(state): State<AppState>) -> Response {
    axum::Json(serde_json::json!({ "lines": state.0.log.lines() })).into_response()
}

/// An extension's own settings, given as one JSON object matching its `config_schema` — the
/// generic form behind the gear icon on its card (ROADMAP M1.6). Unlike [`give_secret`], this
/// isn't limited to a path an extension is currently asking for: it's scoped instead by only
/// ever accepting keys the extension's own schema actually declares, so it can't become "write
/// anything into anyone's settings" (`docs/specs/config.md` §3.6) — just this one extension's own
/// known fields. A `writeOnly` field (`docs/specs/config.md` §3.4: secrets) goes to
/// `secrets.toml`; everything else goes to `extensions/<id>.toml`.
async fn set_extension_settings(
    State(state): State<AppState>,
    Path(id): Path<ExtensionId>,
    Json(given): Json<serde_json::Map<String, serde_json::Value>>,
) -> Response {
    let core = &state.0.core;
    let Some(overview) = core.extensions().remove(&id) else {
        return refused(
            StatusCode::NOT_FOUND,
            format!("there's no extension `{id}`"),
        );
    };
    match save_extension_settings(&state, &id, &overview, given).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(refusal) => *refusal,
    }
}

/// Writes some of an extension's settings, checked against its `config_schema`, as one write: a
/// `writeOnly` field goes to `secrets.toml`, the rest to `extensions/<id>.toml`. Used by the
/// settings form and by an action's way that carries settings.
async fn save_extension_settings(
    state: &AppState,
    id: &ExtensionId,
    overview: &irori_core::ExtensionOverview,
    given: serde_json::Map<String, serde_json::Value>,
) -> Result<(), Box<Response>> {
    let core = &state.0.core;
    let Some(schema) = overview
        .info
        .as_ref()
        .and_then(|info| info.config_schema.as_ref())
    else {
        return Err(Box::new(refused(
            StatusCode::BAD_REQUEST,
            format!("`{id}` has no settings to configure"),
        )));
    };
    let Some(properties) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return Err(Box::new(refused(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("`{id}`'s settings schema has no properties"),
        )));
    };

    let mut non_secret = serde_json::Map::new();
    let mut secret_fields = Vec::new();
    for (key, value) in given {
        let Some(field_schema) = properties.get(&key) else {
            return Err(Box::new(refused(
                StatusCode::BAD_REQUEST,
                format!("`{key}` isn't one of `{id}`'s settings"),
            )));
        };
        if is_write_only(field_schema, schema) {
            secret_fields.push((key, value));
        } else {
            non_secret.insert(key, value);
        }
    }

    // Checked before either half is written: a bad secret value must not leave the non-secret
    // half saved while the secret half fails, half-applying the request.
    let mut secrets_to_set = Vec::new();
    for (key, value) in secret_fields {
        let Some(text) = value.as_str() else {
            return Err(Box::new(refused(
                StatusCode::BAD_REQUEST,
                format!("`{key}` must be given as text"),
            )));
        };
        secrets_to_set.push((key, text.to_owned()));
    }

    // One restart for the whole form. Writing the settings file and then the secrets restarts
    // the extension in between, and Zigbee2MQTT would generate a network key in that gap,
    // before the key from the form arrived. A form with only one of the two is already a
    // single write.
    let saved = if !non_secret.is_empty() && !secrets_to_set.is_empty() {
        state
            .0
            .config
            .edit_extension_with_secrets(
                core,
                id,
                |file| {
                    write_non_secrets(file, &non_secret);
                    Ok(())
                },
                &secrets_to_set,
            )
            .await
    } else if !non_secret.is_empty() {
        state
            .0
            .config
            .edit_extension(core, id, |file| {
                write_non_secrets(file, &non_secret);
                Ok(())
            })
            .await
    } else if !secrets_to_set.is_empty() {
        state
            .0
            .config
            .edit_secrets(core, |secrets| {
                for (key, text) in &secrets_to_set {
                    secrets
                        .set(id, std::slice::from_ref(key), text.clone())
                        .map_err(|e| Refused(e.to_string()))?;
                }
                Ok(())
            })
            .await
    } else {
        Ok(())
    };
    saved.map_err(|e| Box::new(edit_failed(e)))
}

/// Puts the non-secret half of a settings form into the extension's file.
///
/// TOML has no `null`: a schema-valid `null` for an `Option<T>` field means "leave this unset",
/// which in the file is the key's absence, not a value.
fn write_non_secrets(
    file: &mut serde_json::Map<String, serde_json::Value>,
    non_secret: &serde_json::Map<String, serde_json::Value>,
) {
    for (key, value) in non_secret {
        if value.is_null() {
            file.remove(key);
        } else {
            file.insert(key.clone(), value.clone());
        }
    }
}

/// Triggers one of an extension's declared actions — the button behind "+ Add device" for a
/// protocol that has one, e.g. Zigbee's `permit_join`. Checked against both what the extension
/// *declares* (its manifest) and what it says is *usable right now* (`available_actions`), the
/// same static/dynamic split the rest of the extension model uses.
async fn trigger_extension_action(
    State(state): State<AppState>,
    Path((id, action_id)): Path<(ExtensionId, String)>,
    body: Option<Json<Option<ActionBody>>>,
) -> Response {
    let core = &state.0.core;
    let Some(overview) = core.extensions().remove(&id) else {
        return refused(
            StatusCode::NOT_FOUND,
            format!("there's no extension `{id}`"),
        );
    };
    let declared = overview
        .info
        .as_ref()
        .map(|info| info.actions.as_slice())
        .unwrap_or(&[]);
    let Some(action) = declared.iter().find(|action| action.id == action_id) else {
        return refused(
            StatusCode::NOT_FOUND,
            format!("`{id}` has no `{action_id}` action"),
        );
    };
    let body = body.and_then(|Json(body)| body).unwrap_or_default();
    if !overview.available_actions.contains(&action_id) {
        return refused(
            StatusCode::CONFLICT,
            format!("`{action_id}` isn't available on `{id}` right now"),
        );
    }
    let (filled, settings) = match check_filled(action, body) {
        Ok(checked) => checked,
        Err(why) => return refused(StatusCode::BAD_REQUEST, why),
    };

    if !settings.is_empty() {
        // The settings come first, and the action waits for the extension to start again with
        // them: a Wi-Fi password typed into the form must be what the device is given.
        let before = core.extension_settings_of(&id);
        let starts = overview.starts;
        if let Err(refusal) = save_extension_settings(&state, &id, &overview, settings).await {
            return *refusal;
        }
        if core.extension_settings_of(&id) != before
            && let Err(why) = restarted_with_action(core, &id, &action_id, starts).await
        {
            return refused(StatusCode::CONFLICT, why);
        }
    }

    match core.call_action(&id, &action_id, filled).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(why) => refused(StatusCode::BAD_REQUEST, why.to_string()),
    }
}

/// How long an action whose settings changed waits for the extension to be back. Matter starts
/// its server in a few seconds; a first run that downloads Node.js is the slow case.
const RESTART_FOR_ACTION: Duration = Duration::from_secs(60);

/// Waits until `id` has started again since `starts` and offers `action_id` once more. Says why
/// not when it stopped short: it failed, it needs a setting, or it took too long.
async fn restarted_with_action(
    core: &Core,
    id: &ExtensionId,
    action_id: &str,
    starts: u64,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + RESTART_FOR_ACTION;
    loop {
        if let Some(overview) = core.extensions().remove(id)
            && overview.starts > starts
        {
            match &overview.status {
                irori_core::ExtensionStatus::Failed { reason, .. } => {
                    return Err(format!(
                        "the settings were saved, but `{id}` stopped: {reason}"
                    ));
                }
                irori_core::ExtensionStatus::NeedsSetup { missing } => {
                    return Err(format!(
                        "the settings were saved, but `{id}` still needs {}",
                        missing.join(", ")
                    ));
                }
                _ if overview.available_actions.iter().any(|a| a == action_id) => {
                    return Ok(());
                }
                _ => {}
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "the settings were saved, but `{id}` wasn't ready again within {} seconds: try again in a moment",
                RESTART_FOR_ACTION.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Checks what came with an action against what it declares: the way, every one of its
/// fields, and only the settings it lists. Gives back the fields for the extension and the
/// settings to save first.
fn check_filled(
    action: &irori_types::ProtocolAction,
    body: ActionBody,
) -> Result<
    (
        irori_protocol::Filled,
        serde_json::Map<String, serde_json::Value>,
    ),
    String,
> {
    let ActionBody {
        way,
        fields,
        settings,
    } = body;
    if action.ways.is_empty() {
        if way.is_some() || !fields.is_empty() || !settings.is_empty() {
            return Err(format!(
                "`{}` is a plain button: it takes nothing",
                action.id
            ));
        }
        return Ok((irori_protocol::Filled::default(), settings));
    }
    let chosen = match (way, action.ways.as_slice()) {
        (Some(way), ways) => ways
            .iter()
            .find(|w| w.id == way)
            .ok_or_else(|| format!("`{}` has no way `{way}`", action.id))?,
        (None, [only]) => only,
        (None, _) => return Err("choose one of the ways first".to_owned()),
    };
    if let Some(stray) = fields
        .keys()
        .find(|key| !chosen.fields.iter().any(|f| &f.id == *key))
    {
        return Err(format!("\"{}\" has no field `{stray}`", chosen.label));
    }
    if let Some(stray) = settings
        .keys()
        .find(|key| !chosen.settings.iter().any(|s| s == *key))
    {
        return Err(format!("\"{}\" doesn't change `{stray}`", chosen.label));
    }
    let mut filled = irori_protocol::Filled {
        way: Some(chosen.id.clone()),
        fields: BTreeMap::new(),
    };
    for field in &chosen.fields {
        let text = fields.get(&field.id).map(|t| t.trim()).unwrap_or_default();
        if text.is_empty() {
            return Err(format!("fill in \"{}\" first", field.label));
        }
        if text.len() > irori_types::MAX_ACTION_INPUT {
            return Err(format!(
                "\"{}\" is too long: at most {} bytes",
                field.label,
                irori_types::MAX_ACTION_INPUT
            ));
        }
        filled.fields.insert(field.id.clone(), text.to_owned());
    }
    Ok((filled, settings))
}

/// What can come with an action that has ways: which way, what was typed into its fields, and
/// any of the settings it lists. No body at all is a plain press.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionBody {
    #[serde(default)]
    way: Option<String>,
    #[serde(default)]
    fields: BTreeMap<String, String>,
    #[serde(default)]
    settings: serde_json::Map<String, serde_json::Value>,
}

/// Ends early an action an extension says is open: closes Zigbee's network before its minute
/// is up. One that isn't open has nothing to end, and that's fine.
async fn stop_extension_action(
    State(state): State<AppState>,
    Path((id, action_id)): Path<(ExtensionId, String)>,
) -> Response {
    let core = &state.0.core;
    let Some(overview) = core.extensions().remove(&id) else {
        return refused(
            StatusCode::NOT_FOUND,
            format!("there's no extension `{id}`"),
        );
    };
    if !overview.open_actions.contains_key(&action_id) {
        return StatusCode::NO_CONTENT.into_response();
    }
    match core.stop_action(&id, &action_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(why) => refused(StatusCode::BAD_REQUEST, why.to_string()),
    }
}

/// Of the keys `secrets.toml` has for an extension, those its schema declares as secrets. A key
/// the schema doesn't know, or calls an ordinary setting, isn't named.
fn saved_secrets(saved: &[String], schema: &serde_json::Value) -> Vec<String> {
    saved
        .iter()
        .filter(|key| {
            schema
                .get("properties")
                .and_then(|properties| properties.get(key.as_str()))
                .is_some_and(|field| is_write_only(field, schema))
        })
        .cloned()
        .collect()
}

/// Whether a JSON Schema node is (or, through `$ref`/`anyOf`/`oneOf`/`allOf`, resolves to
/// including) a `writeOnly` field — schemars' shape for `Option<Secret>` is
/// `{"anyOf": [{"$ref": "#/$defs/Secret"}, {"type": "null"}]}`, so a direct check on `field_schema`
/// alone isn't enough.
pub(crate) fn is_write_only(field_schema: &serde_json::Value, root: &serde_json::Value) -> bool {
    if field_schema
        .get("writeOnly")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        return true;
    }
    if let Some(reference) = field_schema.get("$ref").and_then(serde_json::Value::as_str) {
        return reference
            .strip_prefix("#/$defs/")
            .and_then(|name| root.get("$defs")?.get(name))
            .is_some_and(|resolved| is_write_only(resolved, root));
    }
    ["anyOf", "oneOf", "allOf"].iter().any(|combinator| {
        field_schema
            .get(combinator)
            .and_then(serde_json::Value::as_array)
            .is_some_and(|branches| branches.iter().any(|branch| is_write_only(branch, root)))
    })
}

/// A secret for an extension, at the place it asked for one: `{"path": [...], "value": "..."}`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretGiven {
    path: Vec<String>,
    value: String,
}

/// Written by hand so the value can't reach a log through `{:?}`.
impl std::fmt::Debug for SecretGiven {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretGiven")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Hands an extension a secret it asked for — an encryption key for a device it found, say — by
/// writing it into `secrets.toml`. The extension is restarted with it.
///
/// **Only where it asked.** The path has to be one the extension lists as waiting right now
/// (`docs/specs/protocols.md` §6.6). There is no sign-in yet (D12), so this endpoint must not
/// be a way to put anything into anyone's settings; this way it can only answer a question an
/// extension is actually asking. The value is never echoed back, logged, or readable afterwards.
async fn give_secret(
    State(state): State<AppState>,
    Path(id): Path<ExtensionId>,
    Json(given): Json<SecretGiven>,
) -> Response {
    let core = &state.0.core;
    let Some(extension) = core.extensions().remove(&id) else {
        return refused(
            StatusCode::NOT_FOUND,
            format!("there's no extension `{id}`"),
        );
    };
    let asked = extension
        .waiting
        .iter()
        .filter_map(|waiting| waiting.secret.as_ref())
        .any(|secret| secret.path == given.path);
    if !asked {
        return refused(
            StatusCode::CONFLICT,
            format!(
                "`{id}` isn't asking for a secret there right now; it may already have one, or \
                 have stopped waiting"
            ),
        );
    }
    if given.value.trim().is_empty() {
        return refused(StatusCode::BAD_REQUEST, "the secret is empty".to_owned());
    }
    let saved = state
        .0
        .config
        .edit_secrets(core, |secrets| {
            secrets
                .set(&id, &given.path, given.value.trim().to_owned())
                .map_err(|e| Refused(e.to_string()))
        })
        .await;
    match saved {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => edit_failed(e),
    }
}

/// Official catalog plus whether each one is installed in this instance.
async fn catalog(State(state): State<AppState>) -> Json<Vec<CatalogEntry>> {
    let running = state.0.core.extensions();
    let work = state.0.host.work();
    // What each installed extension is configured with right now, so its settings form opens
    // showing it. Read before the loop because reading takes the config lock, and the loop is a
    // plain `map`; only for installed extensions, since nothing else has a file.
    let mut configured: std::collections::BTreeMap<
        ExtensionId,
        serde_json::Map<String, serde_json::Value>,
    > = std::collections::BTreeMap::new();
    for id in running.keys() {
        configured.insert(id.clone(), state.0.config.extension_settings(id).await);
    }
    let secrets = state.0.config.secrets_set().await;
    Json(
        crate::packages::official()
            .into_iter()
            .map(|item| {
                let overview = running.get(&item.id);
                // Live, not the catalog's own claim: an installed extension's manifest is only
                // read once its supervised task actually describes it, which can be a moment
                // after `installed` turns true, and a manifest missing `run` or a protocol
                // contribution never gets described at all (host.rs). Either way, this says
                // whether `extension_icon` actually has bytes right now.
                let icon = state.0.core.has_extension_icon(&item.id);
                let busy = work.get(&item.id).map(|work| work.as_str());
                // Secrets never come back out (§3.4). They live in `secrets.toml` and so aren't
                // in this file at all, but a hand-written one could still name a `writeOnly`
                // field, and it would be this response that leaked it.
                let settings = configured.get(&item.id).map(|file| {
                    let schema = overview
                        .and_then(|o| o.info.as_ref())
                        .and_then(|info| info.config_schema.as_ref());
                    file.iter()
                        .filter(|(key, _)| match schema {
                            // A key the schema doesn't mention is not one this can vouch for, so
                            // it stays here rather than going out.
                            Some(schema) => schema
                                .get("properties")
                                .and_then(|properties| properties.get(key.as_str()))
                                .is_some_and(|field| !is_write_only(field, schema)),
                            None => false,
                        })
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                });
                let secrets_set = overview
                    .and_then(|o| o.info.as_ref())
                    .and_then(|info| info.config_schema.as_ref())
                    .map(|schema| {
                        saved_secrets(secrets.get(&item.id).map_or(&[][..], Vec::as_slice), schema)
                    })
                    .unwrap_or_default();
                CatalogEntry {
                    id: item.id,
                    name: item.name.to_string(),
                    category: item.category,
                    description: item.description,
                    version: item.version,
                    official: true,
                    full_access: item.full_access,
                    network: item.network,
                    installed: overview.is_some(),
                    busy,
                    installed_version: overview
                        .and_then(|o| o.info.as_ref())
                        .map(|info| info.version.to_string()),
                    icon,
                    state: overview.map(|o| match &o.status {
                        irori_core::ExtensionStatus::Disabled => "disabled",
                        irori_core::ExtensionStatus::Starting => "starting",
                        irori_core::ExtensionStatus::Running => "running",
                        irori_core::ExtensionStatus::Degraded { .. } => "degraded",
                        irori_core::ExtensionStatus::Failed { .. } => "failed",
                        irori_core::ExtensionStatus::NeedsSetup { .. } => "needs_setup",
                        irori_core::ExtensionStatus::Waiting { .. } => "waiting",
                    }),
                    reason: overview.and_then(|o| match &o.status {
                        irori_core::ExtensionStatus::Degraded { reason }
                        | irori_core::ExtensionStatus::Failed { reason, .. }
                        | irori_core::ExtensionStatus::Waiting { reason } => Some(reason.clone()),
                        irori_core::ExtensionStatus::NeedsSetup { missing } => {
                            Some(needs_setup_reason(missing))
                        }
                        _ => None,
                    }),
                    config_schema: overview
                        .and_then(|o| o.info.as_ref())
                        .and_then(|info| info.config_schema.clone()),
                    settings,
                    secrets_set,
                }
            })
            .collect(),
    )
}

/// What a person needs to do about `needs_setup`, in the same `reason` slot every other state
/// puts its own explanation. The field names come from the extension's own `config_schema`, which
/// is also what the settings form labels them by — `serial_port` there reads as "Serial port".
fn needs_setup_reason(missing: &[String]) -> String {
    let named = missing
        .iter()
        .map(|key| format!("`{key}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "waiting on a setting it can't start without: {named}. Open its settings to fill it in."
    )
}

#[derive(Debug, Serialize)]
struct CatalogEntry {
    id: ExtensionId,
    name: String,
    category: String,
    description: String,
    version: String,
    official: bool,
    /// `host_shell`, or a `host_fs` path outside Irori's own folders. The page says "full
    /// access to this machine" and install is refused until that is approved.
    full_access: bool,
    /// The internet hosts it connects to. Empty for one that stays on the local network.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    network: Vec<String>,
    installed: bool,
    /// `installing`, `updating` or `uninstalling` while that is underway, whoever asked for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    busy: Option<&'static str>,
    /// The version that's running, which `version` (the one Install would fetch) can be ahead
    /// of: nothing updates an installed extension when Irori itself is updated.
    #[serde(skip_serializing_if = "Option::is_none")]
    installed_version: Option<String>,
    icon: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config_schema: Option<serde_json::Value>,
    /// What it's configured with now, secrets excluded — so its form can open showing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    settings: Option<serde_json::Map<String, serde_json::Value>>,
    /// The secret settings that have a value saved, by name only: the form says a password is
    /// saved, and folds a section of them away, without the value ever leaving `secrets.toml`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    secrets_set: Vec<String>,
}

/// What the Extensions page sends when the person has read the full-access warning and chosen
/// Install anyway. Absent means they have not: a plain POST must not skip the warning.
#[derive(Debug, Deserialize)]
struct InstallApproval {
    #[serde(default)]
    approve_full_access: bool,
    /// Replace the version that's installed, keeping its devices, settings and data.
    #[serde(default)]
    update: bool,
}

async fn install_official(
    State(state): State<AppState>,
    Path(id): Path<ExtensionId>,
    approval: Option<Json<InstallApproval>>,
) -> Response {
    let Some(item) = crate::packages::official_by_id(&id) else {
        return refused(
            StatusCode::NOT_FOUND,
            format!("`{id}` isn't an official extension"),
        );
    };
    // There are no accounts yet (the server's own note at the top of this file), so this cannot
    // check that the approver is the owner. It can refuse a request that never acknowledged the
    // warning. The page collects that acknowledgement; this is what stops anything else.
    let update = approval
        .as_ref()
        .is_some_and(|Json(approval)| approval.update);
    let approved = approval.is_some_and(|Json(approval)| approval.approve_full_access);
    if item.full_access && !approved {
        return refused(StatusCode::CONFLICT, full_access_refusal(&id));
    }
    match place_official(state.0.host.clone(), item, approved, update).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(why) => why.into_response(),
    }
}

/// Why an official extension wasn't installed or updated.
#[derive(Debug)]
pub(crate) enum NotPlaced {
    /// Something else is being done to its package already, and is seen to by whoever asked
    /// first: another tab, Irori's own update entity, the reinstall after a restore.
    Underway(irori_core::Underway),
    /// A plain install of one that's installed already.
    Installed(String),
    /// It couldn't be: the fetch, the build or the package itself failed.
    Failed(StatusCode, String),
}

impl NotPlaced {
    /// Said in a sentence, whichever it was.
    pub(crate) fn why(&self) -> String {
        match self {
            Self::Underway(underway) => underway.to_string(),
            Self::Installed(why) | Self::Failed(_, why) => why.clone(),
        }
    }

    fn into_response(self) -> Response {
        match self {
            Self::Underway(underway) => refused_underway(&underway),
            Self::Installed(why) => refused(StatusCode::CONFLICT, why),
            Self::Failed(status, why) => refused(status, why),
        }
    }
}

/// A 409 for work on a package that's underway already. Beside the sentence, `underway` says
/// what it is (`installing`, `updating`, `uninstalling`), so a page can tell this apart from a
/// refusal that's a reason to stop and follow the work instead of showing an error.
fn refused_underway(underway: &irori_core::Underway) -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "error": underway.to_string(),
            "underway": underway.work.as_str(),
        })),
    )
        .into_response()
}

/// Fetches an official extension and installs it, or replaces the installed one with it
/// (`update`). Refused while its package is being worked on already: a second tab, a reload or
/// Irori's own update entity is told it's underway rather than fetching a second copy.
///
/// The work runs in a task of its own, holding the package's claim to the end. The request that
/// asked can be gone (a page closed or reloaded) and it still finishes, or cleans up after
/// itself, instead of stopping wherever the request was dropped.
pub(crate) async fn place_official(
    host: irori_core::ExtensionHost,
    item: crate::packages::Official,
    approved: bool,
    update: bool,
) -> Result<(), NotPlaced> {
    let work = if update {
        irori_core::PackageWork::Updating
    } else {
        irori_core::PackageWork::Installing
    };
    let claim = host.claim(&item.id, work).map_err(NotPlaced::Underway)?;
    // Said before fetching anything, not after a download or a build of minutes.
    if !update && host.packages_dir().join(item.id.as_str()).exists() {
        return Err(NotPlaced::Installed(format!(
            "`{}` is already installed",
            item.id
        )));
    }
    tokio::spawn(async move {
        let _claim = claim;
        let id = item.id.clone();
        // Staged out of any live package's way; `install_package` or `update_package` decides
        // where it lands. A stage left by a stop halfway is cleared at the next start.
        let stage = host
            .new_stage()
            .map_err(|why| NotPlaced::Failed(StatusCode::INTERNAL_SERVER_ERROR, why))?;
        let placed = async {
            tokio::task::spawn_blocking({
                let stage = stage.clone();
                move || crate::packages::install_official(&item, &stage)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
            .map_err(|why| (StatusCode::BAD_GATEWAY, why))?;
            // The catalog said what this extension is supposed to be. The files just staged
            // are what would actually run, and a release asset or a package beside the binary
            // can disagree with the catalog. The manifest's own id and its permissions decide.
            unapproved_full_access(&stage, Some(&id), approved)?;
            // An update replaces what's installed and starts it again; a plain install still
            // refuses one that's already there.
            if update {
                host.update_package(stage.clone()).await
            } else {
                host.install_package(stage.clone())
            }
            .map(|_| ())
            .map_err(|why| (StatusCode::CONFLICT, why))
        }
        .await;
        if placed.is_err() {
            let _ = std::fs::remove_dir_all(&stage);
        }
        placed.map_err(|(status, why)| NotPlaced::Failed(status, why))
    })
    .await
    .unwrap_or_else(|e| {
        Err(NotPlaced::Failed(
            StatusCode::INTERNAL_SERVER_ERROR,
            e.to_string(),
        ))
    })
}

#[derive(Debug, Deserialize)]
struct InstallUrl {
    url: String,
    /// Same acknowledgement as an official install. A tarball's manifest isn't known until it
    /// has been downloaded, so this is checked after that, against the manifest itself.
    #[serde(default)]
    approve_full_access: bool,
}

fn full_access_refusal(id: &ExtensionId) -> String {
    format!("`{id}` has full access to this machine. Install it only by approving that.")
}

/// Reads the staged package's manifest and refuses full access unless `approved`. Answers with
/// the id the manifest gives itself.
///
/// A package that can't be read is refused too: installing it would start whatever the manifest
/// says, and a manifest we couldn't check might be the one that needed the approval.
pub(crate) fn unapproved_full_access(
    dir: &std::path::Path,
    expected: Option<&ExtensionId>,
    approved: bool,
) -> Result<ExtensionId, (StatusCode, String)> {
    let path = dir.join("irori-extension.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{}: {e}", path.display())))?;
    let manifest =
        irori_protocol::parse_manifest(&text).map_err(|why| (StatusCode::BAD_REQUEST, why))?;
    if let Some(expected) = expected
        && &manifest.extension.id != expected
    {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "the package's manifest says `{}`, not `{expected}`",
                manifest.extension.id
            ),
        ));
    }
    if manifest.permissions.full_access() && !approved {
        Err((
            StatusCode::CONFLICT,
            full_access_refusal(&manifest.extension.id),
        ))
    } else {
        Ok(manifest.extension.id)
    }
}

async fn install_url(State(state): State<AppState>, Json(body): Json<InstallUrl>) -> Response {
    let url = body.url.trim();
    if url.is_empty() {
        return refused(StatusCode::BAD_REQUEST, "url is empty".into());
    }
    // `curl` understands every scheme it was built with, so one gate here keeps the endpoint
    // from acting as a proxy for file:/ftp:/gopher: or anything else a hostile URL could reach.
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return refused(
            StatusCode::BAD_REQUEST,
            "only http:// and https:// URLs are allowed".into(),
        );
    }
    let host = state.0.host.clone();
    let url = url.to_owned();
    // In a task of its own, so a page that closes mid-download doesn't leave the download
    // behind. Its id isn't known until the package is in, so it's claimed only from then on.
    tokio::spawn(async move {
        let dest = match host.new_stage() {
            Ok(dest) => dest,
            Err(why) => return refused(StatusCode::INTERNAL_SERVER_ERROR, why),
        };
        let placed = async {
            tokio::task::spawn_blocking({
                let dest = dest.clone();
                move || crate::packages::install_url(&url, &dest)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
            .map_err(|why| refused(StatusCode::BAD_GATEWAY, why))?;
            let id = unapproved_full_access(&dest, None, body.approve_full_access)
                .map_err(|(status, why)| refused(status, why))?;
            let _claim = host
                .claim(&id, irori_core::PackageWork::Installing)
                .map_err(|underway| refused_underway(&underway))?;
            host.install_package(dest.clone())
                .map_err(|why| refused(StatusCode::BAD_REQUEST, why))
        }
        .await;
        match placed {
            Ok(_) => StatusCode::NO_CONTENT.into_response(),
            Err(refusal) => {
                let _ = std::fs::remove_dir_all(&dest);
                refusal
            }
        }
    })
    .await
    .unwrap_or_else(|e| refused(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// Uninstalls an extension, and removes every device it brought in with it: installing it again
/// starts from nothing in the home, with everything it finds listed under "+ Add device".
async fn uninstall(State(state): State<AppState>, Path(id): Path<ExtensionId>) -> Response {
    let claim = match state
        .0
        .host
        .claim(&id, irori_core::PackageWork::Uninstalling)
    {
        Ok(claim) => claim,
        Err(underway) => return refused_underway(&underway),
    };
    // To the end in a task of its own, so a page that closes doesn't leave the extension
    // stopped but its devices still in the home.
    tokio::spawn(async move {
        let _claim = claim;
        if let Err(why) = state.0.host.uninstall(&id).await {
            return refused(StatusCode::BAD_REQUEST, why);
        }
        let Ok(protocol) = irori_types::ProtocolId::try_from(id.as_str()) else {
            return StatusCode::NO_CONTENT.into_response();
        };
        match state
            .0
            .config
            .forget_protocol(&state.0.core, &protocol)
            .await
        {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(e) => edit_failed(e),
        }
    })
    .await
    .unwrap_or_else(|e| refused(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// An edit that couldn't be made says why; one that couldn't be written says that instead, because
/// the two need different things from whoever is reading.
fn edit_failed(error: EditError) -> Response {
    match error {
        EditError::Refused(why) => refused(StatusCode::BAD_REQUEST, why.to_string()),
        EditError::Io(e) => {
            tracing::error!(%e, "couldn't write the config directory");
            refused(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("couldn't write the config directory: {e}"),
            )
        }
    }
}

/// A command from the UI: `{"entity_id": "light.hallway", "command": "toggle"}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandRequest {
    entity_id: EntityId,
    /// One of the entity's kind's actions (`turn_on`), or `toggle`.
    command: String,
    /// The action's data, e.g. brightness or color for a light's `turn_on`.
    #[serde(default)]
    data: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Asks an entity to do something and answers with its state once the protocol confirms, so
/// the page can show the result without waiting for its next refresh.
///
/// A command is attributed to whoever is signed in, or to the home's owner while Irori is open;
/// before anybody has been set up, to one unauthenticated person.
async fn command(
    State(state): State<AppState>,
    Extension(who): Extension<auth::Actor>,
    Extension(reach): Extension<Reach>,
    Extension(trail): Extension<trail::Trail>,
    Json(request): Json<CommandRequest>,
) -> Response {
    // Which command, never its data: a lock's code is data.
    trail.target(request.entity_id.as_str());
    trail.detail(request.command.as_str());
    // A thing outside a person's rooms is answered as one that was never there. That comes
    // before anything else is said about it, or "only an owner may" would give it away. The
    // activity log, which only an owner reads, says what it was.
    if !reach.sees_entity(&request.entity_id) {
        trail.outcome(Outcome::Denied);
        trail.detail(format!("{}: not in their rooms", request.command));
        let gone = CallError::UnknownEntity(request.entity_id.clone());
        return refused(status_for(&gone), gone.to_string());
    }
    // An update changes what the home runs on, which is setup. A token is never an owner, so
    // it doesn't install one either (`docs/specs/api.md` §2).
    if request.entity_id.kind().commands_need_owner() && !who.owner {
        return refused(
            StatusCode::FORBIDDEN,
            format!(
                "only an owner can send a command to `{}`: it changes what the home runs on",
                request.entity_id
            ),
        );
    }
    let command = Command {
        action: request.command,
        data: request.data.unwrap_or_default(),
    };
    let core = &state.0.core;
    let origin = match who.token.as_ref() {
        Some(token) => Origin::Api {
            token_id: token.id.clone(),
        },
        None => Origin::User {
            user_id: who.user_id(),
        },
    };
    let who = core.new_context(origin);
    trail.context(who.id.clone());
    // Subscribed before the call: the protocol reports the new state and answers the call in
    // the same breath, and the report must not slip past while the call is still in flight.
    let changes = core.subscribe();
    match core
        .call_service(&request.entity_id, command, who.clone())
        .await
    {
        Ok(()) => {
            // Something with no value (a button) reports nothing to wait for.
            let seen = core.seen(&reach);
            let has_value = seen
                .entity(&request.entity_id)
                .is_some_and(|entity| entity.capabilities.primary_shape().is_some());
            if !has_value {
                return Json(seen.state(&request.entity_id)).into_response();
            }
            let settled = settled(changes, &request.entity_id, &who.id).await;
            Json(settled.or_else(|| seen.state(&request.entity_id))).into_response()
        }
        Err(e) => refused(status_for(&e), e.to_string()),
    }
}

/// How long to wait for the change a command caused before answering with what's on file. The
/// protocol has already confirmed by this point, so the report is usually a moment away.
const SETTLE: Duration = Duration::from_millis(500);

/// Waits for the state change this command caused. Answers `None` when there wasn't one: a light
/// asked to turn on while it's already on has nothing new to report, and says so by staying quiet.
async fn settled(
    mut changes: broadcast::Receiver<Event>,
    entity_id: &EntityId,
    context_id: &ContextId,
) -> Option<EntityState> {
    let wait = async {
        loop {
            match changes.recv().await {
                Ok(Event::StateChanged {
                    entity_id: changed,
                    new_state,
                    ..
                }) if &changed == entity_id
                    && new_state.context.parent_id.as_ref() == Some(context_id) =>
                {
                    return Some(*new_state);
                }
                // A listener that falls behind skips ahead; it may have skipped past the change.
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    };
    tokio::time::timeout(SETTLE, wait).await.ok().flatten()
}

/// Which HTTP status a refused command gets: the caller's fault, the device's, or time running
/// out.
fn status_for(error: &CallError) -> StatusCode {
    match error {
        CallError::UnknownEntity(_) => StatusCode::NOT_FOUND,
        CallError::NotSupported(_) => StatusCode::BAD_REQUEST,
        CallError::NotRunning(_) | CallError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        CallError::Failed(_) => StatusCode::BAD_GATEWAY,
        CallError::Timeout => StatusCode::GATEWAY_TIMEOUT,
    }
}

fn refused(status: StatusCode, error: String) -> Response {
    #[derive(Debug, Serialize)]
    struct Refused {
        error: String,
    }
    (status, Json(Refused { error })).into_response()
}

#[derive(Debug, Serialize)]
struct Health<'a> {
    status: &'static str,
    version: &'static str,
    /// What this binary was built from, so "am I running the version I just built?" has an
    /// answer even between releases, when every development build is `0.0.0`.
    commit: &'static str,
    built_at: &'static str,
    /// Which boot this is — a different value after every process start. Uptime can't tell an
    /// instance that started a minute ago from one that restarted a minute ago; this can, which
    /// is what lets the Settings page know the restart it asked for is done.
    boot_id: &'a str,
    uptime_ms: u128,
    features: &'a [&'static str],
    sqlite: SqliteHealth<'a>,
}

#[derive(Debug, Serialize)]
struct SqliteHealth<'a> {
    version: &'static str,
    journal_mode: &'a str,
    /// A trivial read succeeded, and the recorder's last write did too. The process can be up
    /// when this is false: the file is the part that is unhappy.
    ok: bool,
}

/// Liveness: the process is up and serving. `sqlite.ok` is a live check of the database the
/// recorder keeps open (a read, and whether the last write failed).
async fn health(State(state): State<AppState>) -> Response {
    let inner = &state.0;
    Json(Health {
        status: "ok",
        version: VERSION,
        commit: crate::build_info::COMMIT,
        built_at: crate::build_info::BUILT_AT,
        boot_id: &inner.boot_id,
        uptime_ms: inner.started.elapsed().as_millis(),
        features: &inner.build.features,
        sqlite: SqliteHealth {
            version: inner.build.sqlite_version,
            journal_mode: &inner.db.journal_mode,
            ok: inner.history.check(),
        },
    })
    .into_response()
}

#[cfg(feature = "ui")]
mod ui {
    use axum::http::{StatusCode, Uri, header};
    use axum::response::{IntoResponse, Response};
    use rust_embed::RustEmbed;

    /// The Leptos app, put here by `cargo xtask ui`. Empty after a plain `cargo build`, which
    /// needs no wasm toolchain; the placeholder page below is then served instead.
    #[derive(RustEmbed)]
    #[folder = "ui/"]
    struct App;

    /// The placeholder page and the favicon: no build step, always there.
    #[derive(RustEmbed)]
    #[folder = "assets/"]
    struct Placeholder;

    const INDEX: &str = "index.html";

    pub async fn serve(uri: Uri) -> Response {
        let path = uri.path().trim_start_matches('/');
        let path = if path.is_empty() { INDEX } else { path };
        // `/api/…` belongs to the API, whatever is or isn't there. Handing an API caller the
        // page with a 200 would let a typo look like success.
        if path == "api" || path.starts_with("api/") {
            return (StatusCode::NOT_FOUND, "no such endpoint\n").into_response();
        }
        // The app wins where both have a file, so a built UI replaces the placeholder page.
        let file = App::get(path)
            .or_else(|| Placeholder::get(path))
            .map(|file| (path, file))
            // A path with no file extension is one of the app's own pages (`/devices`), which
            // the app routes itself once it has loaded: it gets the page, not a 404. A missing
            // file (`/nope.css`) is still a 404, so a broken asset says so plainly.
            .or_else(|| {
                (!path.contains('.'))
                    .then(|| {
                        App::get(INDEX)
                            .or_else(|| Placeholder::get(INDEX))
                            .map(|index| (INDEX, index))
                    })
                    .flatten()
            });
        match file {
            Some((path, file)) => {
                let mime = mime_guess::from_path(path).first_or_octet_stream();
                ([(header::CONTENT_TYPE, mime.as_ref())], file.data).into_response()
            }
            None => (StatusCode::NOT_FOUND, "not found").into_response(),
        }
    }
}

#[cfg(not(feature = "ui"))]
mod ui {
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};

    pub async fn serve() -> Response {
        (
            StatusCode::NOT_FOUND,
            "The web UI is not compiled into this build (built without the `ui` feature).\n",
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    use super::*;

    fn core() -> Core {
        Core::new(Arc::new(irori_core::SystemClock))
    }

    /// A server on throwaway directories. Held for the length of a test, because the config
    /// directory has to outlive the requests that write to it.
    struct Server {
        dir: tempfile::TempDir,
        core: Core,
        config: Config,
        history: History,
        /// Who did what. One for the whole test, as the real one is for the whole process: each
        /// request builds its own router, and the rows from all of them land in one place.
        audit: crate::audit::Audit,
        // The log this instance is serving, so a test can put lines in it. The real one is filled
        // by the log subscriber's writer (`syslog::Tee`), which a test has no reason to stand up.
        log: Arc<syslog::Log>,
        // The restart handle, held back so a test can check that POST /api/restart woke the
        // shutdown and told it to restart, not just that it answered 202.
        restart: Arc<tokio::sync::Notify>,
        restarting: Arc<AtomicBool>,
    }

    impl Server {
        fn new(core: Core) -> anyhow::Result<Self> {
            let dir = tempfile::tempdir()?;
            // A device joins the home only once a person adds it, so without this every test
            // below would have to add the demo's devices before it could look at one. Whatever
            // the core has already been told about is written down as added, as if someone had
            // pressed Add on each; the tests that are *about* adding start from a core with
            // nothing in it.
            let config_dir = dir.path().join("config");
            std::fs::create_dir_all(&config_dir)?;
            let added: String = core
                .devices()
                .into_iter()
                .map(|device| device.id)
                .chain(core.held_devices().into_iter().map(|device| device.id))
                .map(|id| format!("[devices.{id}]\nadded = true\n\n"))
                .collect();
            std::fs::write(config_dir.join("devices.toml"), added)?;
            let config = Config::open_dir(config_dir, &core);
            let history = History::open(
                &dir.path().join("irori.db"),
                irori_recorder::Retention::default(),
            )?;
            let audit = crate::audit::Audit::open(
                &dir.path().join("irori.db"),
                crate::audit::Keep::default(),
            )?;
            Ok(Self {
                dir,
                core,
                config,
                history,
                audit,
                log: Arc::new(syslog::Log::default()),
                restart: Arc::new(tokio::sync::Notify::new()),
                restarting: Arc::new(AtomicBool::new(false)),
            })
        }

        fn app(&self) -> anyhow::Result<Router> {
            let host = irori_core::ExtensionHost::start(
                &self.core,
                Vec::new(),
                irori_core::Timing::default(),
            )
            .map_err(anyhow::Error::msg)?;
            self.app_with(host)
        }

        /// The app over a host the test holds on to.
        fn app_with(&self, host: irori_core::ExtensionHost) -> anyhow::Result<Router> {
            Ok(router(AppState::new(
                crate::db::open(self.dir.path())?,
                self.core.clone(),
                self.config.clone(),
                host,
                self.history.clone(),
                self.audit.clone(),
                Arc::clone(&self.log),
                self.restart.clone(),
                self.restarting.clone(),
            )))
        }

        /// The config directory this server writes to, for checking what landed on disk.
        fn config_dir(&self) -> std::path::PathBuf {
            self.dir.path().join("config")
        }

        async fn send(&self, request: Request<Body>) -> anyhow::Result<(StatusCode, Vec<u8>)> {
            let res = self.app()?.oneshot(request).await?;
            let status = res.status();
            let body = res.into_body().collect().await?.to_bytes().to_vec();
            Ok((status, body))
        }

        async fn json(
            &self,
            method: &str,
            path: &str,
            body: serde_json::Value,
        ) -> anyhow::Result<(StatusCode, serde_json::Value)> {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body)?))?;
            let (status, bytes) = self.send(request).await?;
            Ok((
                status,
                serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
            ))
        }

        async fn read(&self, path: &str) -> anyhow::Result<serde_json::Value> {
            let (_, bytes) = self.send(Request::get(path).body(Body::empty())?).await?;
            Ok(serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
        }
    }

    async fn get(path: &str) -> anyhow::Result<(StatusCode, Option<String>, Vec<u8>)> {
        get_from(core(), path).await
    }

    async fn get_from(
        core: Core,
        path: &str,
    ) -> anyhow::Result<(StatusCode, Option<String>, Vec<u8>)> {
        let server = Server::new(core)?;
        let res = server
            .app()?
            .oneshot(Request::get(path).body(Body::empty())?)
            .await?;
        let status = res.status();
        let content_type = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = res.into_body().collect().await?.to_bytes().to_vec();
        Ok((status, content_type, body))
    }

    #[tokio::test]
    async fn health_reports_ok_and_wal() -> anyhow::Result<()> {
        let (status, _, body) = get("/api/health").await?;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(json["status"], "ok");
        assert_eq!(json["sqlite"]["journal_mode"], "wal");
        assert_eq!(json["sqlite"]["ok"], true);
        Ok(())
    }

    /// The System menu's endpoint answers with the machine the test is running on, and helps
    /// rather than guesses: the numbers read real, and the rows that the OS refused to say are
    /// left out rather than made up.
    #[tokio::test]
    async fn system_describes_the_machine_the_instance_runs_on() -> anyhow::Result<()> {
        let (status, _, body) = get("/api/system").await?;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body)?;
        assert!(
            json["os"].as_str().is_some_and(|os| !os.is_empty()),
            "the OS should say what it is: {json}"
        );
        assert!(
            json["arch"].as_str().is_some_and(|arch| !arch.is_empty()),
            "the architecture should say what build this is: {json}"
        );
        assert!(json["cpu_cores"].as_u64().unwrap_or(0) > 0, "{json}");
        assert!(json["memory_total"].as_u64().unwrap_or(0) > 0, "{json}");
        assert!(
            json["disk"]["total"].as_u64().unwrap_or(0) > 0,
            "the volume with the data should report its size: {json}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn usage_says_what_is_running_and_what_is_kept() -> anyhow::Result<()> {
        let (status, _, body) = get("/api/system/usage").await?;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body)?;
        assert!(
            json["processes"]
                .as_array()
                .is_some_and(|all| !all.is_empty()),
            "something is always running: {json}"
        );
        assert!(json["storage"].is_array(), "{json}");
        Ok(())
    }

    /// The endpoint's own shape: what devices are actually found is host-specific and covered in
    /// `crate::serial`'s own tests.
    #[tokio::test]
    async fn serial_ports_answers_with_a_list() -> anyhow::Result<()> {
        let (status, _, body) = get("/api/serial-ports").await?;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body)?;
        assert!(json.is_array(), "{json}");
        Ok(())
    }

    /// A restart request is accepted first, because the restart happens after the answer: the
    /// page's POST has to come back before the server — and with it the connection — goes. It
    /// also does what it says: the shutdown is woken and told it's a restart, which the runner
    /// (serve) reads to re-exec the binary instead of exiting.
    #[tokio::test]
    async fn restart_is_accepted() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        // The waiter serve()'s own shutdown future would be. It is registered before the
        // request goes out so the handler's notify_one has a waiter to wake; a retained
        // notification (no waiter registered) is covered by the handler's semantics instead.
        let mut shutdown = Box::pin(server.restart.notified());
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        assert!(shutdown.as_mut().poll(&mut cx).is_pending());
        let (status, _) = server
            .send(
                Request::post("/api/restart")
                    .header("x-irori-ui", "1")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(
            server.restarting.load(Ordering::SeqCst),
            "the restart was told it's a restart"
        );
        assert!(
            shutdown.as_mut().poll(&mut cx).is_ready(),
            "the shutdown was woken"
        );
        Ok(())
    }

    /// A backup is made, listed, downloaded, checked and deleted over the API; only a backup's
    /// own name is served; and the first-run restore routes close once there is an owner.
    #[tokio::test]
    async fn backups_are_made_listed_downloaded_and_checked() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let post = |path: &str, body: &str| {
            Request::post(path)
                .header("x-irori-ui", "1")
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))
        };
        let (status, body) = server.send(post("/api/backups", "")?).await?;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "{}",
            String::from_utf8_lossy(&body)
        );
        let name = serde_json::from_slice::<serde_json::Value>(&body)?["name"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(name.starts_with("irori-backup-"), "{name}");
        // Sealed by default: the key was made for it.
        assert!(name.ends_with(".tar.gz.enc"), "{name}");
        let view = server.read("/api/backups").await?;
        assert_eq!(view["backups"][0]["name"], name.as_str(), "{view}");
        assert_eq!(view["backups"][0]["encrypted"], true);
        assert_eq!(view["key_set"], true);
        let key = server.read("/api/backups/key").await?["key"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let (status, _) = server
            .send(Request::get(format!("/api/backups/{name}")).body(Body::empty())?)
            .await?;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = server
            .send(Request::get("/api/backups/secrets.toml").body(Body::empty())?)
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = server
            .send(post(
                &format!("/api/backups/{name}/verify"),
                r#"{"key":"wrong"}"#,
            )?)
            .await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, body) = server
            .send(post(
                &format!("/api/backups/{name}/verify"),
                &format!(r#"{{"key":"{key}"}}"#),
            )?)
            .await?;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let (status, _) = server
            .send(
                Request::delete(format!("/api/backups/{name}"))
                    .header("x-irori-ui", "1")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        Ok(())
    }

    /// A restart without the page's header is refused: restart is a service interruption, so a
    /// bare POST — a cross-site form, say — must not be able to take the instance down. The
    /// header is what only same-origin JavaScript can set (a form carries none, and a fetch
    /// with a custom header is stopped by CORS preflight). The body says what the caller would
    /// need to know to ask again, per the API error contract.
    #[tokio::test]
    async fn restart_without_the_page_header_is_refused() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (status, body) = server
            .send(Request::post("/api/restart").body(Body::empty())?)
            .await?;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let body = String::from_utf8(body)?;
        assert!(
            body.contains("x-irori-ui: 1"),
            "the refusal should name the header and value it needs: {body}"
        );
        assert!(
            !server.restarting.load(Ordering::SeqCst),
            "a refused restart must not mark the boot as restarting"
        );
        Ok(())
    }

    /// A second restart inside the cooldown is refused: anyone who can reach this route at all
    /// can spoof the page header (there is no auth yet), so the endpoint must not let a caller
    /// stack outages. The page never hits this — its button is disabled while a restart is in
    /// flight — so the cooldown only costs an abuser.
    #[tokio::test]
    async fn restart_is_refused_again_within_the_cooldown() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        // One app, so the two requests share the cooldown clock (each fresh `send` would build
        // its own).
        let app = server.app()?;
        let ask = || {
            Request::post("/api/restart")
                .header("x-irori-ui", "1")
                .body(Body::empty())
        };
        let first = app.clone().oneshot(ask()?).await?;
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        let second = app.clone().oneshot(ask()?).await?;
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
        Ok(())
    }

    /// A new boot is a new instance: the health of two servers — two process starts, in the
    /// restart's case — must not carry the same boot id, or the page couldn't tell the restart
    /// it asked for from the process that was already there.
    #[tokio::test]
    async fn every_boot_gets_its_own_id() -> anyhow::Result<()> {
        async fn boot_id(server: &Server) -> anyhow::Result<String> {
            let (_, body) = server
                .send(Request::get("/api/health").body(Body::empty())?)
                .await?;
            let json: serde_json::Value = serde_json::from_slice(&body)?;
            Ok(json["boot_id"].as_str().unwrap_or_default().to_owned())
        }
        let server = Server::new(core())?;
        let first = boot_id(&server).await?;
        let server = Server::new(core())?;
        let second = boot_id(&server).await?;
        assert!(!first.is_empty(), "a boot should name itself: {first:?}");
        assert_ne!(first, second, "two boots share an id: {first}");
        Ok(())
    }

    #[cfg(feature = "ui")]
    #[tokio::test]
    async fn serves_embedded_index() -> anyhow::Result<()> {
        let (status, content_type, body) = get("/").await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type.as_deref(), Some("text/html"));
        assert!(String::from_utf8(body)?.contains("IroriOS"));
        Ok(())
    }

    #[cfg(feature = "ui")]
    #[tokio::test]
    async fn serves_svg_favicon() -> anyhow::Result<()> {
        let (status, content_type, body) = get("/favicon.svg").await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type.as_deref(), Some("image/svg+xml"));
        assert!(String::from_utf8(body)?.starts_with("<svg"));
        Ok(())
    }

    /// A core with the demo extension running and its first state reports in.
    async fn demo() -> anyhow::Result<(Core, irori_core::ExtensionHost)> {
        let core = core();
        let host = irori_core::ExtensionHost::start(
            &core,
            vec![
                irori_protocol::builtin::<irori_protocol_demo::Demo>()
                    .map_err(anyhow::Error::msg)?,
            ],
            irori_core::Timing::default(),
        )
        .map_err(anyhow::Error::msg)?;
        for _ in 0..500 {
            if core.states().iter().any(|s| s.state.is_some()) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Ok((core, host))
    }

    async fn post(
        core: Core,
        path: &str,
        body: serde_json::Value,
    ) -> anyhow::Result<(StatusCode, serde_json::Value)> {
        Server::new(core)?.json("POST", path, body).await
    }

    /// Everything the Devices page needs arrives in one response.
    #[tokio::test]
    async fn dev_home_describes_the_whole_home() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let (status, _, body) = get_from(core.clone(), "/api/home").await?;
        assert_eq!(status, StatusCode::OK);
        let home: serde_json::Value = serde_json::from_slice(&body)?;

        let named = |key: &str, id: &str| {
            home[key]
                .as_array()
                .is_some_and(|all| all.iter().any(|item| item["name"] == id))
        };
        assert!(
            named("devices", "Living room lamp"),
            "no demo lamp in {home}"
        );
        assert!(
            named("entities", "Living room lamp"),
            "no lamp entity in {home}"
        );
        assert!(
            named("devices", "Foyer ceiling light"),
            "no hallway light in {home}"
        );
        assert!(
            named("devices", "Foyer movement sensor"),
            "no movement sensor in {home}"
        );
        assert!(
            named("devices", "Garden light sensor"),
            "no luminosity sensor in {home}"
        );
        assert!(
            named("devices", "Living room presence sensor"),
            "no mmWave sensor in {home}"
        );
        assert_eq!(home["extensions"]["demo"]["state"], "running");
        let lamp = home["states"]
            .as_array()
            .and_then(|all| {
                all.iter()
                    .find(|s| s["entity_id"] == "light.living_room_lamp")
            })
            .ok_or_else(|| anyhow::anyhow!("no demo lamp state in {home}"))?;
        assert_eq!(lamp["availability"], "available");

        host.shutdown().await;
        Ok(())
    }

    /// The page turns the lamp on and is told what it became, without waiting for a refresh.
    #[tokio::test]
    async fn a_command_answers_with_the_state_it_produced() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let lamp = EntityId::try_from("light.living_room_lamp")?;

        for on in [true, false] {
            let (status, body) = post(
                core.clone(),
                "/api/command",
                serde_json::json!({
                    "entity_id": lamp, "command": if on { "turn_on" } else { "turn_off" },
                }),
            )
            .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["state"]["on"], on, "{body}");
            // The device is what changed, but it can be traced back to the click that asked it
            // to (`docs/specs/entities.md` §6).
            assert_eq!(body["context"]["origin"]["type"], "device");
            assert!(body["context"]["parent_id"].is_string(), "{body}");
            // The core's own view agrees: the answer isn't a hopeful echo of the request.
            let state = core
                .state(&lamp)
                .ok_or_else(|| anyhow::anyhow!("the lamp vanished"))?;
            assert!(
                matches!(&state.state, Some(irori_types::State::Light(light)) if light.on == on),
                "the core says {:?}, the answer said {on}",
                state.state
            );
        }

        host.shutdown().await;
        Ok(())
    }

    /// The per-entity history endpoint answers with the recorder's last day of changes, oldest
    /// first, and says plainly when the entity isn't one Irori knows.
    #[tokio::test]
    async fn history_reports_the_last_day_of_changes() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;
        let entity = core
            .states()
            .first()
            .map(|state| state.entity_id.clone())
            .ok_or_else(|| anyhow::anyhow!("the demo reported nothing"))?;

        // The server under test isn't subscribed to the core, so the change is handed to the
        // diary the endpoint reads.
        let today = core
            .state(&entity)
            .ok_or_else(|| anyhow::anyhow!("the demo entity vanished"))?;
        server.history.record(entity.clone(), today.clone());
        let earlier = core
            .state(&entity)
            .ok_or_else(|| anyhow::anyhow!("the demo entity vanished"))?;
        server.history.record(entity.clone(), earlier);

        let (status, body) = server
            .send(Request::get(format!("/api/history/{entity}")).body(Body::empty())?)
            .await?;
        assert_eq!(status, StatusCode::OK);
        let history: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(history["entity"], serde_json::json!(entity.to_string()));
        let states = history["states"].as_array().expect("a list of states");
        assert_eq!(states.len(), 2, "the two recorded changes: {history}");

        // An entity Irori has never heard of is refused, not answered with an empty table.
        let (status, _) = server
            .send(Request::get("/api/history/sensor.never_heard_of").body(Body::empty())?)
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);

        host.shutdown().await;
        Ok(())
    }

    /// Closing the diary and opening the same file again still answers the history endpoint.
    /// That is a restart: the process is gone, the database is not.
    #[tokio::test]
    async fn history_survives_reopening_the_database() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let mut server = Server::new(core.clone())?;
        let entity = core
            .states()
            .first()
            .map(|state| state.entity_id.clone())
            .ok_or_else(|| anyhow::anyhow!("the demo reported nothing"))?;
        let today = core
            .state(&entity)
            .ok_or_else(|| anyhow::anyhow!("the demo entity vanished"))?;
        server.history.record(entity.clone(), today);

        let (status, _) = server
            .send(Request::get(format!("/api/history/{entity}")).body(Body::empty())?)
            .await?;
        assert_eq!(
            status,
            StatusCode::OK,
            "the change is there before the close"
        );

        let path = server.dir.path().join("irori.db");
        let closed = std::mem::replace(&mut server.history, History::temporary());
        drop(closed);
        server.history = History::open(&path, irori_recorder::Retention::default())?;

        let (status, body) = server
            .send(Request::get(format!("/api/history/{entity}")).body(Body::empty())?)
            .await?;
        assert_eq!(status, StatusCode::OK);
        let history: serde_json::Value = serde_json::from_slice(&body)?;
        let states = history["states"].as_array().expect("a list of states");
        assert!(
            !states.is_empty(),
            "the change is still there after the database is opened again: {history}"
        );

        host.shutdown().await;
        Ok(())
    }

    /// Retention is read and written from Settings. A bad value is refused, and the file keeps
    /// every section except `[recorder]`.
    #[tokio::test]
    async fn recorder_settings_are_saved_and_checked() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core)?;
        let body = server.read("/api/recorder").await?;
        assert_eq!(body["retain_days"], 10);
        assert!(body["summary_days"].is_null(), "{body}");

        let (status, refused) = server
            .json(
                "PUT",
                "/api/recorder",
                serde_json::json!({"retain_days": 0}),
            )
            .await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            refused["error"]
                .as_str()
                .unwrap_or("")
                .contains("at least 1"),
            "{refused}"
        );

        let (status, refused) = server
            .json(
                "PUT",
                "/api/recorder",
                serde_json::json!({"retain_days": 30, "summary_days": 7}),
            )
            .await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");

        let (status, saved) = server
            .json(
                "PUT",
                "/api/recorder",
                serde_json::json!({"retain_days": 14, "summary_days": 365}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(saved["retain_days"], 14);
        assert_eq!(saved["summary_days"], 365);

        let body = server.read("/api/recorder").await?;
        assert_eq!(body["retain_days"], 14);
        assert_eq!(body["summary_days"], 365);
        let text = std::fs::read_to_string(server.config_dir().join("irori.toml"))?;
        assert!(text.contains("retain_days = 14\n"), "{text}");
        assert!(text.contains("summary_days = 365\n"), "{text}");
        assert!(
            !text.contains("[server]"),
            "writing history retention must not rewrite the rest of irori.toml: {text}"
        );

        let (status, _) = server
            .send(
                Request::get("/api/history/sensor.living_room_climate_sensor_temperature/summary")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, bytes) = server
            .send(
                Request::get(
                    "/api/history/sensor.living_room_climate_sensor_temperature/summary?since=1970-01-01T00:00:00Z",
                )
                .body(Body::empty())?,
            )
            .await?;
        assert_eq!(status, StatusCode::OK);
        let summary: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert!(summary["points"].is_array(), "{summary}");

        host.shutdown().await;
        Ok(())
    }

    /// A refused command says why, with a status that matches the reason.
    #[tokio::test]
    async fn refusals_explain_themselves() -> anyhow::Result<()> {
        let (core, host) = demo().await?;

        let (status, body) = post(
            core.clone(),
            "/api/command",
            serde_json::json!({"entity_id": "light.nowhere", "command": "toggle"}),
        )
        .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "there's no entity `light.nowhere`");

        // A sensor has nothing to turn on.
        let (status, body) = post(
            core.clone(),
            "/api/command",
            serde_json::json!({"entity_id": "sensor.living_room_climate_sensor_temperature", "command": "toggle"}),
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        // Brightness belongs to `turn_on`, and nowhere else: the refusal names the service.
        let (status, body) = post(
            core.clone(),
            "/api/command",
            serde_json::json!({
                "entity_id": "light.living_room_lamp", "command": "turn_off", "data": {"brightness": 5},
            }),
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "`light.turn_off` takes no data");

        host.shutdown().await;
        Ok(())
    }

    /// The demo's virtual devices show up in the read-only view.
    #[tokio::test]
    async fn dev_view_lists_demo_devices_and_states() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let (status, _, body) = get_from(core.clone(), "/api/states").await?;
        assert_eq!(status, StatusCode::OK);
        let states: serde_json::Value = serde_json::from_slice(&body)?;
        let lamp = states
            .as_array()
            .and_then(|all| {
                all.iter()
                    .find(|s| s["entity_id"] == "light.living_room_lamp")
            })
            .ok_or_else(|| anyhow::anyhow!("no demo lamp in {states}"))?;
        assert_eq!(lamp["state"]["kind"], "light");

        let (_, _, body) = get_from(core.clone(), "/api/extensions").await?;
        let extensions: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(extensions["demo"]["state"], "running");

        host.shutdown().await;
        Ok(())
    }

    /// The catalog's `icon` field is whether `extension_icon` can serve bytes for it *right now*
    /// (running, with an icon in its manifest) — not a static claim from the catalog file — so
    /// the Extensions page never points an `<img>` at a file that isn't there yet, or ever.
    #[tokio::test]
    async fn the_catalog_reports_whether_an_icon_is_actually_servable() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let (status, _, body) = get_from(core.clone(), "/api/catalog").await?;
        assert_eq!(status, StatusCode::OK);
        let catalog: serde_json::Value = serde_json::from_slice(&body)?;
        let entries = catalog.as_array().expect("a list");
        let demo = entries
            .iter()
            .find(|e| e["id"] == "demo")
            .expect("demo is official");
        assert_eq!(demo["icon"], true, "{catalog}");
        let mqtt = entries
            .iter()
            .find(|e| e["id"] == "mqtt")
            .expect("mqtt is official");
        assert_eq!(mqtt["icon"], false, "{catalog}");
        assert_eq!(mqtt["full_access"], false, "{catalog}");
        let zigbee = entries
            .iter()
            .find(|e| e["id"] == "zigbee")
            .expect("zigbee is official");
        assert_eq!(
            zigbee["full_access"], true,
            "zigbee downloads and runs other programs; the page has to be able to say so: {catalog}"
        );

        host.shutdown().await;
        Ok(())
    }

    /// Installing can take minutes. While it's underway the catalog says so, to every page that
    /// asks, and a second Install or an Uninstall is told it's underway instead of starting
    /// another copy. Once it's over, the extension is free again.
    #[tokio::test]
    async fn an_extension_being_installed_says_so_and_isnt_installed_twice() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let host = irori_core::ExtensionHost::start(
            &server.core,
            Vec::new(),
            irori_core::Timing::default(),
        )
        .map_err(anyhow::Error::msg)?;
        let app = server.app_with(host.clone())?;
        let call = |method: &str, path: &str| {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from("{}"));
            let app = app.clone();
            async move {
                let res = app.oneshot(request?).await?;
                let status = res.status();
                let bytes = res.into_body().collect().await?.to_bytes();
                anyhow::Ok((
                    status,
                    serde_json::from_slice::<serde_json::Value>(&bytes)
                        .unwrap_or(serde_json::Value::Null),
                ))
            }
        };
        let demo_busy = |catalog: &serde_json::Value| {
            catalog
                .as_array()
                .and_then(|all| all.iter().find(|entry| entry["id"] == "demo"))
                .map(|entry| entry["busy"].clone())
        };

        let demo = ExtensionId::try_from("demo")?;
        let claim = host
            .claim(&demo, irori_core::PackageWork::Installing)
            .map_err(anyhow::Error::msg)?;
        let (_, catalog) = call("GET", "/api/catalog").await?;
        assert_eq!(demo_busy(&catalog), Some(serde_json::json!("installing")));
        for (method, path) in [
            ("POST", "/api/extensions/demo/install"),
            ("DELETE", "/api/extensions/demo"),
        ] {
            let (status, body) = call(method, path).await?;
            assert_eq!(status, StatusCode::CONFLICT, "{method} {path}: {body}");
            assert!(
                body["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("`demo` is already installing")),
                "{body}"
            );
            assert_eq!(body["underway"], "installing", "{body}");
        }

        drop(claim);
        let (_, catalog) = call("GET", "/api/catalog").await?;
        assert_eq!(demo_busy(&catalog), Some(serde_json::Value::Null));
        host.shutdown().await;
        Ok(())
    }

    /// A bare install must not skip the warning. There are no accounts yet, so this is the
    /// acknowledgement itself, not a check of who sent it.
    #[tokio::test]
    async fn installing_zigbee_is_refused_until_full_access_is_approved() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core)?;
        let (status, body) = server
            .json(
                "POST",
                "/api/extensions/zigbee/install",
                serde_json::json!({}),
            )
            .await?;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|error| error.contains("full access to this machine")),
            "{body}"
        );
        host.shutdown().await;
        Ok(())
    }

    /// The URL install route sees the manifest only after the tarball is on disk. The same
    /// approval the official route requires has to apply to that manifest, or a third-party
    /// package skips the warning by using the other endpoint.
    #[test]
    fn a_downloaded_package_with_full_access_needs_the_same_approval() {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::write(
            dir.path().join("irori-extension.toml"),
            r#"
                [extension]
                id = "toolbox"
                name = "Toolbox"
                version = "0.1.0"
                irori = ">=0.0.0"

                [[contributes.protocol]]
                iot_class = "local_push"
                entity_kinds = ["switch"]

                [permissions]
                host_shell = true
            "#,
        )
        .expect("manifest");

        let refused = unapproved_full_access(dir.path(), None, false).expect_err("needs approval");
        assert_eq!(refused.0, StatusCode::CONFLICT);
        assert!(
            refused.1.contains("full access to this machine"),
            "{}",
            refused.1
        );
        assert!(unapproved_full_access(dir.path(), None, true).is_ok());
        let expected = ExtensionId::try_from("zigbee").expect("valid");
        let wrong = unapproved_full_access(dir.path(), Some(&expected), true)
            .expect_err("the manifest is toolbox, not zigbee");
        assert_eq!(wrong.0, StatusCode::CONFLICT);
        assert!(wrong.1.contains("not `zigbee`"), "{}", wrong.1);

        std::fs::write(
            dir.path().join("irori-extension.toml"),
            r#"
                [extension]
                id = "toolbox"
                name = "Toolbox"
                version = "0.1.0"
                irori = ">=0.0.0"

                [[contributes.protocol]]
                iot_class = "local_push"
                entity_kinds = ["switch"]

                [permissions]
                lan = true
            "#,
        )
        .expect("rewrite the manifest");
        assert!(
            unapproved_full_access(dir.path(), None, false).is_ok(),
            "lan alone is not full access"
        );
    }

    // --- Rooms and names ------------------------------------------------------------------

    /// The whole round trip: make a room, put a device in it, and find both on disk in files a
    /// person could have written themselves.
    #[tokio::test]
    async fn a_room_and_a_device_in_it_are_written_where_a_person_can_read_them()
    -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;

        let (status, area) = server
            .json(
                "POST",
                "/api/areas",
                serde_json::json!({"name": "Living room"}),
            )
            .await?;
        assert_eq!(status, StatusCode::CREATED, "{area}");
        assert_eq!(area["id"], "living_room");

        let (status, device) = server
            .json(
                "PATCH",
                "/api/devices/demo_living_room_lamp",
                serde_json::json!({
                    "name": "Reading lamp", "description": "On the desk", "area": "living_room",
                }),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{device}");
        assert_eq!(device["name"], "Reading lamp");
        assert_eq!(device["description"], "On the desk");
        assert_eq!(
            device["id"], "demo_living_room_lamp",
            "renaming it didn't move its id"
        );
        assert_eq!(device["area_id"], "living_room");

        let areas = std::fs::read_to_string(server.config_dir().join("areas.toml"))?;
        assert!(areas.contains("[areas.living_room]"), "{areas}");
        let devices = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;
        // Keyed by the device's one id — the same id as its page address — so there is never a
        // second identifier for the same device to keep in step (ROADMAP D36).
        assert!(
            devices.contains("[devices.demo_living_room_lamp]"),
            "{devices}"
        );
        assert!(
            devices.contains("description = \"On the desk\""),
            "{devices}"
        );
        assert!(devices.contains("name = \"Reading lamp\""), "{devices}");

        // And the page sees the same thing it would after a restart.
        let home = server.read("/api/home").await?;
        assert_eq!(home["areas"][0]["name"], "Living room");

        host.shutdown().await;
        Ok(())
    }

    /// Renaming a device renames the entities that were following its name, and taking the name
    /// away gives the protocol's name back rather than leaving the chosen one stuck.
    #[tokio::test]
    async fn a_rename_carries_the_entities_and_can_be_undone() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;
        let lamp = EntityId::try_from("light.living_room_lamp")?;
        let entity_named = |core: &Core| {
            core.entities()
                .into_iter()
                .find(|entity| entity.id == lamp)
                .map(|entity| entity.name.to_string())
        };
        assert_eq!(entity_named(&core).as_deref(), Some("Living room lamp"));

        let (status, _) = server
            .json(
                "PATCH",
                "/api/devices/demo_living_room_lamp",
                serde_json::json!({"name": "Reading lamp"}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(entity_named(&core).as_deref(), Some("Reading lamp"));

        let (status, device) = server
            .json(
                "PATCH",
                "/api/devices/demo_living_room_lamp",
                serde_json::json!({"name": null}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{device}");
        assert_eq!(device["name"], "Living room lamp");
        assert_eq!(entity_named(&core).as_deref(), Some("Living room lamp"));

        host.shutdown().await;
        Ok(())
    }

    /// An entity can be named on its own, and then it stops following its device.
    #[tokio::test]
    async fn an_entity_can_have_a_name_of_its_own() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;

        let (status, entity) = server
            .json(
                "PATCH",
                "/api/entities/light.living_room_lamp",
                serde_json::json!({"name": "Reading light"}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{entity}");
        assert_eq!(entity["name"], "Reading light");

        server
            .json(
                "PATCH",
                "/api/devices/demo_living_room_lamp",
                serde_json::json!({"name": "Reading lamp"}),
            )
            .await?;
        let home = server.read("/api/home").await?;
        let named = home["entities"]
            .as_array()
            .and_then(|all| all.iter().find(|e| e["id"] == "light.living_room_lamp"))
            .map(|e| e["name"].clone());
        assert_eq!(named, Some(serde_json::json!("Reading light")));

        host.shutdown().await;
        Ok(())
    }

    /// Deleting a room doesn't delete what was said about the devices in it: the device is
    /// unplaced, and making the room again puts it back.
    #[tokio::test]
    async fn deleting_a_room_unplaces_its_devices_without_forgetting_them() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;
        server
            .json(
                "POST",
                "/api/areas",
                serde_json::json!({"name": "Living room"}),
            )
            .await?;
        server
            .json(
                "PATCH",
                "/api/devices/demo_living_room_lamp",
                serde_json::json!({"area": "living_room"}),
            )
            .await?;

        let (status, _) = server
            .json("DELETE", "/api/areas/living_room", serde_json::json!(null))
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let placed = |core: &Core| {
            core.devices()
                .into_iter()
                .find(|device| device.id.as_str() == "demo_living_room_lamp")
                .and_then(|device| device.area_id)
        };
        assert_eq!(placed(&core), None);

        server
            .json(
                "POST",
                "/api/areas",
                serde_json::json!({"name": "Living room"}),
            )
            .await?;
        assert_eq!(
            placed(&core).map(|id| id.to_string()).as_deref(),
            Some("living_room"),
            "the device remembered where it belonged"
        );

        host.shutdown().await;
        Ok(())
    }

    /// Removing a device answers 204 and takes it out of the core and the config files —
    /// device, entities and all — while its neighbours stay, and it's listed as found at once.
    #[tokio::test]
    async fn forgetting_a_device_removes_it_and_writes_it_down() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;
        let before = core.devices().len();
        assert!(
            core.entities().iter().any(|entity| entity
                .device_id
                .as_ref()
                .is_some_and(|id| id.as_str() == "demo_living_room_lamp")),
            "the demo lamp has entities"
        );

        let (status, _) = server
            .json(
                "DELETE",
                "/api/devices/demo_living_room_lamp",
                serde_json::Value::Null,
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(
            !core
                .devices()
                .iter()
                .any(|device| device.id.as_str() == "demo_living_room_lamp"),
            "out of the core"
        );
        assert_eq!(core.devices().len(), before - 1, "its neighbours stayed");
        assert!(
            !core.entities().iter().any(|entity| entity
                .device_id
                .as_ref()
                .is_some_and(|id| id.as_str() == "demo_living_room_lamp")),
            "its entities went with it"
        );

        // The files agree, so a restart couldn't bring a stale row back.
        let devices = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;
        assert!(!devices.contains("demo_living_room_lamp"), "{devices}");

        // Found again straight away, for "+ Add device" to offer back.
        let home = server.read("/api/home").await?;
        assert!(
            home["held"]
                .as_array()
                .is_some_and(|all| all.iter().any(|item| item["id"] == "demo_living_room_lamp")),
            "{home}"
        );

        // Removing a device that isn't in the home is a 404, found or not.
        let (status, _) = server
            .json(
                "DELETE",
                "/api/devices/demo_living_room_lamp",
                serde_json::Value::Null,
            )
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);

        host.shutdown().await;
        Ok(())
    }

    /// "Not in a room" is an answer. The demo lamp's firmware asks for the living room; once that room
    /// exists the lamp is in it, and `area: false` must take it out and keep it out, while
    /// `area: null` hands the decision back to the device.
    #[tokio::test]
    async fn a_device_can_be_kept_out_of_the_room_it_asks_for() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;
        let placed = |core: &Core| {
            core.devices()
                .into_iter()
                .find(|device| device.id.as_str() == "demo_living_room_lamp")
                .and_then(|device| device.area_id)
                .map(|id| id.to_string())
        };
        server
            .json(
                "POST",
                "/api/areas",
                serde_json::json!({"name": "Living room"}),
            )
            .await?;
        assert_eq!(
            placed(&core).as_deref(),
            Some("living_room"),
            "the suggestion stands in"
        );

        let (status, body) = server
            .json(
                "PATCH",
                "/api/devices/demo_living_room_lamp",
                serde_json::json!({"area": false}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(placed(&core), None, "and a person can say no");
        let devices = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;
        assert!(devices.contains("area = false"), "{devices}");

        server
            .json(
                "PATCH",
                "/api/devices/demo_living_room_lamp",
                serde_json::json!({"area": null}),
            )
            .await?;
        assert_eq!(
            placed(&core).as_deref(),
            Some("living_room"),
            "or give it back to the device"
        );

        let (status, _) = server
            .json(
                "PATCH",
                "/api/devices/demo_living_room_lamp",
                serde_json::json!({"area": true}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "`true` names no room");

        host.shutdown().await;
        Ok(())
    }

    /// A plan drawn on the Floorplan page lands in `floorplan.toml` and comes back with the
    /// home, and one that couldn't be drawn is refused rather than written.
    #[tokio::test]
    async fn a_plan_is_saved_whole_and_a_door_has_to_fit_its_wall() -> anyhow::Result<()> {
        // A real device, because part of what saving a plan does is put devices in the rooms
        // they were drawn in.
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;

        let home = server.read("/api/home").await?;
        assert!(
            home.get("floorplan").is_none(),
            "a home nobody has drawn sends no plan: {home}"
        );

        let plan = serde_json::json!({
            "floors": {
                "ground": {
                    "walls": [{
                        "from": [0, 0],
                        "to": [400, 0],
                        "thickness": 20,
                        "openings": [{"kind": "door", "at": 200, "width": 80}],
                    }],
                    "areas": [{
                        "area": "kitchen",
                        "points": [[0, 0], [400, 0], [400, 300], [0, 300]],
                    }],
                    "devices": [{"device": "demo_living_room_lamp", "at": [120, 90]}],
                },
            },
        });
        let (status, body) = server.json("PUT", "/api/floorplan", plan.clone()).await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["placed"], 0,
            "there is no room called `kitchen` to put it in yet: {body}"
        );

        let written = std::fs::read_to_string(server.config_dir().join("floorplan.toml"))?;
        assert!(written.contains("from = [0, 0]"), "{written}");
        assert!(written.contains("kind = \"door\""), "{written}");
        assert!(written.contains("area = \"kitchen\""), "{written}");
        assert!(
            written.contains("device = \"demo_living_room_lamp\""),
            "{written}"
        );

        let home = server.read("/api/home").await?;
        assert_eq!(
            home["floorplan"]["floors"]["ground"]["walls"][0]["to"][0], 400,
            "{home}"
        );
        assert_eq!(server.read("/api/floorplan").await?, plan);

        // Make the room the lamp was drawn standing in, and saving the same plan again puts the
        // lamp in it: dragging a device onto a floor is a person saying where it is.
        server
            .json("POST", "/api/areas", serde_json::json!({"name": "Kitchen"}))
            .await?;
        let (_, body) = server.json("PUT", "/api/floorplan", plan.clone()).await?;
        assert_eq!(body["placed"], 1, "{body}");
        let devices = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;
        assert!(devices.contains("area = \"kitchen\""), "{devices}");

        // Saying so again moves nothing: it is where the plan says already.
        let (_, body) = server.json("PUT", "/api/floorplan", plan.clone()).await?;
        assert_eq!(body["placed"], 0, "{body}");

        // A copy of the lamp standing in another room doesn't move it: only the original says
        // where a device is.
        server
            .json("POST", "/api/areas", serde_json::json!({"name": "Hall"}))
            .await?;
        let mut copied = plan.clone();
        copied["floors"]["ground"]["areas"]
            .as_array_mut()
            .expect("areas")
            .push(serde_json::json!({
                "area": "hall",
                "points": [[500, 0], [900, 0], [900, 300], [500, 300]],
            }));
        copied["floors"]["ground"]["devices"]
            .as_array_mut()
            .expect("devices")
            .push(serde_json::json!({
                "device": "demo_living_room_lamp", "at": [600, 90], "copy": true,
            }));
        let (status, body) = server.json("PUT", "/api/floorplan", copied.clone()).await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["placed"], 0, "{body}");
        let devices = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;
        assert!(devices.contains("area = \"kitchen\""), "{devices}");

        // A plan that keeps only the copy, written by hand, makes it the original: the lamp is
        // then where the copy stands.
        let mut only_copy = copied.clone();
        only_copy["floors"]["ground"]["devices"]
            .as_array_mut()
            .expect("devices")
            .retain(|placed| placed["device"] != "demo_living_room_lamp" || placed["copy"] == true);
        let (status, body) = server.json("PUT", "/api/floorplan", only_copy).await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["placed"], 1, "{body}");
        let devices = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;
        assert!(devices.contains("area = \"hall\""), "{devices}");
        let saved = std::fs::read_to_string(server.config_dir().join("floorplan.toml"))?;
        assert!(!saved.contains("copy"), "{saved}");
        server.json("PUT", "/api/floorplan", plan.clone()).await?;

        // A device deliberately in no room stays in none. That answer exists so a guess can't
        // overrule it, and a dot standing on a floor is a guess.
        let (status, body) = server
            .json(
                "PATCH",
                "/api/devices/demo_living_room_lamp",
                serde_json::json!({"area": false}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, body) = server.json("PUT", "/api/floorplan", plan.clone()).await?;
        assert_eq!(body["placed"], 0, "{body}");
        let devices = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;
        assert!(devices.contains("area = false"), "{devices}");
        assert_eq!(
            core.devices()
                .iter()
                .find(|device| device.id.as_ref() == "demo_living_room_lamp")
                .and_then(|device| device.area_id.clone()),
            None,
            "and the core agrees"
        );

        // A plan naming a device the home has never heard of is kept and simply not drawn. It
        // must not become a `devices.toml` entry for a device nobody has.
        let (_, body) = server
            .json(
                "PUT",
                "/api/floorplan",
                serde_json::json!({
                    "floors": {
                        "ground": {
                            "areas": [{
                                "area": "kitchen",
                                "points": [[0, 0], [400, 0], [400, 300], [0, 300]],
                            }],
                            "devices": [{"device": "a_device_nobody_has", "at": [120, 90]}],
                        },
                    },
                }),
            )
            .await?;
        assert_eq!(body["placed"], 0, "{body}");
        let devices = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;
        assert!(!devices.contains("a_device_nobody_has"), "{devices}");
        // Put the drawn plan back, for what follows.
        server.json("PUT", "/api/floorplan", plan.clone()).await?;

        // A door wider than the wall it's in would have to be drawn hanging off the end.
        let (status, why) = server
            .json(
                "PUT",
                "/api/floorplan",
                serde_json::json!({
                    "floors": {
                        "ground": {
                            "walls": [{
                                "from": [0, 0],
                                "to": [100, 0],
                                "openings": [{"kind": "door", "at": 50, "width": 300}],
                            }],
                        },
                    },
                }),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}");
        assert!(
            why["error"]
                .as_str()
                .is_some_and(|e| e.contains("hangs off the end")),
            "{why}"
        );
        // And the refused plan changed nothing: the drawn one is still there.
        assert_eq!(server.read("/api/floorplan").await?, plan);

        host.shutdown().await;
        Ok(())
    }

    /// Floors are made, listed lowest first, and rooms go on them; removing a floor leaves its
    /// rooms where they are, on no floor.
    #[tokio::test]
    async fn floors_hold_rooms_and_removing_one_keeps_them() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (status, upstairs) = server
            .json(
                "POST",
                "/api/floors",
                serde_json::json!({"name": "Upstairs", "level": 1}),
            )
            .await?;
        assert_eq!(status, StatusCode::CREATED, "{upstairs}");
        server
            .json(
                "POST",
                "/api/floors",
                serde_json::json!({"name": "Ground floor"}),
            )
            .await?;
        let (status, room) = server
            .json(
                "POST",
                "/api/areas",
                serde_json::json!({"name": "Bedroom", "floor": "upstairs"}),
            )
            .await?;
        assert_eq!(status, StatusCode::CREATED, "{room}");
        assert_eq!(room["floor_id"], "upstairs");

        let home = server.read("/api/home").await?;
        assert_eq!(
            home["floors"][0]["id"], "ground_floor",
            "lowest first: {home}"
        );
        let areas = std::fs::read_to_string(server.config_dir().join("areas.toml"))?;
        assert!(
            areas.contains("[floors.upstairs]") && areas.contains("floor = \"upstairs\""),
            "{areas}"
        );

        let (status, _) = server
            .json(
                "POST",
                "/api/areas",
                serde_json::json!({"name": "Attic", "floor": "nowhere"}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = server
            .json("DELETE", "/api/floors/upstairs", serde_json::json!(null))
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let home = server.read("/api/home").await?;
        assert_eq!(home["areas"][0]["name"], "Bedroom", "the room stayed");

        let (status, moved) = server
            .json(
                "PATCH",
                "/api/areas/bedroom",
                serde_json::json!({"floor": "ground_floor"}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{moved}");
        assert_eq!(moved["floor_id"], "ground_floor");
        assert_eq!(
            moved["name"], "Bedroom",
            "a floor change leaves the name alone"
        );
        Ok(())
    }

    /// Edits that can't be made say why, and change nothing.
    #[tokio::test]
    async fn refused_edits_explain_themselves() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;
        let written = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;

        let (status, body) = server
            .json(
                "PATCH",
                "/api/devices/demo_living_room_lamp",
                serde_json::json!({"area": "nowhere"}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"], "there's no room `nowhere`");

        let (status, body) = server
            .json(
                "PATCH",
                "/api/devices/not_a_device",
                serde_json::json!({"name": "Nope"}),
            )
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

        let (status, body) = server
            .json(
                "PATCH",
                "/api/areas/nowhere",
                serde_json::json!({"name": "Nope"}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        // A name that isn't a name at all, rather than one that's merely wrong.
        let (status, body) = server
            .json("POST", "/api/areas", serde_json::json!({"name": ""}))
            .await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

        assert_eq!(
            std::fs::read_to_string(server.config_dir().join("devices.toml"))?,
            written,
            "nothing was written"
        );

        host.shutdown().await;
        Ok(())
    }

    // --- Secrets ----------------------------------------------------------------------------

    /// A protocol that won't do anything without a key, and says where the key goes.
    struct Safe;

    #[derive(Debug, Deserialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct SafeSettings {
        #[serde(default)]
        code: Option<String>,
        /// What's written on the door. Only the `relabel` way of `dial` changes it.
        #[serde(default)]
        label: Option<String>,
        /// Never read by `run` below; only here so a test can exercise the generic settings
        /// endpoint's schema-driven secret routing without needing a real external extension.
        #[serde(default)]
        #[expect(dead_code)]
        key: Option<TestSecret>,
    }

    #[derive(Debug, Deserialize)]
    struct TestSecret(#[expect(dead_code)] String);

    impl schemars::JsonSchema for TestSecret {
        fn schema_name() -> std::borrow::Cow<'static, str> {
            "TestSecret".into()
        }

        fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
            schemars::json_schema!({ "type": "string", "writeOnly": true })
        }
    }

    impl irori_protocol::Protocol for Safe {
        type Config = SafeSettings;
        const MANIFEST: &'static str = r#"
            [extension]
            id = "safe"
            name = "Safe"
            version = "0.1.0"
            irori = ">=0.0.0"

            [[contributes.protocol]]
            iot_class = "local_push"
            entity_kinds = ["switch"]

            [[contributes.protocol.actions]]
            id = "open"
            label = "Open the safe"

            # Asks for text, like Matter's pairing code, and takes its time answering. One of
            # its ways changes a setting first, like Matter's Wi-Fi for a new device.
            [[contributes.protocol.actions]]
            id = "dial"
            label = "Dial"
            answer_within_seconds = 30

            [[contributes.protocol.actions.ways]]
            id = "typed"
            label = "Type it"
            fields = [{ id = "combination", label = "Combination", hint = "On the back of the door" }]

            [[contributes.protocol.actions.ways]]
            id = "relabel"
            label = "Relabel the door, then dial"
            settings = ["label"]
            fields = [{ id = "combination", label = "Combination" }]
        "#;
        async fn run(
            settings: SafeSettings,
            mut ctx: irori_protocol::ProtocolContext,
        ) -> Result<(), irori_protocol::ProtocolError> {
            if settings.code.is_none() {
                ctx.set_waiting(vec![irori_types::Waiting {
                    unique_id: "vault".parse()?,
                    name: "Vault".parse()?,
                    reason: "it wants a code".into(),
                    secret: Some(irori_types::SecretRequest {
                        path: vec!["code".into()],
                        label: "Code".into(),
                        hint: None,
                    }),
                }])
                .await;
            }
            // Its network: itself, and the vault it reaches. A map that isn't one (a link to
            // nothing) is refused and the last good one kept.
            let node = |id: &str, role| irori_types::NetworkNode {
                id: id.to_owned(),
                name: id.to_owned(),
                role,
                medium: None,
                device_unique_id: None,
                detail: None,
                host: None,
            };
            let link = |from: &str, to: &str| irori_types::NetworkLink {
                from: from.to_owned(),
                to: to.to_owned(),
                medium: Some(irori_types::NetworkMedium::Thread),
                quality: Some(irori_types::LinkQuality::Good),
                reading: Some("-58 dBm".to_owned()),
                detail: None,
            };
            ctx.set_network(irori_types::Network {
                name: Some("Safe mesh".to_owned()),
                nodes: vec![
                    node("safe", irori_types::NetworkRole::Controller),
                    node("vault", irori_types::NetworkRole::SleepyEndDevice),
                ],
                links: vec![link("vault", "safe")],
                ..irori_types::Network::default()
            })
            .await;
            ctx.set_network(irori_types::Network {
                nodes: vec![node("safe", irori_types::NetworkRole::Controller)],
                links: vec![link("nowhere", "safe")],
                ..irori_types::Network::default()
            })
            .await;
            // Only offered once it has a code — same idea as `zigbee`'s permit_join only
            // showing up once it's actually found a Z2M bridge.
            if settings.code.is_some() {
                ctx.set_available_actions(vec!["open".to_owned(), "dial".to_owned()])
                    .await;
            }
            while let Some(incoming) = ctx.next_action().await {
                let dialled = incoming.field("combination");
                let answer = match (incoming.action_id.as_str(), incoming.way.as_deref()) {
                    ("open", None) => Ok(()),
                    ("dial", Some(_)) if dialled != settings.code.as_deref() => {
                        Err("that's not the combination".to_owned())
                    }
                    ("dial", Some("typed")) => Ok(()),
                    ("dial", Some("relabel")) => match &settings.label {
                        Some(label) => Err(format!("dialled; the door says {label}")),
                        None => Err("the door has no label".to_owned()),
                    },
                    (action, way) => Err(format!("unexpected {action} by {way:?}")),
                };
                incoming.reply(answer);
            }
            Ok(())
        }
    }

    /// A protocol with a network of its own, like Zigbee's: a device is paired to it, a timed
    /// action opens it to new ones, and removing a device has to unpair it.
    #[derive(Debug)]
    struct Mesh;

    #[derive(Debug, Deserialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct MeshSettings {}

    impl irori_protocol::Protocol for Mesh {
        type Config = MeshSettings;
        const MANIFEST: &'static str = r#"
            [extension]
            id = "mesh"
            name = "Mesh"
            version = "0.1.0"
            irori = ">=0.0.0"

            [[contributes.protocol]]
            iot_class = "local_push"
            entity_kinds = ["switch"]
            unpairs = true

            [[contributes.protocol.actions]]
            id = "join"
            label = "Permit joining"
            seconds = 60
        "#;
        async fn run(
            _: MeshSettings,
            mut ctx: irori_protocol::ProtocolContext,
        ) -> Result<(), irori_protocol::ProtocolError> {
            for device in ["bulb", "sleepy", "stuck", "hub"] {
                ctx.describe_device(irori_types::DeviceDescription {
                    unique_id: device.parse()?,
                    name: device.parse()?,
                    manufacturer: None,
                    model: None,
                    sw_version: None,
                    hw_version: None,
                    serial_number: None,
                    suggested_area: None,
                    via_device_unique_id: None,
                })
                .await?;
                ctx.describe_entity(irori_types::EntityDescription {
                    unique_id: format!("{device}_switch").parse()?,
                    name: None,
                    device_unique_id: Some(device.parse()?),
                    suggested_object_id: None,
                    capabilities: irori_types::Capabilities::Switch(
                        irori_types::SwitchCapabilities { device_class: None },
                    ),
                    entity_category: None,
                })
                .await?;
            }
            ctx.set_available_actions(vec!["join".to_owned()]).await;
            while let Some(incoming) = ctx.next().await {
                match incoming {
                    irori_protocol::Incoming::Action(action) => {
                        let left = (!action.stop).then_some(Duration::from_secs(60));
                        ctx.set_action_open("join", left).await;
                        action.reply(Ok(()));
                    }
                    // The sleepy one never answers, so only force gets rid of it.
                    irori_protocol::Incoming::Unpair(unpair)
                        if unpair.unique_id.as_str() == "sleepy" && !unpair.force =>
                    {
                        unpair.reply(Err("it didn't answer".into()));
                    }
                    // The network can't be reached for this one at all, forced or not.
                    irori_protocol::Incoming::Unpair(unpair)
                        if unpair.unique_id.as_str() == "stuck" =>
                    {
                        unpair.reply(Err("the radio is unplugged".into()));
                    }
                    // The network itself: nothing to unpair, said as done, and left alone.
                    irori_protocol::Incoming::Unpair(unpair)
                        if unpair.unique_id.as_str() == "hub" =>
                    {
                        unpair.reply(Ok(()));
                    }
                    irori_protocol::Incoming::Unpair(unpair) => {
                        ctx.remove_device(unpair.unique_id.clone()).await?;
                        unpair.reply(Ok(()));
                    }
                    irori_protocol::Incoming::Call(call) => call.reply(Ok(())),
                    irori_protocol::Incoming::Picture(asked) => {
                        asked.reply(Err("no cameras on this mesh".into()));
                    }
                }
            }
            Ok(())
        }
    }

    async fn mesh() -> anyhow::Result<(Core, irori_core::ExtensionHost)> {
        let core = core();
        let host = mesh_on(&core).await?;
        Ok((core, host))
    }

    /// Starts the mesh on `core`. Started after the server, its devices are only found;
    /// before it, the server's own setup has already added them to the home.
    async fn mesh_on(core: &Core) -> anyhow::Result<irori_core::ExtensionHost> {
        let host = irori_core::ExtensionHost::start(
            core,
            vec![irori_protocol::builtin::<Mesh>().map_err(anyhow::Error::msg)?],
            irori_core::Timing::default(),
        )
        .map_err(anyhow::Error::msg)?;
        for _ in 0..500 {
            let ready = core.held_devices().len() + core.devices().len() == 4
                && core
                    .extensions()
                    .values()
                    .any(|overview| !overview.available_actions.is_empty());
            if ready {
                return Ok(host);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        anyhow::bail!(
            "the mesh never found its devices: {:?} {:?}",
            core.held_devices(),
            core.extensions()
        )
    }

    fn known(core: &Core, id: &str) -> bool {
        core.devices().iter().any(|device| device.id.as_str() == id)
            || core
                .held_devices()
                .iter()
                .any(|device| device.id.as_str() == id)
    }

    /// Removing a device whose protocol keeps a network unpairs it: it's gone, not back among
    /// what was found, and nothing of it is left in the config files.
    #[tokio::test]
    async fn removing_a_paired_device_takes_it_off_its_network() -> anyhow::Result<()> {
        let (core, host) = mesh().await?;
        let server = Server::new(core.clone())?;
        let home = server.read("/api/home").await?;
        assert_eq!(home["extensions"]["mesh"]["unpairs"], true, "{home}");

        let (status, body) = server
            .json(
                "PATCH",
                "/api/devices/mesh_bulb",
                serde_json::json!({"name": "Reading lamp"}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(core.devices().iter().any(|d| d.id.as_str() == "mesh_bulb"));
        let written = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;
        assert!(written.contains("mesh_bulb"), "{written}");

        let (status, body) = server
            .json("DELETE", "/api/devices/mesh_bulb", serde_json::Value::Null)
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert!(!known(&core, "mesh_bulb"), "neither in the home nor found");
        assert!(
            !core
                .entities()
                .iter()
                .any(|entity| entity.unique_id.as_str() == "bulb_switch")
        );
        let written = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;
        assert!(!written.contains("mesh_bulb"), "{written}");

        host.shutdown().await;
        Ok(())
    }

    /// A protocol that retires its one device, as the demo does its old ones: told anything,
    /// it removes the lamp for good.
    #[derive(Debug)]
    struct Retiring;

    #[derive(Debug, Deserialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct RetiringSettings {}

    impl irori_protocol::Protocol for Retiring {
        type Config = RetiringSettings;
        const MANIFEST: &'static str = r#"
            [extension]
            id = "retiring"
            name = "Retiring"
            version = "0.1.0"
            irori = ">=0.0.0"

            [[contributes.protocol]]
            iot_class = "local_push"
            entity_kinds = ["switch"]
        "#;
        async fn run(
            _: RetiringSettings,
            mut ctx: irori_protocol::ProtocolContext,
        ) -> Result<(), irori_protocol::ProtocolError> {
            ctx.describe_device(irori_types::DeviceDescription {
                unique_id: "lamp".parse()?,
                name: "Old lamp".parse()?,
                manufacturer: None,
                model: None,
                sw_version: None,
                hw_version: None,
                serial_number: None,
                suggested_area: None,
                via_device_unique_id: None,
            })
            .await?;
            ctx.describe_entity(irori_types::EntityDescription {
                unique_id: "lamp_switch".parse()?,
                name: None,
                device_unique_id: Some("lamp".parse()?),
                suggested_object_id: None,
                capabilities: irori_types::Capabilities::Switch(irori_types::SwitchCapabilities {
                    device_class: None,
                }),
                entity_category: None,
            })
            .await?;
            while let Some(call) = ctx.next_call().await {
                let gone = ctx.remove_device("lamp".parse()?).await;
                call.reply(gone.map_err(|e| irori_protocol::ServiceError::failed(e.to_string())));
            }
            Ok(())
        }
    }

    /// A device its own protocol retires leaves the config files as one a person removes does:
    /// its row, its entities' rows and its spot on the plan. Otherwise it would linger there.
    #[tokio::test]
    async fn a_device_its_protocol_retires_is_forgotten_by_the_files() -> anyhow::Result<()> {
        let core = core();
        let server = Server::new(core.clone())?;
        tokio::spawn(server.config.clone().forget_retired(core.clone()));
        let host = irori_core::ExtensionHost::start(
            &core,
            vec![irori_protocol::builtin::<Retiring>().map_err(anyhow::Error::msg)?],
            irori_core::Timing::default(),
        )
        .map_err(anyhow::Error::msg)?;
        for _ in 0..500 {
            if known(&core, "retiring_lamp") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let (status, body) = server
            .json(
                "PATCH",
                "/api/devices/retiring_lamp",
                serde_json::json!({"name": "Hall lamp"}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        let devices = || std::fs::read_to_string(server.config_dir().join("devices.toml"));
        assert!(devices()?.contains("retiring_lamp"), "{}", devices()?);

        let (status, body) = server
            .json(
                "POST",
                "/api/command",
                serde_json::json!({"entity_id": "switch.retiring_lamp", "command": "turn_on"}),
            )
            .await?;
        assert!(
            status.is_success() || status == StatusCode::NOT_FOUND,
            "{status} {body}"
        );
        for _ in 0..500 {
            if !devices()?.contains("retiring_lamp") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!devices()?.contains("retiring_lamp"), "{}", devices()?);
        assert!(!known(&core, "retiring_lamp"));
        host.shutdown().await;
        Ok(())
    }

    /// A device that doesn't answer stays exactly as it was, and the answer says so in a way
    /// the page can tell apart, so it can offer to drop it anyway. One that was only ever
    /// found can be unpaired too.
    #[tokio::test]
    async fn a_device_that_will_not_unpair_stays_until_it_is_forced() -> anyhow::Result<()> {
        let core = core();
        let server = Server::new(core.clone())?;
        let host = mesh_on(&core).await?;
        assert_eq!(core.held_devices().len(), 4, "found, and not in the home");

        let (status, body) = server
            .json(
                "DELETE",
                "/api/devices/mesh_sleepy",
                serde_json::Value::Null,
            )
            .await?;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "unpair_failed");
        assert_eq!(body["error"], "it didn't answer");
        assert!(known(&core, "mesh_sleepy"));

        let (status, body) = server
            .json(
                "DELETE",
                "/api/devices/mesh_sleepy?force=true",
                serde_json::Value::Null,
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert!(!known(&core, "mesh_sleepy"));

        host.shutdown().await;
        Ok(())
    }

    /// Removing from the home never becomes impossible. A device whose network can't be asked
    /// at all still leaves the home when forced, and the network's own controller, which has
    /// nothing to unpair, is removed like any device used to be: both are found again.
    #[tokio::test]
    async fn a_device_can_always_be_removed_from_the_home() -> anyhow::Result<()> {
        let (core, host) = mesh().await?;
        let server = Server::new(core.clone())?;
        let in_home = |id: &str| core.devices().iter().any(|device| device.id.as_str() == id);
        let found = |id: &str| {
            core.held_devices()
                .iter()
                .any(|device| device.id.as_str() == id)
        };
        let remove = |path: &'static str| server.json("DELETE", path, serde_json::Value::Null);

        let (status, body) = remove("/api/devices/mesh_stuck").await?;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "unpair_failed");
        assert!(in_home("mesh_stuck"));
        let (status, body) = remove("/api/devices/mesh_stuck?force=true").await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert!(!in_home("mesh_stuck") && found("mesh_stuck"));
        // Only found now, there's nothing left that forcing could do for it.
        let (status, body) = remove("/api/devices/mesh_stuck?force=true").await?;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");

        let (status, body) = remove("/api/devices/mesh_hub").await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert!(!in_home("mesh_hub") && found("mesh_hub"));
        let (status, body) = remove("/api/devices/mesh_hub").await?;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|why| why.contains("nothing to unpair"))
        );
        let written = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;
        assert!(
            !written.contains("mesh_hub") && !written.contains("mesh_stuck"),
            "{written}"
        );

        host.shutdown().await;
        Ok(())
    }

    /// A timed action says how long it has left, and can be ended before then.
    #[tokio::test]
    async fn an_open_action_counts_down_and_can_be_closed() -> anyhow::Result<()> {
        let (core, host) = mesh().await?;
        let server = Server::new(core.clone())?;
        let open = |home: &serde_json::Value| {
            home["extensions"]["mesh"]["open_actions"]["join"]["closes_in_ms"].as_u64()
        };
        assert_eq!(open(&server.read("/api/home").await?), None);

        let (status, body) = server
            .json(
                "POST",
                "/api/extensions/mesh/actions/join",
                serde_json::Value::Null,
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let mut left = None;
        for _ in 0..500 {
            left = open(&server.read("/api/home").await?);
            if left.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            left.is_some_and(|left| left > 50_000 && left <= 60_000),
            "{left:?}"
        );

        let (status, body) = server
            .json(
                "DELETE",
                "/api/extensions/mesh/actions/join",
                serde_json::Value::Null,
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        for _ in 0..500 {
            left = open(&server.read("/api/home").await?);
            if left.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(left, None, "closed");

        host.shutdown().await;
        Ok(())
    }

    async fn safe() -> anyhow::Result<(Core, irori_core::ExtensionHost)> {
        let core = core();
        let host = irori_core::ExtensionHost::start(
            &core,
            vec![irori_protocol::builtin::<Safe>().map_err(anyhow::Error::msg)?],
            irori_core::Timing::default(),
        )
        .map_err(anyhow::Error::msg)?;
        waiting_for(&core, 1).await?;
        Ok((core, host))
    }

    async fn waiting_for(core: &Core, count: usize) -> anyhow::Result<()> {
        for _ in 0..500 {
            let waiting = core
                .extensions()
                .get(&ExtensionId::try_from("safe")?)
                .map_or(0, |overview| overview.waiting.len());
            if waiting == count {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        anyhow::bail!("the safe never had {count} waiting")
    }

    async fn available_actions_for(core: &Core, count: usize) -> anyhow::Result<()> {
        for _ in 0..500 {
            let available = core
                .extensions()
                .get(&ExtensionId::try_from("safe")?)
                .map_or(0, |overview| overview.available_actions.len());
            if available == count {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        anyhow::bail!("the safe never had {count} available actions")
    }

    /// The whole path of a secret: the page shows what's waiting, sends the secret to the place
    /// it was asked for, and the extension is restarted with it — and it's written somewhere only
    /// Irori's user can read, and never handed back.
    #[tokio::test]
    async fn a_secret_given_where_it_was_asked_for_unlocks_the_extension() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        let home = server.read("/api/home").await?;
        assert_eq!(home["extensions"]["safe"]["waiting"][0]["name"], "Vault");
        assert_eq!(
            home["extensions"]["safe"]["waiting"][0]["secret"]["path"],
            serde_json::json!(["code"])
        );

        let (status, body) = server
            .json(
                "PUT",
                "/api/extensions/safe/secrets",
                serde_json::json!({"path": ["code"], "value": "1234-5678"}),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert!(
            !body.to_string().contains("1234"),
            "the secret came back: {body}"
        );

        waiting_for(&core, 0).await?;
        let written = std::fs::read_to_string(server.config_dir().join("secrets.toml"))?;
        assert!(
            written.contains("[safe]") && written.contains("code = \"1234-5678\""),
            "{written}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(server.config_dir().join("secrets.toml"))?
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        }
        let home = server.read("/api/home").await?;
        assert!(
            !home.to_string().contains("1234"),
            "a secret leaked into the home view"
        );

        host.shutdown().await;
        Ok(())
    }

    /// Without sign-in, this endpoint can only answer a question an extension is asking. It can't
    /// put anything anywhere else in anyone's settings.
    #[tokio::test]
    async fn a_secret_nobody_asked_for_is_refused() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        let (status, _) = server
            .json(
                "PUT",
                "/api/extensions/safe/secrets",
                serde_json::json!({"path": ["something_else"], "value": "x"}),
            )
            .await?;
        assert_eq!(status, StatusCode::CONFLICT);

        let (status, _) = server
            .json(
                "PUT",
                "/api/extensions/nope/secrets",
                serde_json::json!({"path": ["code"], "value": "x"}),
            )
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = server
            .json(
                "PUT",
                "/api/extensions/safe/secrets",
                serde_json::json!({"path": ["code"], "value": "   "}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        assert!(
            !server.config_dir().join("secrets.toml").exists(),
            "nothing was written"
        );
        host.shutdown().await;
        Ok(())
    }

    // --- Generic settings (the gear icon) ----------------------------------------------------

    /// A non-secret field goes to `extensions/<id>.toml` and restarts the extension with it —
    /// same outcome as giving a secret, through the generic form instead of the waiting-item one.
    #[tokio::test]
    async fn generic_settings_reach_the_extension_and_restart_it() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        let (status, body) = server
            .json(
                "POST",
                "/api/extensions/safe/settings",
                serde_json::json!({"code": "1234-5678"}),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        waiting_for(&core, 0).await?;
        let written = std::fs::read_to_string(server.config_dir().join("extensions/safe.toml"))?;
        assert!(written.contains("code = \"1234-5678\""), "{written}");
        assert!(
            !server.config_dir().join("secrets.toml").exists(),
            "a non-secret field must not land in secrets.toml"
        );

        host.shutdown().await;
        Ok(())
    }

    /// A schema-valid `null` for an `Option<T>` field means "leave this unset" — it must clear
    /// the field, not get handed to TOML (which has no `null`) and silently blank the whole file.
    #[tokio::test]
    async fn a_null_value_unsets_the_field_instead_of_erasing_the_file() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        let (status, body) = server
            .json(
                "POST",
                "/api/extensions/safe/settings",
                serde_json::json!({"code": "1234-5678"}),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        let (status, body) = server
            .json(
                "POST",
                "/api/extensions/safe/settings",
                serde_json::json!({"code": null}),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        let written = std::fs::read_to_string(server.config_dir().join("extensions/safe.toml"))?;
        assert!(
            !written.contains("1234-5678"),
            "the field should have been cleared: {written}"
        );

        host.shutdown().await;
        Ok(())
    }

    /// The home's networks are there to read while their extensions run, each saying who drew
    /// it; one that isn't a map is refused, and the last good one stays.
    #[tokio::test]
    async fn the_homes_networks_are_there_to_read() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        let answer = server.read("/api/network").await?;
        let network = &answer["networks"][0];
        assert_eq!(network["extension"], "safe", "{answer}");
        assert_eq!(network["name"], "Safe mesh", "{answer}");
        assert_eq!(network["nodes"][1]["id"], "vault", "{answer}");
        assert_eq!(network["links"][0]["quality"], "good", "{answer}");
        assert_eq!(network["links"][0]["reading"], "-58 dBm", "{answer}");
        assert_eq!(answer["networks"].as_array().map(Vec::len), Some(1));

        host.shutdown().await;
        Ok(())
    }

    /// A `writeOnly` field — schemars' shape for an `Option<Secret>`-like type — is routed to
    /// `secrets.toml` instead, the same file a waiting-item secret goes to.
    #[tokio::test]
    async fn a_write_only_field_is_routed_to_secrets_toml() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        let (status, body) = server
            .json(
                "POST",
                "/api/extensions/safe/settings",
                serde_json::json!({"key": "shh"}),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        let written = std::fs::read_to_string(server.config_dir().join("secrets.toml"))?;
        assert!(
            written.contains("[safe]") && written.contains("key = \"shh\""),
            "{written}"
        );
        assert!(
            !server.config_dir().join("extensions/safe.toml").exists()
                || !std::fs::read_to_string(server.config_dir().join("extensions/safe.toml"))?
                    .contains("shh"),
            "a secret field must not land in extensions/<id>.toml"
        );

        host.shutdown().await;
        Ok(())
    }

    /// The catalog names a saved secret so its form can say so, and names nothing the schema
    /// doesn't call a secret: a stray key in `secrets.toml` isn't vouched for.
    #[test]
    fn only_the_schemas_own_secrets_are_named_as_saved() {
        let schema = serde_json::json!({
            "properties": {
                "password": {"anyOf": [{"$ref": "#/$defs/Secret"}, {"type": "null"}]},
                "name": {"type": "string"}
            },
            "$defs": {"Secret": {"type": "string", "writeOnly": true}}
        });
        let saved = ["password", "name", "stray"].map(str::to_owned);
        assert_eq!(saved_secrets(&saved, &schema), ["password".to_owned()]);
    }

    /// A bad secret value is caught before the non-secret half of the same request is written —
    /// otherwise a request with both a valid `code` and an invalid `key` would save `code` and
    /// then fail on `key`, leaving the request half-applied.
    #[tokio::test]
    async fn an_invalid_secret_value_leaves_the_non_secret_half_unwritten() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        let (status, body) = server
            .json(
                "POST",
                "/api/extensions/safe/settings",
                serde_json::json!({"code": "1234-5678", "key": 42}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            !server.config_dir().join("extensions/safe.toml").exists(),
            "the non-secret field must not be written when the secret field is invalid"
        );
        assert!(
            !server.config_dir().join("secrets.toml").exists(),
            "nothing was written"
        );

        host.shutdown().await;
        Ok(())
    }

    /// Scoped to exactly the fields the extension's own schema declares — not a way to write
    /// anything into anyone's settings (`docs/specs/config.md` §3.6).
    #[tokio::test]
    async fn unknown_settings_keys_are_refused() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        let (status, body) = server
            .json(
                "POST",
                "/api/extensions/safe/settings",
                serde_json::json!({"not_a_real_field": "x"}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            !server.config_dir().join("extensions/safe.toml").exists(),
            "nothing was written"
        );

        let (status, _) = server
            .json(
                "POST",
                "/api/extensions/nope/settings",
                serde_json::json!({"code": "x"}),
            )
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);

        host.shutdown().await;
        Ok(())
    }

    // --- Protocol actions (the "+ Add device" button, e.g. Zigbee's permit_join) ------------

    /// An action declared in the manifest, and said to be available right now, actually runs.
    #[tokio::test]
    async fn a_declared_and_available_action_can_be_triggered() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        // Give it its code, which is what makes `open` available (see `Safe::run`).
        server
            .json(
                "PUT",
                "/api/extensions/safe/secrets",
                serde_json::json!({"path": ["code"], "value": "1234-5678"}),
            )
            .await?;
        waiting_for(&core, 0).await?;
        available_actions_for(&core, 2).await?;

        let (status, body) = server
            .json(
                "POST",
                "/api/extensions/safe/actions/open",
                serde_json::json!(null),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        host.shutdown().await;
        Ok(())
    }

    /// An action with ways gets the way and what was typed in its fields, trimmed. A missing
    /// way, a blank or unknown field, a setting the way doesn't list, anything for a plain
    /// button, and text too long to be a code are refused before they reach the extension.
    /// (How long the core waits for it is `irori-core`'s test.)
    #[tokio::test]
    async fn an_action_with_ways_gets_what_was_typed() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;
        server
            .json(
                "PUT",
                "/api/extensions/safe/secrets",
                serde_json::json!({"path": ["code"], "value": "1234-5678"}),
            )
            .await?;
        waiting_for(&core, 0).await?;
        available_actions_for(&core, 2).await?;
        let dial = "/api/extensions/safe/actions/dial";
        let typed = |combination: &str| serde_json::json!({"way": "typed", "fields": {"combination": combination}});

        let (status, body) = server.json("POST", dial, typed(" 1234-5678 ")).await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        let (status, body) = server.json("POST", dial, typed("0000")).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.to_string().contains("not the combination"), "{body}");

        let refusals = [
            (serde_json::json!(null), "choose one of the ways"),
            (
                serde_json::json!({"fields": {"combination": "1234-5678"}}),
                "choose one of the ways",
            ),
            (typed("   "), "Combination"),
            (serde_json::json!({"way": "typed"}), "Combination"),
            (
                serde_json::json!({"way": "spin", "fields": {"combination": "1"}}),
                "no way `spin`",
            ),
            (
                serde_json::json!({"way": "typed", "fields": {"combination": "1", "pin": "2"}}),
                "no field `pin`",
            ),
            (
                serde_json::json!({
                    "way": "typed",
                    "fields": {"combination": "1234-5678"},
                    "settings": {"label": "Front"},
                }),
                "doesn't change `label`",
            ),
            (
                typed(&"9".repeat(irori_types::MAX_ACTION_INPUT + 1)),
                "too long",
            ),
        ];
        for (sent, why) in refusals {
            let (status, body) = server.json("POST", dial, sent.clone()).await?;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{sent}: {body}");
            assert!(body.to_string().contains(why), "{sent}: {body}");
        }

        let (status, body) = server
            .json(
                "POST",
                "/api/extensions/safe/actions/open",
                serde_json::json!({"way": "typed"}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.to_string().contains("plain button"), "{body}");

        // The field this used to be is now a body nobody understands.
        let (status, _) = server
            .json("POST", dial, serde_json::json!({"input": "1234-5678"}))
            .await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

        host.shutdown().await;
        Ok(())
    }

    /// A way that carries settings saves them first and waits for the extension to start again
    /// with them, so the action runs on the new settings. Settings that change nothing don't
    /// restart it.
    #[tokio::test]
    async fn a_way_with_settings_runs_on_the_new_settings() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;
        server
            .json(
                "PUT",
                "/api/extensions/safe/secrets",
                serde_json::json!({"path": ["code"], "value": "1234-5678"}),
            )
            .await?;
        waiting_for(&core, 0).await?;
        available_actions_for(&core, 2).await?;
        let safe_id = ExtensionId::try_from("safe")?;
        let starts = || core.extensions()[&safe_id].starts;
        let relabel = serde_json::json!({
            "way": "relabel",
            "fields": {"combination": "1234-5678"},
            "settings": {"label": "Front"},
        });

        let before = starts();
        let (status, body) = server
            .json("POST", "/api/extensions/safe/actions/dial", relabel.clone())
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body.to_string().contains("the door says Front"),
            "it answered after starting again: {body}"
        );
        assert_eq!(starts(), before + 1);
        assert!(
            std::fs::read_to_string(server.config_dir().join("extensions/safe.toml"))?
                .contains("Front")
        );

        let (_, body) = server
            .json("POST", "/api/extensions/safe/actions/dial", relabel)
            .await?;
        assert!(body.to_string().contains("the door says Front"), "{body}");
        assert_eq!(starts(), before + 1, "the same label doesn't restart it");

        host.shutdown().await;
        Ok(())
    }

    /// An action nobody declared, one that isn't available yet, and an unknown extension are
    /// all refused rather than reaching the extension.
    #[tokio::test]
    async fn an_undeclared_or_unavailable_action_is_refused() -> anyhow::Result<()> {
        let (core, host) = safe().await?;
        let server = Server::new(core.clone())?;

        let (status, _) = server
            .json(
                "POST",
                "/api/extensions/safe/actions/not_a_real_action",
                serde_json::json!(null),
            )
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Declared in the manifest, but not yet available: no code has been given.
        let (status, _) = server
            .json(
                "POST",
                "/api/extensions/safe/actions/open",
                serde_json::json!(null),
            )
            .await?;
        assert_eq!(status, StatusCode::CONFLICT);

        let (status, _) = server
            .json(
                "POST",
                "/api/extensions/nope/actions/open",
                serde_json::json!(null),
            )
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);

        host.shutdown().await;
        Ok(())
    }

    /// A found device joins the home when "+ Add device" adds it, with its entities, and it's
    /// written down so a restart keeps it. There's no ignoring any more: taking a device out is
    /// removing it, so a request that still asks to ignore one is refused rather than
    /// half-understood.
    #[tokio::test]
    async fn a_found_device_is_added_and_ignoring_is_no_longer_a_thing() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        // Nothing added: an empty `devices.toml` over the harness's.
        let server = Server::new(core.clone())?;
        std::fs::write(server.config_dir().join("devices.toml"), "")?;
        server
            .config
            .edit(&core, |_| Ok(()))
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let home = server.read("/api/home").await?;
        assert!(
            home["devices"].as_array().is_some_and(Vec::is_empty),
            "nothing joins on its own: {home}"
        );
        assert!(
            home["held"].as_array().is_some_and(|all| all
                .iter()
                .any(|item| item["id"] == "demo_living_room_lamp"
                    && item["provides"]["light"] == 1
                    && item["model"] == "Virtual colour lamp")),
            "{home}"
        );

        let (status, body) = server
            .json(
                "PATCH",
                "/api/devices/demo_living_room_lamp",
                serde_json::json!({"added": true}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        let home = server.read("/api/home").await?;
        assert!(
            home["entities"]
                .as_array()
                .is_some_and(|all| all.iter().any(|e| e["id"] == "light.living_room_lamp")),
            "{home}"
        );
        let devices = std::fs::read_to_string(server.config_dir().join("devices.toml"))?;
        assert!(
            devices.contains("[devices.demo_living_room_lamp]"),
            "{devices}"
        );

        let (status, _) = server
            .json(
                "PATCH",
                "/api/devices/demo_fridge_plug",
                serde_json::json!({"ignored": true}),
            )
            .await?;
        assert!(status.is_client_error(), "{status}");

        host.shutdown().await;
        Ok(())
    }

    /// The helpers extension on its own, started against a core with storage that outlives it,
    /// as the database does.
    async fn helpers(core: &Core) -> anyhow::Result<irori_core::ExtensionHost> {
        irori_core::ExtensionHost::start(
            core,
            vec![irori_protocol::builtin::<irori_helpers::Helpers>().map_err(anyhow::Error::msg)?],
            irori_core::Timing::default(),
        )
        .map_err(anyhow::Error::msg)
    }

    async fn until(what: &str, mut check: impl FnMut() -> bool) -> anyhow::Result<()> {
        for _ in 0..500 {
            if check() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        anyhow::bail!("timed out waiting for: {what}")
    }

    /// A toggle made from the page is a switch that remembers its value — through a new toggle
    /// being added, which restarts the extension, and through Irori starting again. Renaming it
    /// changes its one name, in the file that defines it, and removing it removes the entity.
    #[tokio::test]
    async fn a_toggle_keeps_its_value_and_has_one_name() -> anyhow::Result<()> {
        let storage: Arc<dyn irori_protocol::Storage> =
            Arc::new(irori_protocol::MemoryStorage::default());
        let core = core();
        core.use_storage(Arc::clone(&storage));
        let server = Server::new(core.clone())?;
        let host = helpers(&core).await?;

        let (status, made) = server
            .json(
                "POST",
                "/api/helpers/toggles",
                serde_json::json!({"name": "Guests are over"}),
            )
            .await?;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        assert_eq!(made["entity_id"], "switch.guests_are_over");
        let guests = EntityId::try_from("switch.guests_are_over")?;
        until("the toggle exists", || {
            core.state(&guests).is_some_and(|s| s.state.is_some())
        })
        .await?;

        let (status, body) = server
            .json(
                "POST",
                "/api/command",
                serde_json::json!({"entity_id": guests, "command": "turn_on"}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{body}");

        // Another toggle restarts the extension; the first keeps its value.
        server
            .json(
                "POST",
                "/api/helpers/toggles",
                serde_json::json!({"name": "Holiday"}),
            )
            .await?;
        let holiday = EntityId::try_from("switch.holiday")?;
        until("the second toggle exists", || {
            core.state(&holiday).is_some_and(|s| s.state.is_some())
        })
        .await?;
        let on = |core: &Core, id: &EntityId| {
            matches!(
                core.state(id).and_then(|s| s.state),
                Some(irori_types::State::Switch(irori_types::SwitchState {
                    on: true
                }))
            )
        };
        until("still on", || on(&core, &guests)).await?;

        // One name, kept where the toggle is defined.
        let (status, _) = server
            .json(
                "PATCH",
                "/api/entities/switch.guests_are_over",
                serde_json::json!({"name": "Visitors"}),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let file = std::fs::read_to_string(server.config_dir().join("extensions/helpers.toml"))?;
        assert!(file.contains("Visitors"), "{file}");
        assert!(
            !server.config_dir().join("entities.toml").exists(),
            "no second name anywhere"
        );
        until("renamed", || {
            core.entities()
                .iter()
                .any(|e| e.id == guests && e.name.as_str() == "Visitors")
        })
        .await?;
        host.shutdown().await;

        // Irori starting again, with the same storage: still on.
        let again = self::core();
        again.use_storage(storage);
        let _config = Config::open_dir(server.config_dir(), &again);
        let host = helpers(&again).await?;
        until("on after a restart", || on(&again, &guests)).await?;
        host.shutdown().await;

        let host = helpers(&core).await?;
        let (status, _) = server
            .json(
                "DELETE",
                "/api/helpers/toggles/holiday",
                serde_json::json!(null),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        until("the removed toggle's entity is gone", || {
            core.state(&holiday).is_none()
        })
        .await?;
        host.shutdown().await;
        Ok(())
    }

    /// Every kind of helper is made the same way and is an entity of a kind Irori already has:
    /// a number is set and counted within its range, a choice takes only its options, a button
    /// says each time it was pressed. What's wrong with one is said when it's made.
    #[tokio::test]
    async fn helpers_of_every_kind_are_made_set_changed_and_listed() -> anyhow::Result<()> {
        let storage: Arc<dyn irori_protocol::Storage> =
            Arc::new(irori_protocol::MemoryStorage::default());
        let core = core();
        core.use_storage(Arc::clone(&storage));
        let server = Server::new(core.clone())?;
        let host = helpers(&core).await?;
        let number_is = |core: &Core, id: &EntityId, value: f64| {
            core.state(id).and_then(|s| s.state)
                == Some(irori_types::State::Number(irori_types::NumberState {
                    value,
                }))
        };
        let command = |entity: &EntityId, command: &str, data: serde_json::Value| {
            let mut body = serde_json::json!({"entity_id": entity, "command": command});
            if !data.is_null() {
                body["data"] = data;
            }
            server.json("POST", "/api/command", body)
        };

        // A number: within its range, at its `initial` until it's set.
        let (status, made) = server
            .json(
                "POST",
                "/api/helpers/numbers",
                serde_json::json!({"name": "Evening brightness", "min": 0, "max": 100,
                    "step": 5, "unit": "%", "initial": 40}),
            )
            .await?;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        assert_eq!(made["entity_id"], "number.evening_brightness");
        let evening = EntityId::try_from("number.evening_brightness")?;
        until("the number exists", || number_is(&core, &evening, 40.0)).await?;
        let (status, body) =
            command(&evening, "set_value", serde_json::json!({"value": 95})).await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        until("set", || number_is(&core, &evening, 95.0)).await?;
        let (status, body) =
            command(&evening, "set_value", serde_json::json!({"value": 140})).await?;
        assert_ne!(status, StatusCode::OK, "{body}");
        // Counted by its step, and no further than its range.
        for _ in 0..2 {
            let (status, body) = command(&evening, "increment", serde_json::Value::Null).await?;
            assert_eq!(status, StatusCode::OK, "{body}");
        }
        until("counted up to the end", || {
            number_is(&core, &evening, 100.0)
        })
        .await?;

        // What's wrong with one is said, and nothing is made.
        let (status, body) = server
            .json(
                "POST",
                "/api/helpers/numbers",
                serde_json::json!({"name": "Backwards", "min": 10, "max": 1}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|e| e.contains("min 10 is above its max 1")),
            "{body}"
        );
        // Checked without being made: what it would be, or why not.
        let (status, checked) = server
            .json(
                "POST",
                "/api/helpers/thresholds/check",
                serde_json::json!({"name": "Bright", "source": "number.evening_brightness", "above": 50}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{checked}");
        assert_eq!(checked["entity_id"], "binary_sensor.bright");
        let (status, checked) = server
            .json(
                "POST",
                "/api/helpers/thresholds/check",
                serde_json::json!({"name": "Bright", "source": "number.evening_brightness"}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{checked}");
        // Previewed before it's made, without a name: played over its source's day.
        let (status, preview) = server
            .json(
                "POST",
                "/api/helpers/thresholds/preview",
                serde_json::json!({"source": "number.evening_brightness", "above": 50}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{preview}");
        assert_eq!(preview["entity"]["id"], "binary_sensor.preview");
        assert_eq!(preview["computed"], true);
        let said = preview["states"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            said.last().map(|s| s["state"]["on"].clone()),
            Some(serde_json::json!(true)),
            "{preview}"
        );
        // One a person sets starts as its `initial`.
        let (status, preview) = server
            .json(
                "POST",
                "/api/helpers/numbers/preview",
                serde_json::json!({"name": "Fan speed", "min": 0, "max": 10, "initial": 3}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{preview}");
        assert_eq!(preview["computed"], false);
        assert_eq!(preview["states"][0]["state"]["value"], 3.0, "{preview}");
        // A name already in use: the check says the id Create would give it, and why.
        let (status, checked) = server
            .json(
                "POST",
                "/api/helpers/numbers/check",
                serde_json::json!({"name": "Evening brightness", "min": 0, "max": 10}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{checked}");
        assert_eq!(checked["entity_id"], "number.evening_brightness_2");
        assert_eq!(
            checked["taken"],
            serde_json::json!(["number.evening_brightness"])
        );
        let (status, body) = server
            .json(
                "POST",
                "/api/helpers/gadgets",
                serde_json::json!({"name": "X"}),
            )
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|e| e.contains("toggles, numbers")),
            "{body}"
        );

        // A counter is a number too, so one with the same name gets an id of its own.
        let (status, made) = server
            .json(
                "POST",
                "/api/helpers/counters",
                serde_json::json!({"name": "Evening brightness"}),
            )
            .await?;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        assert_eq!(made["entity_id"], "number.evening_brightness_2");
        let count = EntityId::try_from("number.evening_brightness_2")?;
        until("the counter exists", || number_is(&core, &count, 0.0)).await?;
        // The first number kept what it was left at through the restart a new helper is.
        until("kept", || number_is(&core, &evening, 100.0)).await?;

        // A choice takes only what it lists.
        let (status, made) = server
            .json(
                "POST",
                "/api/helpers/selects",
                serde_json::json!({"name": "House mode", "options": ["home", "away"]}),
            )
            .await?;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        let mode = EntityId::try_from("select.house_mode")?;
        let mode_is = |core: &Core, option: &str| {
            core.state(&mode).and_then(|s| s.state)
                == Some(irori_types::State::Select(irori_types::SelectState {
                    option: option.to_owned(),
                }))
        };
        until("the choice exists", || mode_is(&core, "home")).await?;
        let (status, body) = command(
            &mode,
            "select_option",
            serde_json::json!({"option": "away"}),
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        until("chosen", || mode_is(&core, "away")).await?;
        let (status, _) = command(
            &mode,
            "select_option",
            serde_json::json!({"option": "asleep"}),
        )
        .await?;
        assert_ne!(status, StatusCode::OK);

        // A button is pressed, and its event says so each time.
        let (status, made) = server
            .json(
                "POST",
                "/api/helpers/buttons",
                serde_json::json!({"name": "Movie time"}),
            )
            .await?;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        assert_eq!(
            made["entities"],
            serde_json::json!(["button.movie_time", "event.movie_time"])
        );
        let button = EntityId::try_from("button.movie_time")?;
        let pressed = EntityId::try_from("event.movie_time")?;
        until("the button exists", || core.state(&pressed).is_some()).await?;
        assert_eq!(core.state(&pressed).and_then(|s| s.state), None);
        let (status, body) = command(&button, "press", serde_json::Value::Null).await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        until("pressed", || {
            core.state(&pressed).and_then(|s| s.state)
                == Some(irori_types::State::Event(irori_types::EventState {
                    event_type: "pressed".to_owned(),
                }))
        })
        .await?;
        let first = core.state(&pressed).map(|s| s.last_changed);
        tokio::time::sleep(Duration::from_millis(5)).await;
        command(&button, "press", serde_json::Value::Null).await?;
        until("pressed again", || {
            core.state(&pressed).map(|s| s.last_changed) != first
        })
        .await?;
        // Its event is named after it: there's one name to change.
        let (status, _) = server
            .json(
                "PATCH",
                "/api/entities/event.movie_time",
                serde_json::json!({"name": "Film"}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // A changed range: a value it can no longer have goes back to its `initial`.
        let (status, body) = server
            .json(
                "PATCH",
                "/api/helpers/numbers/evening_brightness",
                serde_json::json!({"max": 80}),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        until("back to its initial", || number_is(&core, &evening, 40.0)).await?;
        let (status, body) = server
            .json(
                "PATCH",
                "/api/helpers/numbers/evening_brightness",
                serde_json::json!({"max": 30}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|e| e.contains("`initial`")),
            "{body}"
        );

        // All of them, as they're written.
        let listed = server.read("/api/helpers").await?;
        let kinds: Vec<(&str, &str)> = listed
            .as_array()
            .expect("a list")
            .iter()
            .map(|h| {
                (
                    h["kind"].as_str().unwrap_or(""),
                    h["id"].as_str().unwrap_or(""),
                )
            })
            .collect();
        assert_eq!(
            kinds,
            [
                ("numbers", "evening_brightness"),
                ("counters", "evening_brightness_2"),
                ("selects", "house_mode"),
                ("buttons", "movie_time"),
            ]
        );
        assert_eq!(listed[0]["max"], 80);
        assert_eq!(listed[0]["entity_id"], "number.evening_brightness");

        let (status, _) = server
            .json(
                "DELETE",
                "/api/helpers/buttons/movie_time",
                serde_json::json!(null),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        until("the button and its event are gone", || {
            core.state(&button).is_none() && core.state(&pressed).is_none()
        })
        .await?;
        host.shutdown().await;
        Ok(())
    }

    /// A time helper starts at its `initial` and is set like any time; a date nobody has picked
    /// is unknown until someone does.
    #[tokio::test]
    async fn a_time_and_a_date_helper_are_set_like_a_devices() -> anyhow::Result<()> {
        let storage: Arc<dyn irori_protocol::Storage> =
            Arc::new(irori_protocol::MemoryStorage::default());
        let core = core();
        core.use_storage(Arc::clone(&storage));
        let server = Server::new(core.clone())?;
        let host = helpers(&core).await?;
        let text_of = |core: &Core, id: &EntityId| {
            core.state(id)
                .and_then(|s| s.state)
                .map(|state| state.primary().to_json())
        };

        let (status, made) = server
            .json(
                "POST",
                "/api/helpers/times",
                serde_json::json!({"name": "Wake up", "initial": "06:45"}),
            )
            .await?;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        let wake = EntityId::try_from("time.wake_up")?;
        until("the time exists", || {
            text_of(&core, &wake) == Some(serde_json::json!("06:45:00"))
        })
        .await?;
        let (status, body) = server
            .json(
                "POST",
                "/api/command",
                serde_json::json!({"entity_id": wake, "command": "set_value",
                    "data": {"time": "07:30"}}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        until("set", || {
            text_of(&core, &wake) == Some(serde_json::json!("07:30:00"))
        })
        .await?;

        let (status, body) = server
            .json(
                "POST",
                "/api/helpers/dates",
                serde_json::json!({"name": "Bins go out", "initial": "2026-02-30"}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|e| e.contains("a day that exists")),
            "{body}"
        );
        let (status, made) = server
            .json(
                "POST",
                "/api/helpers/dates",
                serde_json::json!({"name": "Bins go out"}),
            )
            .await?;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        let bins = EntityId::try_from("date.bins_go_out")?;
        until("the date exists", || core.state(&bins).is_some()).await?;
        assert_eq!(text_of(&core, &bins), None, "nobody has picked a day");
        // The time kept what it was set to through the restart a new helper is.
        until("kept", || {
            text_of(&core, &wake) == Some(serde_json::json!("07:30:00"))
        })
        .await?;
        let (status, body) = server
            .json(
                "POST",
                "/api/command",
                serde_json::json!({"entity_id": bins, "command": "set_value",
                    "data": {"date": "2026-10-20"}}),
            )
            .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        until("picked", || {
            text_of(&core, &bins) == Some(serde_json::json!("2026-10-20"))
        })
        .await?;
        host.shutdown().await;
        Ok(())
    }

    /// A timer helper counts down by itself and says when it's done, on its event; paused, it
    /// keeps what's left.
    #[tokio::test]
    async fn a_timer_helper_counts_down_and_says_it_finished() -> anyhow::Result<()> {
        let storage: Arc<dyn irori_protocol::Storage> =
            Arc::new(irori_protocol::MemoryStorage::default());
        let core = core();
        core.use_storage(Arc::clone(&storage));
        let server = Server::new(core.clone())?;
        let host = helpers(&core).await?;
        let (status, made) = server
            .json(
                "POST",
                "/api/helpers/timers",
                serde_json::json!({"name": "Egg", "duration": 1}),
            )
            .await?;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        assert_eq!(
            made["entities"],
            serde_json::json!(["timer.egg", "event.egg"])
        );
        let egg = EntityId::try_from("timer.egg")?;
        let ended = EntityId::try_from("event.egg")?;
        let status_of = |core: &Core| {
            core.state(&egg)
                .and_then(|s| s.state)
                .map(|state| state.primary().to_json())
        };
        let said = |core: &Core| {
            core.state(&ended)
                .and_then(|s| s.state)
                .map(|state| state.primary().to_json())
        };
        until("the timer exists", || {
            status_of(&core) == Some(serde_json::json!("idle"))
        })
        .await?;
        let call = |command: &str, data: serde_json::Value| {
            let mut body = serde_json::json!({"entity_id": egg, "command": command});
            if !data.is_null() {
                body["data"] = data;
            }
            server.json("POST", "/api/command", body)
        };

        // Paused, it keeps what's left; cancelled, it says so.
        let (status, body) = call("start", serde_json::json!({"duration": 60})).await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        until("started", || {
            said(&core) == Some(serde_json::json!("started"))
        })
        .await?;
        let (status, body) = call("pause", serde_json::Value::Null).await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        until("paused", || {
            status_of(&core) == Some(serde_json::json!("paused"))
        })
        .await?;
        let remaining = core
            .state(&egg)
            .and_then(|s| s.state)
            .and_then(|state| state.field("remaining"));
        assert!(
            matches!(remaining, Some(irori_types::Typed::Number(n)) if n > 55.0 && n <= 60.0),
            "{remaining:?}"
        );
        let (status, _) = call("pause", serde_json::Value::Null).await?;
        assert_ne!(status, StatusCode::OK, "a paused timer isn't paused again");
        call("cancel", serde_json::Value::Null).await?;
        until("cancelled", || {
            said(&core) == Some(serde_json::json!("cancelled"))
        })
        .await?;

        // Its own length, run out by itself.
        let (status, body) = call("start", serde_json::Value::Null).await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        until("running", || {
            status_of(&core) == Some(serde_json::json!("active"))
        })
        .await?;
        for _ in 0..300 {
            if said(&core) == Some(serde_json::json!("finished")) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(said(&core), Some(serde_json::json!("finished")));
        assert_eq!(status_of(&core), Some(serde_json::json!("idle")));
        host.shutdown().await;
        Ok(())
    }

    /// A schedule helper says whether it's on now and when that changes, on the home's clock.
    #[tokio::test]
    async fn a_schedule_helper_says_whether_it_is_on_and_until_when() -> anyhow::Result<()> {
        let storage: Arc<dyn irori_protocol::Storage> =
            Arc::new(irori_protocol::MemoryStorage::default());
        let core = core();
        core.use_storage(Arc::clone(&storage));
        let server = Server::new(core.clone())?;
        let host = helpers(&core).await?;
        let every_day = serde_json::json!([["00:00", "24:00"]]);
        let (status, made) = server
            .json(
                "POST",
                "/api/helpers/schedules",
                serde_json::json!({"name": "Always", "mon": every_day, "tue": every_day,
                    "wed": every_day, "thu": every_day, "fri": every_day, "sat": every_day,
                    "sun": every_day}),
            )
            .await?;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        let always = EntityId::try_from("schedule.always")?;
        until("on", || {
            core.state(&always).and_then(|s| s.state)
                == Some(irori_types::State::Schedule(irori_types::ScheduleState {
                    on: true,
                    next_change: None,
                }))
        })
        .await?;
        let (status, body) = server
            .json(
                "POST",
                "/api/helpers/schedules",
                serde_json::json!({"name": "Backwards", "mon": [["22:00", "02:00"]]}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|e| e.contains("ends before it starts")),
            "{body}"
        );
        host.shutdown().await;
        Ok(())
    }

    /// Helpers worked out from other entities follow them: a group of toggles is on while one
    /// is and turns them all on, a threshold follows a number, a meter counts what a counter
    /// went up by. One whose source isn't there says why.
    #[tokio::test]
    async fn helpers_worked_out_from_others_follow_them() -> anyhow::Result<()> {
        let storage: Arc<dyn irori_protocol::Storage> =
            Arc::new(irori_protocol::MemoryStorage::default());
        let core = core();
        core.use_storage(Arc::clone(&storage));
        let server = Server::new(core.clone())?;
        let host = helpers(&core).await?;

        let primary = |core: &Core, id: &str| {
            core.state(&EntityId::try_from(id).expect("an entity id"))
                .and_then(|s| s.state)
                .map(|state| state.primary().to_json())
        };
        let call = |entity: &str, command: &str, data: serde_json::Value| {
            let mut body = serde_json::json!({"entity_id": entity, "command": command});
            if !data.is_null() {
                body["data"] = data;
            }
            server.json("POST", "/api/command", body)
        };
        for (table, body) in [
            ("toggles", serde_json::json!({"name": "Lamp one"})),
            ("toggles", serde_json::json!({"name": "Lamp two"})),
            (
                "numbers",
                serde_json::json!({"name": "Level", "min": 0, "max": 100, "initial": 10}),
            ),
            ("counters", serde_json::json!({"name": "Rings"})),
            (
                "groups",
                serde_json::json!({"name": "Lamps", "kind": "switch",
                "members": ["switch.lamp_one", "switch.lamp_two"]}),
            ),
            (
                "thresholds",
                serde_json::json!({"name": "High", "source": "number.level", "above": 50}),
            ),
            (
                "meters",
                serde_json::json!({"name": "Rings today", "source": "number.rings", "every": "day"}),
            ),
            (
                "thresholds",
                serde_json::json!({"name": "Nowhere", "source": "sensor.missing", "above": 1}),
            ),
        ] {
            let path = format!("/api/helpers/{table}");
            let (status, made) = server.json("POST", &path, body).await?;
            assert_eq!(status, StatusCode::CREATED, "{made}");
        }
        until("the group is off", || {
            primary(&core, "switch.lamps") == Some(serde_json::json!(false))
        })
        .await?;
        until("the threshold is off", || {
            primary(&core, "binary_sensor.high") == Some(serde_json::json!(false))
        })
        .await?;

        // The group is on while any member is, and turning it on turns them all on.
        let (status, body) = call("switch.lamp_one", "turn_on", serde_json::Value::Null).await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        until("the group is on", || {
            primary(&core, "switch.lamps") == Some(serde_json::json!(true))
        })
        .await?;
        call("switch.lamp_one", "turn_off", serde_json::Value::Null).await?;
        until("off again", || {
            primary(&core, "switch.lamps") == Some(serde_json::json!(false))
        })
        .await?;
        let (status, body) = call("switch.lamps", "turn_on", serde_json::Value::Null).await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        until("both members on", || {
            primary(&core, "switch.lamp_one") == Some(serde_json::json!(true))
                && primary(&core, "switch.lamp_two") == Some(serde_json::json!(true))
        })
        .await?;

        // The threshold follows its number.
        call(
            "number.level",
            "set_value",
            serde_json::json!({"value": 80}),
        )
        .await?;
        until("high", || {
            primary(&core, "binary_sensor.high") == Some(serde_json::json!(true))
        })
        .await?;

        // The meter counts what the counter went up by.
        for _ in 0..3 {
            call("number.rings", "increment", serde_json::Value::Null).await?;
        }
        until("three rings", || {
            primary(&core, "sensor.rings_today") == Some(serde_json::json!(2.0))
                || primary(&core, "sensor.rings_today") == Some(serde_json::json!(3.0))
        })
        .await?;

        // A source that isn't there is said, not guessed around.
        let nowhere = core
            .state(&EntityId::try_from("binary_sensor.nowhere")?)
            .expect("described");
        assert_eq!(nowhere.state, None);
        assert!(
            nowhere
                .attributes
                .iter()
                .any(|(key, why)| key.as_str() == "problem"
                    && why
                        .as_str()
                        .is_some_and(|why| why.contains("sensor.missing"))),
            "{:?}",
            nowhere.attributes
        );

        // Helpers that read each other in a ring are refused.
        let (status, body) = server
            .json(
                "POST",
                "/api/helpers/thresholds",
                serde_json::json!({"name": "Loop", "source": "binary_sensor.loop", "above": 1}),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        host.shutdown().await;
        Ok(())
    }

    /// An icon arrives as an image with a policy that stops it doing anything but being one.
    /// A camera's picture comes from its protocol as it is now, never cached, and served so
    /// that opening it on its own can't run anything. Only a camera has one.
    #[tokio::test]
    async fn a_camera_sends_its_picture_and_nothing_else_has_one() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;
        let res = server
            .app()?
            .oneshot(
                Request::get("/api/entities/camera.garden_camera_1/picture").body(Body::empty())?,
            )
            .await?;
        assert_eq!(res.status(), StatusCode::OK);
        let headers = res.headers().clone();
        assert_eq!(headers["content-type"], "image/svg+xml");
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert!(
            headers["content-security-policy"]
                .to_str()?
                .contains("sandbox")
        );
        let body = String::from_utf8(res.into_body().collect().await?.to_bytes().to_vec())?;
        assert!(
            body.starts_with("<svg") && body.contains("Garden camera 1"),
            "{body}"
        );

        for (path, wanted) in [
            (
                "/api/entities/light.living_room_lamp/picture",
                StatusCode::NOT_FOUND,
            ),
            (
                "/api/entities/camera.nowhere/picture",
                StatusCode::NOT_FOUND,
            ),
        ] {
            let (status, body) = server.send(Request::get(path).body(Body::empty())?).await?;
            assert_eq!(status, wanted, "{path}: {}", String::from_utf8_lossy(&body));
        }
        host.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn an_extensions_icon_is_served_as_a_locked_down_image() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let server = Server::new(core.clone())?;
        let home = server.read("/api/home").await?;
        assert_eq!(home["extensions"]["demo"]["has_icon"], true);

        let res = server
            .app()?
            .oneshot(Request::get("/api/extensions/demo/icon.svg").body(Body::empty())?)
            .await?;
        assert_eq!(res.status(), StatusCode::OK);
        let headers = res.headers().clone();
        assert_eq!(headers["content-type"], "image/svg+xml");
        assert!(
            headers["content-security-policy"]
                .to_str()?
                .contains("default-src 'none'")
        );
        let body = res.into_body().collect().await?.to_bytes();
        assert!(
            String::from_utf8(body.to_vec())?
                .trim_start()
                .starts_with("<svg")
        );

        let (status, _) = server
            .send(Request::get("/api/extensions/nope/icon.svg").body(Body::empty())?)
            .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        host.shutdown().await;
        Ok(())
    }

    /// Irori's own log is served the way an extension's is: a 200 with `lines`, oldest first,
    /// and empty rather than an error when there is nothing to show.
    #[tokio::test]
    async fn the_system_log_is_whatever_this_process_has_said() -> anyhow::Result<()> {
        let server = Server::new(core())?;

        let empty = server.read("/api/system/log").await?;
        assert_eq!(
            empty["lines"],
            serde_json::json!([]),
            "nothing logged yet is an answer"
        );

        server.log.keep("2026-09-26T10:00:00Z  INFO irori is ready");
        server
            .log
            .keep("2026-09-26T10:00:01Z  WARN listening beyond this machine");

        let log = server.read("/api/system/log").await?;
        assert_eq!(
            log["lines"],
            serde_json::json!([
                "2026-09-26T10:00:00Z  INFO irori is ready",
                "2026-09-26T10:00:01Z  WARN listening beyond this machine",
            ])
        );
        Ok(())
    }

    /// A missing file says so, rather than quietly handing back the page.
    #[tokio::test]
    async fn a_missing_file_is_not_found() -> anyhow::Result<()> {
        let (status, _, _) = get("/does-not-exist.css").await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        Ok(())
    }

    /// An endpoint that doesn't exist says so, rather than answering with the page: a caller
    /// asking for JSON must not read a 200 as "that worked".
    #[tokio::test]
    async fn a_missing_endpoint_is_not_found() -> anyhow::Result<()> {
        for path in ["/api/not-real", "/api/nope", "/api"] {
            let (status, _, _) = get(path).await?;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        }
        Ok(())
    }

    /// The UI's own pages are its business: reloading on `/devices` has to reach the app, which
    /// then decides what to show. Holds whether or not the UI has been built into this binary.
    #[cfg(feature = "ui")]
    #[tokio::test]
    async fn the_apps_own_pages_reach_the_app() -> anyhow::Result<()> {
        let (status, content_type, body) = get("/devices").await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type.as_deref(), Some("text/html"));
        assert!(String::from_utf8(body)?.contains("IroriOS"));
        Ok(())
    }

    /// A cloud model is a URL and a key. The key is stored, never returned, and one view's
    /// conversation stays out of another's. A reply that failed, or a page that left, is not kept.
    #[cfg(feature = "assist")]
    mod assistant {
        use super::*;

        async fn cloud() -> String {
            let app = Router::new().route(
                "/v1/chat/completions",
                axum::routing::post(|Json(body): Json<serde_json::Value>| async move {
                    if body["model"] == "broken" {
                        return (StatusCode::BAD_GATEWAY, "the provider said no").into_response();
                    }
                    // A model that was never taught tools, and says so the way Ollama does.
                    if body["model"] == "no-tools" && !body["tools"].is_null() {
                        return (
                            StatusCode::BAD_REQUEST,
                            "{\"error\":\"no-tools does not support tools\"}",
                        )
                            .into_response();
                    }
                    // Says a word, then reaches for a tool; answers once the tool has reported.
                    if body["model"] == "looks-first" {
                        let looked = body["messages"]
                            .as_array()
                            .is_some_and(|messages| messages.iter().any(|m| m["role"] == "tool"));
                        let stream = if looked {
                            "data: {\"choices\":[{\"delta\":{\"content\":\"Nothing is on.\"}}]}\n\ndata: [DONE]\n\n"
                        } else {
                            "data: {\"choices\":[{\"delta\":{\"content\":\"Let me check.\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"function\":{\"name\":\"list_devices\",\"arguments\":\"{}\"}}]}}]}\n\ndata: [DONE]\n\n"
                        };
                        return (
                            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                            stream,
                        )
                            .into_response();
                    }
                    // Makes a room and asks for another to go, then says what came of it.
                    // `floods` asks for thirty rooms, and keeps asking.
                    if body["model"] == "arranges" || body["model"] == "floods" {
                        let report = body["messages"].as_array().and_then(|messages| {
                            messages
                                .iter()
                                .find(|m| m["role"] == "tool")
                                .and_then(|m| m["content"].as_str().map(str::to_owned))
                        });
                        let flooding = body["model"] == "floods";
                        let ops = if flooding {
                            serde_json::Value::from(vec![
                                serde_json::json!({"op": "add_area", "name": "Spare"});
                                15
                            ])
                        } else {
                            serde_json::json!([
                                {"op": "add_area", "name": "Pantry"},
                                {"op": "remove_area", "area": "kitchen"},
                                {"op": "remove_floor", "floor": "attic"},
                            ])
                        };
                        // Flooding, it asks twice before it says anything.
                        let asked = body["messages"].as_array().map_or(0, |messages| {
                            messages.iter().filter(|m| m["role"] == "tool").count()
                        });
                        let report = report.filter(|_| !flooding || asked >= 2);
                        let line = match report {
                            Some(_) if flooding => serde_json::json!({"choices": [{"delta": {
                                "content": body["messages"].to_string(),
                            }}]}),
                            Some(report) => serde_json::json!({"choices": [{"delta": {
                                "content": report,
                            }}]}),
                            None => serde_json::json!({"choices": [{"delta": {
                                "tool_calls": [{"index": 0, "id": "a", "function": {
                                    "name": "edit_home",
                                    "arguments": serde_json::json!({"ops": ops}).to_string(),
                                }}],
                            }}]}),
                        };
                        return (
                            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                            format!("data: {line}\n\ndata: [DONE]\n\n"),
                        )
                            .into_response();
                    }
                    // Says whether it was shown a picture, and how much of one.
                    if body["model"] == "looks" {
                        let shown = body["messages"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|m| m["content"][1]["image_url"]["url"].as_str())
                            .map(str::len)
                            .next();
                        let said = match shown {
                            Some(size) => format!("A picture of {size} characters."),
                            None => "No picture.".to_owned(),
                        };
                        let line = serde_json::json!({"choices": [{"delta": {"content": said}}]});
                        return (
                            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                            format!("data: {line}\n\ndata: [DONE]\n\n"),
                        )
                            .into_response();
                    }
                    // Suggests a flow, then says what the tool answered. The Automations
                    // engine is not running in this test, so the answer is a refusal.
                    if body["model"] == "proposes-flow" {
                        let report = body["messages"].as_array().and_then(|messages| {
                            messages
                                .iter()
                                .find(|message| message["role"] == "tool")
                                .and_then(|message| message["content"].as_str().map(str::to_owned))
                        });
                        let line = match report {
                            Some(report) => serde_json::json!({"choices": [{"delta": {
                                "content": format!("Told: {report}"),
                            }}]}),
                            None => serde_json::json!({"choices": [{"delta": {
                                "tool_calls": [{"index": 0, "id": "a", "function": {
                                    "name": "propose_flow",
                                    "arguments": serde_json::json!({
                                        "flow": {"id": "hallway", "name": "Hall"}
                                    }).to_string(),
                                }}],
                            }}]}),
                        };
                        return (
                            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                            format!("data: {line}\n\ndata: [DONE]\n\n"),
                        )
                            .into_response();
                    }
                    // Fills in the helper builder: a toggle, or with `proposes-badly` a threshold
                    // on a sensor the home doesn't have. Says what came back.
                    if body["model"] == "proposes" || body["model"] == "proposes-badly" {
                        let report = body["messages"].as_array().and_then(|messages| {
                            messages
                                .iter()
                                .find(|m| m["role"] == "tool")
                                .and_then(|m| m["content"].as_str().map(str::to_owned))
                        });
                        let arguments = if body["model"] == "proposes" {
                            serde_json::json!({"kind": "toggles", "name": "Guest mode",
                                "settings": {"initial": true}})
                        } else {
                            serde_json::json!({"kind": "thresholds", "name": "Too warm",
                                "settings": "{\"source\": \"sensor.nowhere\", \"above\": 26}"})
                        };
                        let line = match report {
                            Some(report) => serde_json::json!({"choices": [{"delta": {
                                "content": format!("Told: {report}"),
                            }}]}),
                            None => serde_json::json!({"choices": [{"delta": {
                                "tool_calls": [{"index": 0, "id": "a", "function": {
                                    "name": "propose_helper",
                                    "arguments": arguments.to_string(),
                                }}],
                            }}]}),
                        };
                        return (
                            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                            format!("data: {line}\n\ndata: [DONE]\n\n"),
                        )
                            .into_response();
                    }
                    // Draws a room when it is offered the tool to, and says it can't when it
                    // isn't. `draws-badly` asks for a room the home doesn't have.
                    if body["model"] == "draws" || body["model"] == "draws-badly" {
                        let offered = body["tools"].to_string().contains("edit_floorplan");
                        let report = body["messages"].as_array().and_then(|messages| {
                            messages
                                .iter()
                                .find(|m| m["role"] == "tool")
                                .map(|m| m["content"].to_string())
                        });
                        let room = if body["model"] == "draws" { "study" } else { "attic" };
                        let ops = serde_json::json!([
                            {"op": "add_wall", "from": [500, 0], "to": [800, 0]},
                            {"op": "add_wall", "from": [800, 0], "to": [800, 300]},
                            {"op": "add_opening", "wall": 2, "kind": "door", "at": 150},
                            {"op": "trace_area", "area": room,
                             "points": [[500, 0], [800, 0], [800, 300], [500, 300]]},
                        ])
                        .to_string();
                        let said = |text: &str| {
                            serde_json::json!({"choices": [{"delta": {"content": text}}]})
                        };
                        let line = match report {
                            Some(report) if report.contains("Done") => said("Drawn."),
                            Some(report) => said(&format!("Refused: {report}")),
                            None if offered => serde_json::json!({"choices": [{"delta": {
                                "tool_calls": [{"index": 0, "id": "a", "function": {
                                    "name": "edit_floorplan",
                                    "arguments": serde_json::json!({"ops": ops}).to_string(),
                                }}],
                            }}]}),
                            None => said("I can't draw here."),
                        };
                        return (
                            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                            format!("data: {line}\n\ndata: [DONE]\n\n"),
                        )
                            .into_response();
                    }
                    (
                        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                        "data: {\"choices\":[{\"delta\":{\"content\":\"Hello from the model\"}}]}\n\ndata: [DONE]\n\n",
                    )
                        .into_response()
                }),
            )
            // A provider takes a picture; the stand-in has to as well.
            .layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("a local port");
            let address = listener.local_addr().expect("the port it bound");
            tokio::spawn(async move {
                axum::serve(listener, app).await.ok();
            });
            format!("http://{address}/v1")
        }

        fn configure(base: &str, model: &str) -> serde_json::Value {
            serde_json::json!({
                "mode": "cloud",
                "preset": "compatible",
                "base_url": base,
                "model": model,
                "api_key": "sk-test-key-should-not-leak",
            })
        }

        async fn events(server: &Server, scope: &str, message: &str) -> anyhow::Result<String> {
            let (status, bytes) = server
                .send(
                    Request::post("/api/assistant/turns")
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&serde_json::json!({
                            "scope": scope,
                            "message": message,
                        }))?))?,
                )
                .await?;
            assert_eq!(status, StatusCode::OK);
            Ok(String::from_utf8(bytes)?)
        }

        #[tokio::test]
        async fn a_cloud_chat_stays_in_its_view_and_the_key_stays_out() -> anyhow::Result<()> {
            let base = cloud().await;
            let server = Server::new(core())?;
            let (status, body) = server
                .json("PUT", "/api/assistant", configure(&base, "test-model"))
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["ready"], true);
            assert_eq!(body["credential"], "set");
            assert!(
                !body.to_string().contains("sk-test-key-should-not-leak"),
                "the key came back on the status"
            );
            let file = std::fs::read_to_string(server.config_dir().join("assistant.toml"))?;
            assert!(!file.contains("sk-test-key-should-not-leak"), "{file}");
            assert!(file.contains("test-model"), "{file}");
            let secrets = std::fs::read_to_string(server.config_dir().join("secrets.toml"))?;
            assert!(secrets.contains("sk-test-key-should-not-leak"));

            let general = events(&server, "general", "What is on?").await?;
            assert!(general.contains("Hello from the model"), "{general}");
            let automation = events(&server, "automation:kettle", "Why this one?").await?;
            assert!(automation.contains("Hello from the model"), "{automation}");

            let home = server.read("/api/assistant/transcript/general").await?;
            let flow = server
                .read("/api/assistant/transcript/automation:kettle")
                .await?;
            assert_eq!(home["turns"][0]["body"], "What is on?");
            assert_eq!(home["turns"][1]["body"], "Hello from the model");
            assert!(home["pending"].is_null(), "{home}");
            assert_eq!(flow["turns"][0]["body"], "Why this one?");
            assert!(!flow.to_string().contains("What is on?"));

            let (status, _) = server
                .send(
                    Request::delete("/api/assistant/transcript/automation:kettle")
                        .body(Body::empty())?,
                )
                .await?;
            assert_eq!(status, StatusCode::NO_CONTENT);
            let flow = server
                .read("/api/assistant/transcript/automation:kettle")
                .await?;
            assert_eq!(flow["turns"], serde_json::json!([]));
            let home = server.read("/api/assistant/transcript/general").await?;
            assert_eq!(home["turns"][1]["body"], "Hello from the model");

            // Settings has a conversation of its own, and is told what Settings holds.
            let settings = events(&server, "settings", "Which floors are there?").await?;
            assert!(settings.contains("Hello from the model"), "{settings}");
            let kept = server.read("/api/assistant/transcript/settings").await?;
            assert_eq!(kept["turns"][0]["body"], "Which floors are there?");
            assert_eq!(home["turns"].as_array().map(Vec::len), Some(2));

            let (status, body) = server
                .json(
                    "PUT",
                    "/api/assistant",
                    serde_json::json!({ "model": "broken" }),
                )
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            let failed = events(&server, "general", "Again").await?;
            assert!(failed.contains("error"), "{failed}");
            let home = server.read("/api/assistant/transcript/general").await?;
            assert_eq!(home["turns"].as_array().map(Vec::len), Some(2));
            Ok(())
        }

        #[tokio::test]
        async fn a_tool_asked_for_after_a_few_words_is_still_run() -> anyhow::Result<()> {
            let base = cloud().await;
            let server = Server::new(core())?;
            let (status, body) = server
                .json("PUT", "/api/assistant", configure(&base, "looks-first"))
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            let reply = events(&server, "general", "What is on?").await?;
            assert!(reply.contains("Let me check."), "{reply}");
            assert!(reply.contains("Nothing is on."), "{reply}");
            let home = server.read("/api/assistant/transcript/general").await?;
            assert_eq!(home["turns"][1]["body"], "Let me check.\n\nNothing is on.");
            Ok(())
        }

        #[tokio::test]
        async fn a_model_without_tools_is_asked_again_without_them() -> anyhow::Result<()> {
            let base = cloud().await;
            let server = Server::new(core())?;
            let (status, body) = server
                .json("PUT", "/api/assistant", configure(&base, "no-tools"))
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            let reply = events(&server, "general", "What is on?").await?;
            assert!(reply.contains("Hello from the model"), "{reply}");
            assert!(!reply.contains("error"), "{reply}");
            Ok(())
        }

        /// One question from the Floorplan page, with the plan being edited there or without.
        async fn asked_on_the_plan(
            server: &Server,
            plan: Option<&serde_json::Value>,
        ) -> anyhow::Result<Vec<serde_json::Value>> {
            let mut body = serde_json::json!({
                "scope": "floorplan:ground",
                "message": "Draw the study to the right of the kitchen.",
            });
            if let Some(plan) = plan {
                body["plan"] = plan.clone();
            }
            let (status, bytes) = server
                .send(
                    Request::post("/api/assistant/turns")
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&body)?))?,
                )
                .await?;
            assert_eq!(status, StatusCode::OK);
            Ok(String::from_utf8(bytes)?
                .lines()
                .filter_map(|line| serde_json::from_str(line.strip_prefix("data:")?.trim()).ok())
                .collect())
        }

        /// In the helper builder the assistant fills the form in. Nothing is made until the
        /// person presses Create.
        #[tokio::test]
        async fn the_assistant_fills_in_the_helper_builder_and_makes_nothing() -> anyhow::Result<()>
        {
            let base = cloud().await;
            let server = Server::new(core())?;
            let (status, body) = server
                .json("PUT", "/api/assistant", configure(&base, "proposes"))
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            let lines = |text: String| -> Vec<serde_json::Value> {
                text.lines()
                    .filter_map(|line| {
                        serde_json::from_str(line.strip_prefix("data:")?.trim()).ok()
                    })
                    .collect()
            };
            let said = lines(events(&server, "helpers", "A guest mode switch, on").await?);
            let helper = said
                .iter()
                .find_map(|event| event.get("helper"))
                .unwrap_or_else(|| panic!("nothing filled in: {said:?}"));
            assert_eq!(helper["kind"], "toggles");
            assert_eq!(helper["name"], "Guest mode");
            assert_eq!(helper["settings"]["initial"], true);
            assert_eq!(helper["entities"][0], "switch.guest_mode");
            assert!(
                said.iter().any(|event| event["delta"]
                    .as_str()
                    .is_some_and(|text| text.contains("Create"))),
                "{said:?}"
            );
            let file = std::fs::read_to_string(
                server.config_dir().join("extensions").join("helpers.toml"),
            )
            .unwrap_or_default();
            assert!(!file.contains("guest_mode"), "{file}");

            // A source the home doesn't have is said to the model, and nothing is filled in.
            let (status, body) = server
                .json("PUT", "/api/assistant", configure(&base, "proposes-badly"))
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            let said = lines(events(&server, "helpers", "Tell me when it's too warm").await?);
            assert!(
                said.iter().all(|event| event.get("helper").is_none()),
                "{said:?}"
            );
            let text = serde_json::Value::from(said).to_string();
            assert!(text.contains("sensor.nowhere"), "{text}");

            // Elsewhere there is no builder to fill in.
            let said = lines(events(&server, "general", "A guest mode switch").await?);
            assert!(
                said.iter().all(|event| event.get("helper").is_none()),
                "{said:?}"
            );
            Ok(())
        }

        /// Suggesting a flow is only offered about the automation being asked about, and
        /// nothing is written when Automations isn't there to check it.
        #[tokio::test]
        async fn a_flow_suggestion_is_refused_when_automations_is_not_running() -> anyhow::Result<()>
        {
            let base = cloud().await;
            let server = Server::new(core())?;
            let (status, body) = server
                .json("PUT", "/api/assistant", configure(&base, "proposes-flow"))
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            let lines = |text: String| -> Vec<serde_json::Value> {
                text.lines()
                    .filter_map(|line| {
                        serde_json::from_str(line.strip_prefix("data:")?.trim()).ok()
                    })
                    .collect()
            };
            let said = lines(events(&server, "automation:hallway", "Dim the light").await?);
            assert!(
                said.iter().all(|event| event.get("flow").is_none()),
                "nothing to draw: {said:?}"
            );
            let text = serde_json::Value::from(said).to_string();
            assert!(
                text.contains("isn't installed") && text.contains("Nothing was changed"),
                "{text}"
            );

            // A new automation, not saved yet, is asked about the same way.
            let said = lines(events(&server, "new-automation", "Dim the light").await?);
            assert!(
                said.iter().all(|event| event.get("flow").is_none()),
                "nothing to draw: {said:?}"
            );
            let text = serde_json::Value::from(said).to_string();
            assert!(
                text.contains("isn't installed") && text.contains("Nothing was changed"),
                "{text}"
            );

            // The general chat is not offered the tool, so the call is turned away.
            let said = lines(events(&server, "general", "Dim the hallway light").await?);
            assert!(
                said.iter().all(|event| event.get("flow").is_none()),
                "{said:?}"
            );
            let text = serde_json::Value::from(said).to_string();
            assert!(text.contains("there is no tool"), "{text}");
            Ok(())
        }

        /// On the Floorplan page the assistant may draw — on the copy the person is editing,
        /// which comes back to them to keep or undo, and never on the home's own plan.
        #[tokio::test]
        async fn the_assistant_draws_on_the_plan_being_edited_and_saves_nothing()
        -> anyhow::Result<()> {
            let base = cloud().await;
            let server = Server::new(core())?;
            for (path, body) in [
                (
                    "/api/floors",
                    serde_json::json!({"name": "Ground", "level": 0}),
                ),
                ("/api/areas", serde_json::json!({"name": "Kitchen"})),
                ("/api/areas", serde_json::json!({"name": "Study"})),
            ] {
                let (status, body) = server.json("POST", path, body).await?;
                assert!(status.is_success(), "{path}: {body}");
            }
            let draft = serde_json::json!({"floors": {"ground": {
                "walls": [{"from": [0, 0], "to": [400, 0]}],
                "areas": [{"area": "kitchen", "points": [[0, 0], [400, 0], [400, 300], [0, 300]]}],
            }}});
            let (status, body) = server
                .json("PUT", "/api/assistant", configure(&base, "draws"))
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");

            let events = asked_on_the_plan(&server, Some(&draft)).await?;
            let drawn = events
                .iter()
                .find_map(|event| event.get("plan"))
                .unwrap_or_else(|| panic!("no plan came back: {events:?}"));
            let ground = &drawn["floors"]["ground"];
            assert_eq!(ground["walls"].as_array().map(Vec::len), Some(3), "{drawn}");
            assert_eq!(ground["walls"][1]["openings"][0]["kind"], "door");
            assert_eq!(ground["areas"][1]["area"], "study");
            assert!(events.iter().any(|event| event["step"] == "edit_floorplan"));
            assert!(
                events.iter().any(|event| event["delta"] == "Drawn."),
                "{events:?}"
            );
            // The home's own plan was never drawn at all, and still isn't.
            assert_eq!(server.read("/api/floorplan").await?, serde_json::json!({}));
            let file = std::fs::read_to_string(server.config_dir().join("floorplan.toml"))
                .unwrap_or_default();
            assert!(!file.contains("[[floors"), "{file}");

            // Without the plan being edited there is nothing to draw on, and no tool to try.
            let events = asked_on_the_plan(&server, None).await?;
            assert!(
                events.iter().all(|event| event.get("plan").is_none()),
                "{events:?}"
            );
            assert!(
                events
                    .iter()
                    .any(|event| event["delta"] == "I can't draw here.")
            );

            // A room the home doesn't have is refused, in words the model can act on, and the
            // page is sent nothing to take.
            let (status, body) = server
                .json("PUT", "/api/assistant", configure(&base, "draws-badly"))
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            let events = asked_on_the_plan(&server, Some(&draft)).await?;
            assert!(
                events.iter().all(|event| event.get("plan").is_none()),
                "{events:?}"
            );
            let said = serde_json::Value::from(events).to_string();
            assert!(
                said.contains("no room `attic`") && said.contains("study"),
                "{said}"
            );

            // A plan that couldn't be saved as it stands is not drawn on, and the model is told
            // why rather than left to say the plan isn't being edited.
            let (status, body) = server
                .json("PUT", "/api/assistant", configure(&base, "draws"))
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            let broken = serde_json::json!({"floors": {"ground": {
                "walls": [{"from": [0, 0], "to": [0, 0]}],
            }}});
            let events = asked_on_the_plan(&server, Some(&broken)).await?;
            assert!(
                events.iter().all(|event| event.get("plan").is_none()),
                "{events:?}"
            );
            assert!(
                events
                    .iter()
                    .any(|event| event["delta"] == "I can't draw here.")
            );

            // The same tool, asked for from a conversation that isn't offered it.
            let reply = events_of(&server, "general", &draft).await?;
            assert!(!reply.contains("\"plan\""), "{reply}");
            Ok(())
        }

        /// Saving a cloud model doesn't say whether it works. Asking it does.
        #[tokio::test]
        async fn a_cloud_model_can_be_asked_whether_it_answers() -> anyhow::Result<()> {
            let base = cloud().await;
            let server = Server::new(core())?;
            let (status, body) = server
                .json("POST", "/api/assistant/check", serde_json::json!({}))
                .await?;
            assert_eq!(status, StatusCode::BAD_REQUEST, "nothing is set up: {body}");

            server
                .json("PUT", "/api/assistant", configure(&base, "test-model"))
                .await?;
            let (status, body) = server
                .json("POST", "/api/assistant/check", serde_json::json!({}))
                .await?;
            assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
            // Nothing of the asking is kept as a conversation.
            let kept = server.read("/api/assistant/transcript/general").await?;
            assert_eq!(kept["turns"], serde_json::json!([]));

            server
                .json("PUT", "/api/assistant", configure(&base, "broken"))
                .await?;
            let (status, body) = server
                .json("POST", "/api/assistant/check", serde_json::json!({}))
                .await?;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let why = body["error"].as_str().unwrap_or_default();
            assert!(why.contains("the provider said no"), "{body}");
            assert!(
                !body.to_string().contains("sk-test-key-should-not-leak"),
                "{body}"
            );
            Ok(())
        }

        /// A picture of a floorplan goes to the model with the question it was asked with —
        /// one far bigger than any other request may be — and is not what is remembered.
        #[tokio::test]
        async fn a_picture_given_on_the_floorplan_reaches_the_model() -> anyhow::Result<()> {
            let base = cloud().await;
            let server = Server::new(core())?;
            let (status, body) = server
                .json(
                    "POST",
                    "/api/floors",
                    serde_json::json!({"name": "Ground", "level": 0}),
                )
                .await?;
            assert!(status.is_success(), "{body}");
            let (status, body) = server
                .json("PUT", "/api/assistant", configure(&base, "looks"))
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            let ask = |scope: &str, media_type: &str, data: String| {
                let body = serde_json::json!({
                    "scope": scope,
                    "message": "Draw this.",
                    "attachment": {"name": "plan.png", "media_type": media_type, "data": data},
                });
                let server = &server;
                async move {
                    let (status, bytes) = server
                        .send(
                            Request::post("/api/assistant/turns")
                                .header("content-type", "application/json")
                                .body(Body::from(serde_json::to_vec(&body)?))?,
                        )
                        .await?;
                    anyhow::Ok((status, String::from_utf8(bytes)?))
                }
            };
            // Three megabytes: more than a request is otherwise allowed to be.
            let picture = "QUJD".repeat(750_000);
            let (status, reply) = ask("floorplan:ground", "image/png", picture.clone()).await?;
            assert_eq!(status, StatusCode::OK, "{reply}");
            let expected = picture.len() + "data:image/png;base64,".len();
            assert!(
                reply.contains(&format!("A picture of {expected} characters.")),
                "{reply}"
            );
            let kept = server
                .read("/api/assistant/transcript/floorplan:ground")
                .await?;
            assert_eq!(
                kept["turns"][0]["body"],
                "Draw this.\n\n(attached: plan.png)"
            );
            assert!(kept.to_string().len() < 2_000, "the picture was kept");

            // Not something that isn't a picture, and not in a conversation that can't draw.
            let (_, reply) = ask("floorplan:ground", "text/html", "QUJD".into()).await?;
            assert!(reply.contains("isn't a picture or a PDF"), "{reply}");
            let (_, reply) = ask("general", "image/png", "QUJD".into()).await?;
            assert!(reply.contains("on the Floorplan page"), "{reply}");
            Ok(())
        }

        /// From the Floorplan's chat the assistant may make a room, which is saved as a room
        /// made in Settings is. It may not remove one: the person is asked, and until they
        /// answer the room is still there.
        #[tokio::test]
        async fn the_assistant_adds_a_room_and_only_asks_to_remove_one() -> anyhow::Result<()> {
            let base = cloud().await;
            let server = Server::new(core())?;
            for (path, body) in [
                (
                    "/api/floors",
                    serde_json::json!({"name": "Ground", "level": 0}),
                ),
                ("/api/areas", serde_json::json!({"name": "Kitchen"})),
            ] {
                let (status, body) = server.json("POST", path, body).await?;
                assert!(status.is_success(), "{path}: {body}");
            }
            let (status, body) = server
                .json("PUT", "/api/assistant", configure(&base, "arranges"))
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            let draft = serde_json::json!({});
            let events = asked_on_the_plan(&server, Some(&draft)).await?;
            let said = serde_json::Value::from(events.clone()).to_string();

            let rooms = server.read("/api/areas").await?;
            let pantry = rooms
                .as_array()
                .into_iter()
                .flatten()
                .find(|room| room["id"] == "pantry")
                .unwrap_or_else(|| panic!("no pantry was made: {rooms} / {said}"));
            assert_eq!(pantry["floor_id"], "ground", "on the floor being looked at");
            assert!(
                rooms.to_string().contains("\"kitchen\""),
                "the kitchen was removed without anybody saying yes: {rooms}"
            );
            let asked = events
                .iter()
                .find_map(|event| event.get("confirm"))
                .unwrap_or_else(|| panic!("nobody was asked: {said}"));
            assert_eq!(
                asked,
                &serde_json::json!([{"kind": "area", "id": "kitchen", "name": "Kitchen"}])
            );
            assert!(said.contains("There's no floor `attic`"), "{said}");
            assert!(said.contains("saved already"), "{said}");

            // One question gets only so many rooms added, however many it asks for.
            let (status, body) = server
                .json("PUT", "/api/assistant", configure(&base, "floods"))
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            let events = asked_on_the_plan(&server, Some(&draft)).await?;
            let said = serde_json::Value::from(events).to_string();
            assert!(said.contains("No more floors or rooms"), "{said}");
            let rooms = server.read("/api/areas").await?;
            let spares = rooms
                .as_array()
                .into_iter()
                .flatten()
                .filter(|room| room["name"] == "Spare")
                .count();
            assert_eq!(spares, 12, "{rooms}");
            Ok(())
        }

        /// A question with a plan attached, from a conversation that has no use for one.
        async fn events_of(
            server: &Server,
            scope: &str,
            plan: &serde_json::Value,
        ) -> anyhow::Result<String> {
            let (_, bytes) = server
                .send(
                    Request::post("/api/assistant/turns")
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&serde_json::json!({
                            "scope": scope,
                            "message": "Draw the study.",
                            "plan": plan,
                        }))?))?,
                )
                .await?;
            Ok(String::from_utf8(bytes)?)
        }

        /// Irori finishes an answer once it has been asked for, with nobody there to read it.
        #[tokio::test]
        async fn a_page_that_leaves_still_gets_its_answer_kept() -> anyhow::Result<()> {
            let base = cloud().await;
            let server = Server::new(core())?;
            let (status, body) = server
                .json("PUT", "/api/assistant", configure(&base, "test-model"))
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            let db = crate::db::open(server.dir.path())?;
            let turns = crate::assistant::Turns::default();
            let pending = turns
                .begin("general", "hello")
                .map_err(anyhow::Error::msg)?;
            // Nothing follows it: the page that asked has gone.
            turns
                .run("general", pending, |tx| {
                    crate::assistant::take_turn(
                        crate::assistant::Turn {
                            core: &server.core,
                            config: &server.config,
                            history: &server.history,
                            db: &db.path,
                            log: &server.log,
                            owner: true,
                            user: None,
                            reach: irori_core::Reach::Whole,
                        },
                        "general".into(),
                        "hello".into(),
                        None,
                        None,
                        tx,
                    )
                })
                .await;
            let (kept, used) =
                crate::assistant::transcript(&db.path, "general").map_err(anyhow::Error::msg)?;
            assert_eq!(kept.len(), 2, "{kept:?}");
            assert_eq!(kept[1].body, "Hello from the model");
            // How much of the model's context it took is kept with it. This provider counts
            // nothing, so it is what the words come to; a cloud model's size isn't Irori's
            // to know.
            let used = used.expect("the answer's usage was kept");
            assert!(used.used > 100, "{used:?}");
            assert_eq!(used.size, None);
            assert!(turns.pending("general").is_none());
            Ok(())
        }

        /// The Settings conversation's model can read the log, and only what it asks for.
        #[tokio::test]
        async fn the_settings_chat_is_told_the_trouble_in_the_log() -> anyhow::Result<()> {
            let seen = Arc::new(Mutex::new(String::new()));
            let heard = seen.clone();
            let app = Router::new().route(
                "/v1/chat/completions",
                axum::routing::post(move |Json(body): Json<serde_json::Value>| {
                    let heard = heard.clone();
                    async move {
                        *heard.lock().expect("the lock") = body.to_string();
                        (
                            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                            "data: {\"choices\":[{\"delta\":{\"content\":\"Read.\"}}]}\n\ndata: [DONE]\n\n",
                        )
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let base = format!("http://{}/v1", listener.local_addr()?);
            tokio::spawn(async move {
                axum::serve(listener, app).await.ok();
            });
            let server = Server::new(core())?;
            server.log.keep("2026-01-01T00:00:00Z  INFO irori is ready");
            server
                .log
                .keep("2026-01-01T00:00:01Z  WARN the port was taken");
            let (status, body) = server
                .json("PUT", "/api/assistant", configure(&base, "test-model"))
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            events(&server, "settings", "Anything wrong?").await?;
            let asked = seen.lock().expect("the lock").clone();
            assert!(asked.contains("the port was taken"), "{asked}");
            assert!(!asked.contains("irori is ready"), "{asked}");
            assert!(asked.contains("read_logs"), "{asked}");
            assert!(asked.contains("Floors and areas"), "{asked}");
            assert!(!asked.contains("sk-test-key-should-not-leak"), "{asked}");
            // A device's chat is offered no way into the log.
            events(&server, "device:nothing", "Anything wrong?").await?;
            events(&server, "automation:kettle", "Anything wrong?").await?;
            let asked = seen.lock().expect("the lock").clone();
            assert!(!asked.contains("read_logs"), "{asked}");
            assert!(!asked.contains("the port was taken"), "{asked}");

            // The chat about the whole home is told what Settings holds and what automations
            // there are, and is offered the tools that read more of each.
            events(&server, "general", "Anything wrong?").await?;
            let asked = seen.lock().expect("the lock").clone();
            assert!(asked.contains("This is the person's home"), "{asked}");
            assert!(asked.contains("the port was taken"), "{asked}");
            assert!(asked.contains("Floors and areas"), "{asked}");
            assert!(asked.contains("Automations, each with its id"), "{asked}");
            // No Automations extension runs here, and the model is told so, not left guessing.
            assert!(
                asked.contains("isn't installed, or isn't running"),
                "{asked}"
            );
            for tool in [
                "read_logs",
                "read_settings",
                "list_automations",
                "get_automation",
            ] {
                assert!(asked.contains(tool), "{tool}: {asked}");
            }
            assert!(!asked.contains("sk-test-key-should-not-leak"), "{asked}");

            // What the person wants kept to goes in front of every conversation, and can be
            // saved on its own, without choosing a model again.
            let (status, body) = server
                .json(
                    "PUT",
                    "/api/assistant",
                    serde_json::json!({ "instructions": "  Always answer in Spanish. " }),
                )
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["instructions"], "Always answer in Spanish.");
            assert_eq!(body["ready"], true, "{body}");
            for scope in ["general", "settings", "automation:kettle"] {
                events(&server, scope, "Anything wrong?").await?;
                let asked = seen.lock().expect("the lock").clone();
                assert!(
                    asked.contains("Always answer in Spanish."),
                    "{scope}: {asked}"
                );
            }
            let file = std::fs::read_to_string(server.config_dir().join("assistant.toml"))?;
            assert!(file.contains("Always answer in Spanish."), "{file}");
            let (status, body) = server
                .json(
                    "PUT",
                    "/api/assistant",
                    serde_json::json!({ "instructions": "" }),
                )
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            events(&server, "general", "Anything wrong?").await?;
            let asked = seen.lock().expect("the lock").clone();
            assert!(!asked.contains("Always answer in Spanish."), "{asked}");
            assert!(
                asked.contains("It has no standing instructions."),
                "{asked}"
            );
            Ok(())
        }

        #[tokio::test]
        async fn a_chat_says_how_much_of_the_context_it_took() -> anyhow::Result<()> {
            let base = cloud().await;
            let server = Server::new(core())?;
            let (status, body) = server
                .json("PUT", "/api/assistant", configure(&base, "test-model"))
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            let before = server.read("/api/assistant/transcript/general").await?;
            assert!(before["context"].is_null(), "{before}");
            events(&server, "general", "What is on?").await?;
            let after = server.read("/api/assistant/transcript/general").await?;
            assert!(
                after["context"]["used"].as_u64().is_some_and(|n| n > 100),
                "{after}"
            );
            assert!(after["context"]["size"].is_null(), "{after}");
            // Another conversation has taken nothing yet, and clearing this one forgets it.
            let other = server.read("/api/assistant/transcript/settings").await?;
            assert!(other["context"].is_null(), "{other}");
            let (status, _) = server
                .send(Request::delete("/api/assistant/transcript/general").body(Body::empty())?)
                .await?;
            assert_eq!(status, StatusCode::NO_CONTENT);
            let cleared = server.read("/api/assistant/transcript/general").await?;
            assert!(cleared["context"].is_null(), "{cleared}");
            Ok(())
        }

        #[tokio::test]
        async fn a_local_models_context_is_set_within_bounds() -> anyhow::Result<()> {
            let server = Server::new(core())?;
            let status = server.read("/api/assistant").await?;
            assert_eq!(status["local_context"], 4096);
            assert_eq!(status["context"], 4096);
            let (code, body) = server
                .json(
                    "PUT",
                    "/api/assistant",
                    serde_json::json!({ "context": 8192 }),
                )
                .await?;
            assert_eq!(code, StatusCode::OK, "{body}");
            assert_eq!(body["local_context"], 8192);
            // Setting it chooses nothing: the assistant is as off as it was.
            assert_eq!(body["mode"], "off");
            let file = std::fs::read_to_string(server.config_dir().join("assistant.toml"))?;
            assert!(file.contains("context = 8192"), "{file}");
            for wrong in [serde_json::json!(100), serde_json::json!("lots")] {
                let (code, body) = server
                    .json(
                        "PUT",
                        "/api/assistant",
                        serde_json::json!({ "context": wrong }),
                    )
                    .await?;
                assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
            }
            let status = server.read("/api/assistant").await?;
            assert_eq!(status["local_context"], 8192);
            Ok(())
        }

        #[tokio::test]
        async fn a_local_model_is_not_ready_without_ollama() -> anyhow::Result<()> {
            let server = Server::new(core())?;
            let (status, body) = server
                .json(
                    "PUT",
                    "/api/assistant",
                    serde_json::json!({ "mode": "local", "local_tag": "qwen3:1.7b" }),
                )
                .await?;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["ready"], false);
            assert_eq!(body["ollama"], "down");
            assert!(
                body["detail"].as_str().unwrap_or("").contains("Ollama"),
                "{body}"
            );
            let (status, bytes) = server
                .send(
                    Request::post("/api/assistant/pull")
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&serde_json::json!({
                            "tag": "qwen3:1.7b",
                        }))?))?,
                )
                .await?;
            assert_eq!(status, StatusCode::OK);
            let text = String::from_utf8(bytes)?;
            assert!(text.contains("error"), "{text}");
            Ok(())
        }
    }

    // ---- who is there (`server/auth.rs`, `server/people.rs`) --------------------------------

    /// One request, as a browser with (or without) a session cookie would send it. `ui` is
    /// whether it carries the header only Irori's own page sends.
    async fn ask(
        app: &Router,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
        cookie: Option<&str>,
        ui: bool,
    ) -> anyhow::Result<(StatusCode, serde_json::Value, Option<String>)> {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        if ui {
            request = request.header(UI_HEADER, "1");
        }
        let request = match body {
            Some(body) => request
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body)?))?,
            None => request.body(Body::empty())?,
        };
        let response = app.clone().oneshot(request).await?;
        let status = response.status();
        // Only the name and the value: what a browser sends back.
        let cookie = response
            .headers()
            .get("set-cookie")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::to_owned);
        let bytes = response.into_body().collect().await?.to_bytes();
        Ok((
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
            cookie,
        ))
    }

    /// A home with an owner who has a password, and the owner's cookie.
    async fn locked_home(server: &Server) -> anyhow::Result<(Router, String)> {
        let app = server.app()?;
        let (status, _, cookie) = ask(
            &app,
            "POST",
            "/api/setup",
            Some(serde_json::json!({ "name": "Nico", "password": "correct horse" })),
            None,
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED);
        Ok((app, cookie.expect("setting a password signs its owner in")))
    }

    #[tokio::test]
    async fn a_home_nobody_has_set_up_is_open_and_says_so() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let app = server.app()?;
        let (status, session, _) = ask(&app, "GET", "/api/session", None, None, false).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(session["locked"], false);
        assert_eq!(session["owner"], true);
        assert_eq!(session["user"], serde_json::Value::Null);
        assert_eq!(
            session["setup"],
            serde_json::json!({ "owner": false, "place": false })
        );
        // And everything works as it always did, with no header and no cookie.
        let (status, _, _) = ask(&app, "GET", "/api/home", None, None, false).await?;
        assert_eq!(status, StatusCode::OK);
        let (status, _, _) = ask(
            &app,
            "POST",
            "/api/areas",
            Some(serde_json::json!({ "name": "Hall" })),
            None,
            false,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED);
        Ok(())
    }

    #[tokio::test]
    async fn the_owner_is_set_up_with_a_password_and_nothing_less() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let app = server.app()?;
        for (body, said) in [
            (
                serde_json::json!({ "name": "Nico", "password": "" }),
                "at least 8",
            ),
            (
                serde_json::json!({ "name": "Nico", "password": "short" }),
                "at least 8",
            ),
        ] {
            let (status, why, cookie) =
                ask(&app, "POST", "/api/setup", Some(body), None, false).await?;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{why}");
            assert!(
                why["error"].as_str().is_some_and(|e| e.contains(said)),
                "{why}"
            );
            assert_eq!(cookie, None);
        }
        // Left out altogether, it isn't a setup at all.
        let (status, _, _) = ask(
            &app,
            "POST",
            "/api/setup",
            Some(serde_json::json!({ "name": "Nico" })),
            None,
            false,
        )
        .await?;
        assert!(status.is_client_error());
        assert!(
            !server.config_dir().join("users.toml").exists(),
            "nobody was made"
        );

        let (status, session, cookie) = ask(
            &app,
            "POST",
            "/api/setup",
            Some(serde_json::json!({ "name": "Nico", "password": "correct horse" })),
            None,
            false,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{session}");
        assert!(cookie.is_some(), "whoever set the home up is signed in");
        assert_eq!(session["locked"], true);
        assert_eq!(session["setup"]["owner"], true);
        assert_eq!(session["user"]["id"], "nico");
        assert_eq!(session["user"]["role"], "owner");

        // And nobody else can be set up from the door.
        let (status, why, _) = ask(
            &app,
            "POST",
            "/api/setup",
            Some(serde_json::json!({ "name": "Mallory", "password": "let me in!" })),
            None,
            false,
        )
        .await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            why["error"]
                .as_str()
                .is_some_and(|e| e.contains("already has an owner"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn an_owner_from_before_passwords_were_needed_is_the_one_set_up() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        std::fs::write(
            server.config_dir().join("users.toml"),
            "[users.nico]\nname = \"Nico\"\nrole = \"owner\"\n",
        )?;
        server.config.home().await;
        let app = server.app()?;
        // Still open, and still to be set up: the welcome is what asks for the password.
        let (_, session, _) = ask(&app, "GET", "/api/session", None, None, false).await?;
        assert_eq!(session["locked"], false);
        assert_eq!(session["setup"]["owner"], false);

        let (status, session, cookie) = ask(
            &app,
            "POST",
            "/api/setup",
            Some(serde_json::json!({ "name": "Nicolas", "password": "correct horse" })),
            None,
            false,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{session}");
        assert!(cookie.is_some());
        // The same person, by the id they always had, under the name they just gave.
        assert_eq!(session["user"]["id"], "nico");
        assert_eq!(session["user"]["name"], "Nicolas");
        let users = std::fs::read_to_string(server.config_dir().join("users.toml"))?;
        assert_eq!(users.matches("[users.").count(), 1, "{users}");
        Ok(())
    }

    #[tokio::test]
    async fn a_password_locks_the_home_and_only_its_hash_is_kept() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (app, cookie) = locked_home(&server).await?;

        let secrets = std::fs::read_to_string(server.config_dir().join("secrets.toml"))?;
        assert!(secrets.contains("[users.passwords]"), "{secrets}");
        assert!(secrets.contains("$argon2id$"), "a hash is kept");
        assert!(!secrets.contains("correct horse"), "the password is not");
        assert!(
            !std::fs::read_to_string(server.config_dir().join("users.toml"))?.contains("argon2")
        );

        // Nobody's request is refused, and says what to do.
        let (status, why, _) = ask(&app, "GET", "/api/home", None, None, false).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(why["error"].as_str().is_some_and(|e| e.contains("sign in")));
        // What it takes to get in stays open, and so does being told you're alive.
        for open in ["/api/health", "/api/session", "/", "/settings"] {
            let (status, _, _) = ask(&app, "GET", open, None, None, false).await?;
            assert_ne!(status, StatusCode::UNAUTHORIZED, "{open}");
        }
        let (_, session, _) = ask(&app, "GET", "/api/session", None, None, false).await?;
        assert_eq!(session["locked"], true);
        assert_eq!(session["user"], serde_json::Value::Null);
        // Who lives here isn't said to whoever asks.
        assert!(session.get("people").is_none(), "{session}");
        assert!(!session.to_string().contains("Nico"), "{session}");

        // The owner's is let through.
        let (status, _, _) = ask(&app, "GET", "/api/home", None, Some(&cookie), false).await?;
        assert_eq!(status, StatusCode::OK);
        let (_, session, _) = ask(&app, "GET", "/api/session", None, Some(&cookie), false).await?;
        assert_eq!(session["user"]["id"], "nico");
        assert_eq!(session["owner"], true);
        Ok(())
    }

    #[tokio::test]
    async fn signing_in_takes_the_right_password_and_signing_out_ends_it() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (app, _) = locked_home(&server).await?;
        let sign_in = |password: &'static str| {
            let app = app.clone();
            async move {
                ask(
                    &app,
                    "POST",
                    "/api/session",
                    Some(serde_json::json!({ "user": "nico", "password": password })),
                    None,
                    true,
                )
                .await
            }
        };
        let (status, why, cookie) = sign_in("battery staple").await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(cookie, None);
        assert!(
            why["error"]
                .as_str()
                .is_some_and(|e| e.contains("don't go together"))
        );
        // Somebody who isn't there gets the same answer: it doesn't say which was wrong.
        let (status, same, _) = ask(
            &app,
            "POST",
            "/api/session",
            Some(serde_json::json!({ "user": "nobody", "password": "correct horse" })),
            None,
            true,
        )
        .await?;
        assert_eq!((status, &same), (StatusCode::UNAUTHORIZED, &why));

        let (status, session, cookie) = sign_in("correct horse").await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(session["user"]["id"], "nico");
        // The name as a person types it works as well as the id.
        let (status, _, _) = ask(
            &app,
            "POST",
            "/api/session",
            Some(serde_json::json!({ "user": "  NICO ", "password": "correct horse" })),
            None,
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::OK);
        let cookie = cookie.expect("a session");
        assert!(cookie.starts_with("irori_session="));

        // The session is kept in the database: a restarted server still knows it.
        let again = server.app()?;
        let (status, _, _) = ask(&again, "GET", "/api/home", None, Some(&cookie), false).await?;
        assert_eq!(status, StatusCode::OK);

        let (status, _, _) = ask(&app, "DELETE", "/api/session", None, Some(&cookie), true).await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _, _) = ask(&app, "GET", "/api/home", None, Some(&cookie), false).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        Ok(())
    }

    #[tokio::test]
    async fn too_many_wrong_passwords_have_to_wait() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (app, _) = locked_home(&server).await?;
        let wrong = serde_json::json!({ "user": "nico", "password": "battery staple" });
        for _ in 0..5 {
            let (status, _, _) = ask(
                &app,
                "POST",
                "/api/session",
                Some(wrong.clone()),
                None,
                true,
            )
            .await?;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
        // The sixth isn't looked at, right or wrong.
        let right = serde_json::json!({ "user": "nico", "password": "correct horse" });
        let (status, why, _) = ask(&app, "POST", "/api/session", Some(right), None, true).await?;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert!(
            why["error"]
                .as_str()
                .is_some_and(|e| e.contains("try again in"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_change_in_a_locked_home_needs_the_pages_own_header() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (app, cookie) = locked_home(&server).await?;
        let area = serde_json::json!({ "name": "Hall" });
        // The cookie alone is what another site's form would send.
        let (status, why, _) = ask(
            &app,
            "POST",
            "/api/areas",
            Some(area.clone()),
            Some(&cookie),
            false,
        )
        .await?;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            why["error"]
                .as_str()
                .is_some_and(|e| e.contains("x-irori-ui"))
        );
        let (status, _, _) =
            ask(&app, "POST", "/api/areas", Some(area), Some(&cookie), true).await?;
        assert_eq!(status, StatusCode::CREATED);
        Ok(())
    }

    #[tokio::test]
    async fn a_user_uses_the_home_and_an_owner_runs_it() -> anyhow::Result<()> {
        let (core, _host) = demo().await?;
        let server = Server::new(core)?;
        let (app, owner) = locked_home(&server).await?;
        let (status, users, _) = ask(
            &app,
            "POST",
            "/api/users",
            Some(serde_json::json!({ "name": "Guest", "role": "user", "password": "let me in!" })),
            Some(&owner),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{users}");
        assert_eq!(users[0]["id"], "guest");
        assert_eq!(users[0]["has_password"], true);
        assert!(users.to_string().find("argon2").is_none(), "never the hash");

        let (status, session, guest) = ask(
            &app,
            "POST",
            "/api/session",
            Some(serde_json::json!({ "user": "guest", "password": "let me in!" })),
            None,
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(session["owner"], false);
        let guest = guest.expect("a session");

        // Sees everything, and switches a light.
        let (status, _, _) = ask(&app, "GET", "/api/home", None, Some(&guest), false).await?;
        assert_eq!(status, StatusCode::OK);
        let (status, state, _) = ask(
            &app,
            "POST",
            "/api/command",
            Some(serde_json::json!({ "entity_id": "switch.fridge_plug", "command": "turn_on" })),
            Some(&guest),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{state}");

        // Changes nothing about how the home is set up.
        let refused = [
            ("POST", "/api/areas", serde_json::json!({ "name": "Hall" })),
            (
                "PUT",
                "/api/place",
                serde_json::json!({ "time_zone": "Europe/Brussels" }),
            ),
            (
                "POST",
                "/api/users",
                serde_json::json!({ "name": "Friend", "password": "open sesame" }),
            ),
            ("DELETE", "/api/users/nico", serde_json::Value::Null),
            ("POST", "/api/restart", serde_json::Value::Null),
            (
                "POST",
                "/api/extensions/demo/install",
                serde_json::json!({}),
            ),
            (
                "PATCH",
                "/api/users/nico",
                serde_json::json!({ "password": "mine now!" }),
            ),
            (
                "PATCH",
                "/api/users/guest",
                serde_json::json!({ "role": "owner" }),
            ),
        ];
        for (method, path, body) in refused {
            let (status, why, _) = ask(&app, method, path, Some(body), Some(&guest), true).await?;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {why}");
            assert!(
                why["error"].as_str().is_some_and(|e| e.contains("owner")),
                "{why}"
            );
        }
        // A question for an extension's engine is for one that limits what a regular user can do
        // with it (`docs/specs/access.md` §4.1). Any other answers as if it had no page.
        let (status, why, _) = ask(
            &app,
            "POST",
            "/api/apps/automations/rpc",
            Some(serde_json::json!({ "method": "flows.list" })),
            Some(&guest),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{why}");
        // Their own password is theirs to change, with the one they have.
        for (current, expected) in [
            (serde_json::Value::Null, StatusCode::FORBIDDEN),
            ("not it at all".into(), StatusCode::FORBIDDEN),
            ("let me in!".into(), StatusCode::OK),
        ] {
            let (status, why, _) = ask(
                &app,
                "PATCH",
                "/api/users/guest",
                Some(
                    serde_json::json!({ "password": "a better one", "current_password": current }),
                ),
                Some(&guest),
                true,
            )
            .await?;
            assert_eq!(status, expected, "{why}");
        }
        // The owner can reset it without knowing it: that is what an owner is for.
        let (status, _, _) = ask(
            &app,
            "PATCH",
            "/api/users/guest",
            Some(serde_json::json!({ "password": "reset by owner" })),
            Some(&owner),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::OK);
        Ok(())
    }

    /// A person the owner adds and who signs in: their cookie.
    async fn person(app: &Router, owner: &str, name: &str, role: &str) -> anyhow::Result<String> {
        let (status, why, _) = ask(
            app,
            "POST",
            "/api/users",
            Some(serde_json::json!({ "name": name, "role": role, "password": "let me in!" })),
            Some(owner),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{why}");
        let (status, why, cookie) = ask(
            app,
            "POST",
            "/api/session",
            Some(serde_json::json!({ "user": name, "password": "let me in!" })),
            None,
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{why}");
        cookie.ok_or_else(|| anyhow::anyhow!("signing in sets a cookie"))
    }

    /// An update changes the software and firmware the home runs on, which is setup: a regular
    /// user never sends one a command, whatever the command (`docs/specs/access.md` §2.1).
    #[tokio::test]
    async fn a_user_sends_no_command_to_an_update() -> anyhow::Result<()> {
        let core = core();
        let _host = irori_core::ExtensionHost::start(
            &core,
            vec![
                irori_protocol::builtin::<crate::system_device::IroriDevice>()
                    .map_err(anyhow::Error::msg)?,
            ],
            irori_core::Timing::default(),
        )
        .map_err(anyhow::Error::msg)?;
        for _ in 0..500 {
            if core
                .entities()
                .iter()
                .any(|entity| entity.id.to_string() == "update.irori")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let server = Server::new(core)?;
        let (app, owner) = locked_home(&server).await?;
        let guest = person(&app, &owner, "Guest", "user").await?;

        for command in ["install", "check", "skip", "clear_skipped"] {
            let body = serde_json::json!({ "entity_id": "update.irori", "command": command });
            let (status, why, _) = ask(
                &app,
                "POST",
                "/api/command",
                Some(body.clone()),
                Some(&guest),
                true,
            )
            .await?;
            assert_eq!(status, StatusCode::FORBIDDEN, "{command}: {why}");
            assert!(
                why["error"].as_str().is_some_and(|e| e.contains("owner")),
                "{command}: {why}"
            );
            // The owner is not told that. What the update then does is its own business.
            let (status, why, _) =
                ask(&app, "POST", "/api/command", Some(body), Some(&owner), true).await?;
            assert_ne!(status, StatusCode::FORBIDDEN, "{command}: {why}");
        }
        Ok(())
    }

    /// The page shows a camera with an `<img>` that carries the session's cookie, so what a
    /// regular user sees of the home includes its pictures.
    #[tokio::test]
    async fn a_user_sees_a_camera_picture() -> anyhow::Result<()> {
        let (core, _host) = demo().await?;
        let server = Server::new(core)?;
        let (app, owner) = locked_home(&server).await?;
        let guest = person(&app, &owner, "Guest", "user").await?;
        for cookie in [&owner, &guest] {
            let (status, _, _) = ask(
                &app,
                "GET",
                "/api/entities/camera.garden_camera_1/picture",
                None,
                Some(cookie),
                false,
            )
            .await?;
            assert_eq!(status, StatusCode::OK);
        }
        Ok(())
    }

    /// The logs hold the names of everybody who signs in and what the owner did, and are read
    /// to run the home: they are the owner's (`docs/specs/access.md` §2.2).
    #[tokio::test]
    async fn the_logs_are_the_owners() -> anyhow::Result<()> {
        let (core, _host) = demo().await?;
        let server = Server::new(core)?;
        let (app, owner) = locked_home(&server).await?;
        let guest = person(&app, &owner, "Guest", "user").await?;
        for path in [
            "/api/system/log",
            "/api/assistant/log",
            "/api/extensions/demo/log",
        ] {
            let (status, why, _) = ask(&app, "GET", path, None, Some(&guest), false).await?;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {why}");
            let (status, why, _) = ask(&app, "GET", path, None, Some(&owner), false).await?;
            assert_ne!(status, StatusCode::FORBIDDEN, "{path}: {why}");
        }
        Ok(())
    }

    /// Only an owner makes tokens, so an owner who is made a regular user loses theirs: they
    /// stay valid for as long as the person does otherwise (`docs/specs/access.md` §5).
    #[tokio::test]
    async fn an_owner_made_a_user_loses_their_tokens() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (app, nico) = locked_home(&server).await?;
        let ana = person(&app, &nico, "Ana", "owner").await?;
        let (status, created, _) = ask(
            &app,
            "POST",
            "/api/tokens",
            Some(serde_json::json!({ "name": "Tablet", "scopes": ["states:read"] })),
            Some(&ana),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let secret = created["secret"].as_str().unwrap_or_default().to_owned();
        let (status, _) = bearer(&app, "GET", "/api/states", None, &secret).await?;
        assert_eq!(status, StatusCode::OK);

        let (status, why, _) = ask(
            &app,
            "PATCH",
            "/api/users/ana",
            Some(serde_json::json!({ "role": "user" })),
            Some(&nico),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{why}");
        let (status, why) = bearer(&app, "GET", "/api/states", None, &secret).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{why}");
        Ok(())
    }

    /// A token is looked at against its person each time it is used: an owner made a regular
    /// user by a hand edit of `users.toml` (or by a restore that brings their tokens back) doesn't
    /// keep a token, which is whole-home.
    #[tokio::test]
    async fn a_token_of_somebody_who_is_not_an_owner_is_not_heard() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (app, nico) = locked_home(&server).await?;
        let ana = person(&app, &nico, "Ana", "owner").await?;
        let (status, created, _) = ask(
            &app,
            "POST",
            "/api/tokens",
            Some(serde_json::json!({ "name": "Tablet", "scopes": ["states:read"] })),
            Some(&ana),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let secret = created["secret"].as_str().unwrap_or_default().to_owned();
        let (status, _) = bearer(&app, "GET", "/api/states", None, &secret).await?;
        assert_eq!(status, StatusCode::OK);

        // By hand, in the file: the tokens are not touched.
        let file = server.config_dir().join("users.toml");
        let text = std::fs::read_to_string(&file)?;
        let (before, after) = text.split_once("[users.ana]").expect("ana is in the file");
        let after = after.replacen("role = \"owner\"", "role = \"user\"", 1);
        std::fs::write(&file, format!("{before}[users.ana]{after}"))?;
        server.config.home().await;
        let (status, why) = bearer(&app, "GET", "/api/states", None, &secret).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{why}");

        // And back: she is an owner again, and the token is hers again.
        std::fs::write(&file, text)?;
        server.config.home().await;
        let (status, why) = bearer(&app, "GET", "/api/states", None, &secret).await?;
        assert_eq!(status, StatusCode::OK, "{why}");
        Ok(())
    }

    /// An edit that is refused for something else takes nothing away: the tokens of an owner told
    /// to be a regular user, and to have a room that is not there, are still theirs.
    #[tokio::test]
    async fn a_refused_demotion_leaves_the_tokens_alone() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (app, nico) = locked_home(&server).await?;
        let ana = person(&app, &nico, "Ana", "owner").await?;
        let (status, created, _) = ask(
            &app,
            "POST",
            "/api/tokens",
            Some(serde_json::json!({ "name": "Tablet", "scopes": ["states:read"] })),
            Some(&ana),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let secret = created["secret"].as_str().unwrap_or_default().to_owned();
        let (status, why, _) = ask(
            &app,
            "PATCH",
            "/api/users/ana",
            Some(serde_json::json!({ "role": "user", "areas": ["no_such_room"] })),
            Some(&nico),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{why}");
        let (status, why) = bearer(&app, "GET", "/api/states", None, &secret).await?;
        assert_eq!(
            status,
            StatusCode::OK,
            "the refused edit took the token: {why}"
        );
        Ok(())
    }

    /// Rooms are set by the owner, are rooms the home has, belong to somebody who doesn't run
    /// it, and go from a person's list when the room does (`docs/specs/access.md` §1).
    #[tokio::test]
    async fn a_person_has_rooms_the_owner_gave_and_loses_one_when_it_goes() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (app, owner) = locked_home(&server).await?;
        for name in ["Hall", "Study"] {
            let (status, why, _) = ask(
                &app,
                "POST",
                "/api/areas",
                Some(serde_json::json!({ "name": name })),
                Some(&owner),
                true,
            )
            .await?;
            assert_eq!(status, StatusCode::CREATED, "{why}");
        }
        let add = |body: serde_json::Value| {
            let (app, owner) = (app.clone(), owner.clone());
            async move { ask(&app, "POST", "/api/users", Some(body), Some(&owner), true).await }
        };
        let (status, users, _) = add(serde_json::json!({
            "name": "Leo", "password": "let me in!", "areas": ["hall", "study"],
        }))
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{users}");
        let rooms = |users: &serde_json::Value, id: &str| {
            users
                .as_array()
                .and_then(|all| all.iter().find(|user| user["id"] == id))
                .map(|user| user["areas"].clone())
        };
        assert_eq!(
            rooms(&users, "leo"),
            Some(serde_json::json!(["hall", "study"]))
        );
        assert_eq!(
            rooms(&users, "nico"),
            Some(serde_json::Value::Null),
            "an owner has no list"
        );

        // A room that isn't there, and rooms for an owner, are refused.
        for (body, said) in [
            (
                serde_json::json!({
                    "name": "Mia", "password": "let me in!", "areas": ["garage"],
                }),
                "no room `garage`",
            ),
            (
                serde_json::json!({
                    "name": "Mia", "password": "let me in!", "role": "owner", "areas": ["hall"],
                }),
                "owner has the whole home",
            ),
        ] {
            let (status, why, _) = add(body).await?;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{why}");
            assert!(
                why["error"].as_str().is_some_and(|e| e.contains(said)),
                "{why}"
            );
        }

        let patch = |who: &str, body: serde_json::Value, cookie: String| {
            let (app, who) = (app.clone(), who.to_owned());
            async move {
                ask(
                    &app,
                    "PATCH",
                    &format!("/api/users/{who}"),
                    Some(body),
                    Some(&cookie),
                    true,
                )
                .await
            }
        };
        // `null` is the whole home again, a list sets them, and leaving it out changes nothing.
        let (status, users, _) =
            patch("leo", serde_json::json!({ "areas": null }), owner.clone()).await?;
        assert_eq!(status, StatusCode::OK, "{users}");
        assert_eq!(rooms(&users, "leo"), Some(serde_json::Value::Null));
        let (_, users, _) = patch(
            "leo",
            serde_json::json!({ "areas": ["study", "study", "hall"] }),
            owner.clone(),
        )
        .await?;
        assert_eq!(
            rooms(&users, "leo"),
            Some(serde_json::json!(["study", "hall"])),
            "each once, in the order given"
        );
        let (_, users, _) =
            patch("leo", serde_json::json!({ "name": "Leo B" }), owner.clone()).await?;
        assert_eq!(
            rooms(&users, "leo"),
            Some(serde_json::json!(["study", "hall"]))
        );

        // Nobody gives themselves rooms, or takes them off.
        let leo = {
            let (status, why, cookie) = ask(
                &app,
                "POST",
                "/api/session",
                Some(serde_json::json!({ "user": "leo", "password": "let me in!" })),
                None,
                true,
            )
            .await?;
            assert_eq!(status, StatusCode::OK, "{why}");
            cookie.ok_or_else(|| anyhow::anyhow!("a cookie"))?
        };
        let (status, why, _) = patch("leo", serde_json::json!({ "areas": null }), leo).await?;
        assert_eq!(status, StatusCode::FORBIDDEN, "{why}");

        // Made an owner, they have the whole home.
        let (_, users, _) =
            patch("leo", serde_json::json!({ "role": "owner" }), owner.clone()).await?;
        assert_eq!(rooms(&users, "leo"), Some(serde_json::Value::Null));
        let (_, users, _) = patch(
            "leo",
            serde_json::json!({ "role": "user", "areas": ["hall", "study"] }),
            owner.clone(),
        )
        .await?;
        assert_eq!(
            rooms(&users, "leo"),
            Some(serde_json::json!(["hall", "study"]))
        );

        // A room that goes is taken off everybody's list in the same breath. A room made again
        // under the same name is a new room to them.
        let (status, _, _) =
            ask(&app, "DELETE", "/api/areas/hall", None, Some(&owner), true).await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, users, _) = ask(&app, "GET", "/api/users", None, Some(&owner), false).await?;
        assert_eq!(rooms(&users, "leo"), Some(serde_json::json!(["study"])));
        let (status, _, _) = ask(
            &app,
            "POST",
            "/api/areas",
            Some(serde_json::json!({ "name": "Hall" })),
            Some(&owner),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED);
        let (_, users, _) = ask(&app, "GET", "/api/users", None, Some(&owner), false).await?;
        assert_eq!(
            rooms(&users, "leo"),
            Some(serde_json::json!(["study"])),
            "the new Hall is not the old one"
        );
        // A person left with no room has none, not the whole home.
        let (_, _, _) = ask(&app, "DELETE", "/api/areas/study", None, Some(&owner), true).await?;
        let (_, users, _) = ask(&app, "GET", "/api/users", None, Some(&owner), false).await?;
        assert_eq!(rooms(&users, "leo"), Some(serde_json::json!([])));
        Ok(())
    }

    #[tokio::test]
    async fn everybody_who_is_added_has_a_password_of_their_own() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let app = server.app()?;
        // Nobody is let in before the home has its owner.
        let guest = serde_json::json!({ "name": "Guest", "password": "let me in!" });
        let (status, why, _) =
            ask(&app, "POST", "/api/users", Some(guest.clone()), None, false).await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            why["error"]
                .as_str()
                .is_some_and(|e| e.contains("set the home up first")),
            "{why}"
        );

        let (app, owner) = locked_home(&server).await?;
        for (body, said) in [
            (serde_json::json!({ "name": "Guest" }), "needs a password"),
            (
                serde_json::json!({ "name": "Guest", "password": "short" }),
                "at least 8",
            ),
            (
                serde_json::json!({ "name": "nico", "password": "let me in!" }),
                "already somebody",
            ),
        ] {
            let (status, why, _) =
                ask(&app, "POST", "/api/users", Some(body), Some(&owner), true).await?;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{why}");
            assert!(
                why["error"].as_str().is_some_and(|e| e.contains(said)),
                "{why}"
            );
        }
        let (status, _, _) =
            ask(&app, "POST", "/api/users", Some(guest), Some(&owner), true).await?;
        assert_eq!(status, StatusCode::CREATED);
        Ok(())
    }

    #[tokio::test]
    async fn a_home_always_has_somebody_to_run_it() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (app, owner) = locked_home(&server).await?;
        ask(
            &app,
            "POST",
            "/api/users",
            Some(serde_json::json!({ "name": "Guest", "password": "let me in!" })),
            Some(&owner),
            true,
        )
        .await?;
        // The only owner can't step down, or go.
        for (method, body) in [
            ("PATCH", serde_json::json!({ "role": "user" })),
            ("DELETE", serde_json::Value::Null),
        ] {
            let (status, why, _) = ask(
                &app,
                method,
                "/api/users/nico",
                Some(body),
                Some(&owner),
                true,
            )
            .await?;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{method}");
            assert!(
                why["error"]
                    .as_str()
                    .is_some_and(|e| e.contains("needs an owner")),
                "{why}"
            );
        }
        // A password can't be taken away while there are two people to tell apart.
        let (status, why, _) = ask(
            &app,
            "PATCH",
            "/api/users/nico",
            Some(serde_json::json!({ "password": "", "current_password": "correct horse" })),
            Some(&owner),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            why["error"]
                .as_str()
                .is_some_and(|e| e.contains("not taken away")),
            "{why}"
        );

        // Removing somebody signs them out and takes their hash with them.
        let (_, _, guest) = ask(
            &app,
            "POST",
            "/api/session",
            Some(serde_json::json!({ "user": "guest", "password": "let me in!" })),
            None,
            true,
        )
        .await?;
        let guest = guest.expect("a session");
        let (status, users, _) =
            ask(&app, "DELETE", "/api/users/guest", None, Some(&owner), true).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(users.as_array().map(Vec::len), Some(1));
        let (status, _, _) = ask(&app, "GET", "/api/home", None, Some(&guest), false).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let secrets = std::fs::read_to_string(server.config_dir().join("secrets.toml"))?;
        assert!(!secrets.contains("guest"), "{secrets}");

        // Alone again, the owner still can't open the home back up: a password is changed,
        // never taken away.
        let (status, why, _) = ask(
            &app,
            "PATCH",
            "/api/users/nico",
            Some(serde_json::json!({ "password": "", "current_password": "correct horse" })),
            Some(&owner),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{why}");
        let (status, _, _) = ask(&app, "GET", "/api/home", None, None, false).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        Ok(())
    }

    #[tokio::test]
    async fn where_the_home_is_lands_in_home_toml_and_reaches_the_core() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let app = server.app()?;
        let (status, home, _) = ask(&app, "GET", "/api/place", None, None, false).await?;
        assert_eq!((status, home), (StatusCode::OK, serde_json::json!({})));

        let place = serde_json::json!({
            "time_zone": "Europe/Brussels",
            "location": { "latitude": 50.846_712_3, "longitude": 4.3525, "label": "Brussels, Belgium" },
        });
        let (status, kept, _) = ask(&app, "PUT", "/api/place", Some(place), None, false).await?;
        assert_eq!(status, StatusCode::OK, "{kept}");
        // To about a metre, which is what's written down.
        assert_eq!(kept["location"]["latitude"], 50.84671);

        let file = std::fs::read_to_string(server.config_dir().join("home.toml"))?;
        assert!(file.contains("time_zone = \"Europe/Brussels\""), "{file}");
        assert!(file.contains("latitude = 50.84671"), "{file}");
        assert!(file.contains("label = \"Brussels, Belgium\""), "{file}");
        // The core has it, which is how the engines that fire by the clock are told.
        let known = server.core.place();
        assert_eq!(
            known.time_zone.map(|zone| zone.to_string()).as_deref(),
            Some("Europe/Brussels")
        );
        let (_, session, _) = ask(&app, "GET", "/api/session", None, None, false).await?;
        assert_eq!(session["setup"]["place"], true);
        Ok(())
    }

    #[tokio::test]
    async fn a_place_that_cant_be_is_refused_and_says_why() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let app = server.app()?;
        for (place, said) in [
            (
                serde_json::json!({ "time_zone": "Europe/Atlantis" }),
                "isn't a time zone Irori knows",
            ),
            (
                serde_json::json!({ "time_zone": "Europe Brussels" }),
                "isn't a time zone name",
            ),
            (
                serde_json::json!({ "location": { "latitude": 123.0, "longitude": 4.0 } }),
                "from -90 to 90",
            ),
        ] {
            let (status, why, _) =
                ask(&app, "PUT", "/api/place", Some(place.clone()), None, false).await?;
            assert!(status.is_client_error(), "{place}: {status}");
            let text = why["error"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| why.to_string());
            // A body that doesn't parse is refused by the framework, in its own words.
            assert!(
                text.contains(said) || status == StatusCode::UNPROCESSABLE_ENTITY,
                "{text}"
            );
        }
        assert!(
            !server.config_dir().join("home.toml").exists(),
            "nothing was written"
        );
        Ok(())
    }

    #[tokio::test]
    async fn people_written_into_the_file_by_hand_are_the_home_s_people() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        std::fs::write(
            server.config_dir().join("users.toml"),
            "[users.ana]\nname = \"Ana\"\nrole = \"owner\"\n",
        )?;
        // Picked up the way every file is: the next time the directory is read.
        server.config.home().await;
        let (_, users, _) = ask(&server.app()?, "GET", "/api/users", None, None, false).await?;
        assert_eq!(users[0]["name"], "Ana");
        assert_eq!(users[0]["has_password"], false);
        Ok(())
    }

    #[tokio::test]
    async fn wrong_passwords_sent_all_at_once_are_each_counted() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (app, _) = locked_home(&server).await?;
        let wrong = serde_json::json!({ "user": "nico", "password": "battery staple" });
        let tries: Vec<_> = (0..12)
            .map(|_| {
                let (app, wrong) = (app.clone(), wrong.clone());
                tokio::spawn(async move {
                    ask(&app, "POST", "/api/session", Some(wrong), None, true)
                        .await
                        .map(|(status, _, _)| status)
                })
            })
            .collect();
        let mut looked_at = 0;
        for one in tries {
            if one.await?? == StatusCode::UNAUTHORIZED {
                looked_at += 1;
            }
        }
        // Five passwords are looked at; the rest are turned away unread.
        assert_eq!(looked_at, 5);
        Ok(())
    }

    #[tokio::test]
    async fn names_that_are_nobodys_share_one_count() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (app, _) = locked_home(&server).await?;
        let mut statuses = Vec::new();
        for n in 0..7 {
            let body = serde_json::json!({ "user": format!("nobody_{n}"), "password": "whatever" });
            statuses.push(
                ask(&app, "POST", "/api/session", Some(body), None, true)
                    .await?
                    .0,
            );
        }
        assert_eq!(statuses[..5], [StatusCode::UNAUTHORIZED; 5]);
        assert_eq!(statuses[5..], [StatusCode::TOO_MANY_REQUESTS; 2]);
        // And the owner, who has guessed nothing, is not kept waiting by it.
        let right = serde_json::json!({ "user": "nico", "password": "correct horse" });
        let (status, _, _) = ask(&app, "POST", "/api/session", Some(right), None, true).await?;
        assert_eq!(status, StatusCode::OK);
        Ok(())
    }

    #[tokio::test]
    async fn a_location_needs_its_time_zone_and_a_zone_stands_alone() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let app = server.app()?;
        let (status, why, _) = ask(
            &app,
            "PUT",
            "/api/place",
            Some(serde_json::json!({ "location": { "latitude": 50.8, "longitude": 4.3 } })),
            None,
            false,
        )
        .await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            why["error"]
                .as_str()
                .is_some_and(|e| e.contains("needs a time zone")),
            "{why}"
        );
        let (status, kept, _) = ask(
            &app,
            "PUT",
            "/api/place",
            Some(serde_json::json!({ "time_zone": "Asia/Tokyo" })),
            None,
            false,
        )
        .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(kept, serde_json::json!({ "time_zone": "Asia/Tokyo" }));
        Ok(())
    }

    #[tokio::test]
    async fn the_first_password_has_to_be_an_owners() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        // Two people written into the file by hand, neither with a password.
        std::fs::write(
            server.config_dir().join("users.toml"),
            "[users.ana]\nname = \"Ana\"\nrole = \"owner\"\n\n[users.bo]\nname = \"Bo\"\nrole = \"user\"\n",
        )?;
        server.config.home().await;
        let app = server.app()?;
        let (status, why, _) = ask(
            &app,
            "PATCH",
            "/api/users/bo",
            Some(serde_json::json!({ "password": "let me in!" })),
            None,
            false,
        )
        .await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            why["error"]
                .as_str()
                .is_some_and(|e| e.contains("owner needs a password")),
            "{why}"
        );
        // Still open, and nothing secret was written.
        let (status, _, _) = ask(&app, "GET", "/api/home", None, None, false).await?;
        assert_eq!(status, StatusCode::OK);
        assert!(!server.config_dir().join("secrets.toml").exists());
        Ok(())
    }

    /// Takes one person's password out of `secrets.toml`, the way a person would by hand.
    async fn forget_password_in_the_files(server: &Server, id: &str) -> anyhow::Result<()> {
        let path = server.config_dir().join("secrets.toml");
        let kept: String = std::fs::read_to_string(&path)?
            .lines()
            .filter(|line| !line.trim_start().starts_with(&format!("{id} =")))
            .map(|line| format!("{line}\n"))
            .collect();
        // Long enough after the last write for the file to read as changed.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        std::fs::write(&path, kept)?;
        server.config.home().await;
        Ok(())
    }

    #[tokio::test]
    async fn a_password_put_right_from_the_files_signs_every_old_screen_out() -> anyhow::Result<()>
    {
        let server = Server::new(core())?;
        let (app, old) = locked_home(&server).await?;
        forget_password_in_the_files(&server, "nico").await?;
        // With its password gone, the session that was begun with it is nobody's.
        let (_, session, _) = ask(&app, "GET", "/api/session", None, Some(&old), false).await?;
        assert_eq!(session["setup"]["owner"], false, "{session}");

        let (status, _, new) = ask(
            &app,
            "POST",
            "/api/setup",
            Some(serde_json::json!({ "name": "Nico", "password": "a new one, remembered" })),
            None,
            false,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED);
        let new = new.expect("a session");
        // The lost phone is out; the screen that set the password is in.
        let (status, _, _) = ask(&app, "GET", "/api/home", None, Some(&old), false).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _, _) = ask(&app, "GET", "/api/home", None, Some(&new), false).await?;
        assert_eq!(status, StatusCode::OK);
        Ok(())
    }

    #[tokio::test]
    async fn an_owner_who_forgot_can_be_set_up_again_while_somebody_else_still_has_a_password()
    -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (app, owner) = locked_home(&server).await?;
        ask(
            &app,
            "POST",
            "/api/users",
            Some(serde_json::json!({ "name": "Guest", "password": "let me in!" })),
            Some(&owner),
            true,
        )
        .await?;
        forget_password_in_the_files(&server, "nico").await?;

        // Still locked, because the guest has a password, and with no owner who can sign in:
        // the page is told so, and shows the welcome instead of a sign-in nobody could pass.
        let (_, session, _) = ask(&app, "GET", "/api/session", None, None, false).await?;
        assert_eq!(session["locked"], true);
        assert_eq!(session["setup"]["owner"], false);
        let (status, _, _) = ask(&app, "GET", "/api/home", None, None, false).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, session, cookie) = ask(
            &app,
            "POST",
            "/api/setup",
            Some(serde_json::json!({ "name": "Nico", "password": "a new one, remembered" })),
            None,
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{session}");
        assert_eq!(session["user"]["id"], "nico");
        assert_eq!(session["owner"], true);
        assert!(cookie.is_some());
        // The guest is still there, with the password they had.
        let (status, _, _) = ask(
            &app,
            "POST",
            "/api/session",
            Some(serde_json::json!({ "user": "guest", "password": "let me in!" })),
            None,
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::OK);
        // And with the owner back, the door is shut again.
        let (status, _, _) = ask(
            &app,
            "POST",
            "/api/setup",
            Some(serde_json::json!({ "name": "Mallory", "password": "let me in!" })),
            None,
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        Ok(())
    }

    /// One request carrying `Authorization: Bearer`, and no cookie. A token is the whole
    /// credential: the page's header is not required, and a bad header is not a cookie.
    async fn bearer(
        app: &Router,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
        secret: &str,
    ) -> anyhow::Result<(StatusCode, serde_json::Value)> {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {secret}"));
        let request = match body {
            Some(body) => request
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body)?))?,
            None => request.body(Body::empty())?,
        };
        let response = app.clone().oneshot(request).await?;
        let status = response.status();
        let bytes = response.into_body().collect().await?.to_bytes();
        Ok((
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        ))
    }

    fn token_count(dir: &std::path::Path) -> anyhow::Result<i64> {
        let conn = rusqlite::Connection::open(dir.join("irori.db"))?;
        conn.query_row("SELECT COUNT(*) FROM tokens", [], |row| row.get(0))
            .map_err(Into::into)
    }

    fn database_holds(dir: &std::path::Path, secret: &str) -> bool {
        ["irori.db", "irori.db-wal", "irori.db-shm"]
            .into_iter()
            .any(|name| {
                std::fs::read(dir.join(name)).is_ok_and(|bytes| {
                    bytes
                        .windows(secret.len())
                        .any(|window| window == secret.as_bytes())
                })
            })
    }

    /// An owner makes a token, the program uses only what it was given, and the secret is
    /// shown once (`docs/specs/api.md` §2).
    #[tokio::test]
    async fn a_program_uses_a_scoped_token_and_cannot_run_the_home() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let app = server.app()?;
        let (status, created, _) = ask(
            &app,
            "POST",
            "/api/tokens",
            Some(serde_json::json!({
                "name": "Tablet",
                "scopes": ["states:read"],
            })),
            None,
            false,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["id"], "tablet");
        assert_eq!(created["scopes"], serde_json::json!(["states:read"]));
        let secret = created["secret"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("the secret is shown once: {created}"))?
            .to_owned();
        assert!(
            secret.starts_with("irori_") && secret.len() == "irori_".len() + 64,
            "a secret is irori_ and 32 random bytes, got {secret}"
        );
        assert!(
            secret["irori_".len()..]
                .chars()
                .all(|c| c.is_ascii_hexdigit()),
            "{secret}"
        );
        assert!(
            !database_holds(server.dir.path(), &secret),
            "the database keeps the hash, never the secret"
        );

        let (status, listed, _) = ask(&app, "GET", "/api/tokens", None, None, false).await?;
        assert_eq!(status, StatusCode::OK);
        let listed_text = listed.to_string();
        assert!(!listed_text.contains(&secret), "{listed}");
        assert!(!listed_text.contains("token_hash"), "{listed}");
        assert_eq!(listed[0]["id"], "tablet");
        assert!(listed[0].get("secret").is_none(), "{listed}");

        // The same name again is the same token, not a second secret.
        let (status, why, _) = ask(
            &app,
            "POST",
            "/api/tokens",
            Some(serde_json::json!({ "name": "Tablet", "scopes": ["states:read"] })),
            None,
            false,
        )
        .await?;
        assert_eq!(status, StatusCode::CONFLICT, "{why}");
        assert!(
            why["error"]
                .as_str()
                .is_some_and(|error| error.contains("already a token")),
            "{why}"
        );

        for (body, said) in [
            (serde_json::json!({ "name": "Empty" }), "needs a scope"),
            (
                serde_json::json!({ "name": "Twice", "scopes": ["states:read", "states:read"] }),
                "more than once",
            ),
            (
                serde_json::json!({ "name": "...", "scopes": ["states:read"] }),
                "no letters or numbers",
            ),
            (
                serde_json::json!({
                    "name": "Both",
                    "scopes": ["states:read"],
                    "extension": "bridge",
                }),
                "not both",
            ),
            (
                serde_json::json!({ "name": "Bridge", "extension": "bridge" }),
                "irori-extension.toml",
            ),
        ] {
            let (status, why, _) =
                ask(&app, "POST", "/api/tokens", Some(body), None, false).await?;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{why}");
            assert!(
                why["error"]
                    .as_str()
                    .is_some_and(|error| error.contains(said)),
                "expected {said} in {why}"
            );
        }

        // A header that isn't a token Irori issued is refused, even while the home is open
        // and a missing header would be treated as the owner.
        for header in ["Bearer nope", "nope", "Bearer "] {
            let (status, body) = server
                .send(
                    Request::get("/api/home")
                        .header("authorization", header)
                        .body(Body::empty())?,
                )
                .await?;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{header}: {body:?}");
        }

        // states:read sees states, and nothing that needs the registry or a service call.
        let (status, _) = bearer(&app, "GET", "/api/states", None, &secret).await?;
        assert_eq!(status, StatusCode::OK);
        let (status, why) = bearer(&app, "GET", "/api/devices", None, &secret).await?;
        assert_eq!(status, StatusCode::FORBIDDEN, "{why}");
        assert!(
            why["error"]
                .as_str()
                .is_some_and(|error| error.contains("registry:read")),
            "{why}"
        );
        let (status, why) = bearer(
            &app,
            "POST",
            "/api/command",
            Some(serde_json::json!({ "entity_id": "light.hall", "command": "turn_on" })),
            &secret,
        )
        .await?;
        assert_eq!(status, StatusCode::FORBIDDEN, "{why}");
        assert!(
            why["error"]
                .as_str()
                .is_some_and(|error| error.contains("services:call")),
            "{why}"
        );
        let (status, why) = bearer(
            &app,
            "POST",
            "/api/tokens",
            Some(serde_json::json!({ "name": "Another", "scopes": ["states:read"] })),
            &secret,
        )
        .await?;
        assert_eq!(status, StatusCode::FORBIDDEN, "{why}");
        assert!(
            why["error"]
                .as_str()
                .is_some_and(|error| error.contains("can't change how the home is set up")),
            "{why}"
        );

        let (status, _) = bearer(&app, "DELETE", "/api/tokens/tablet", None, &secret).await?;
        // Revoking is running the home. The page does it; the token does not.
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, _) = ask(&app, "DELETE", "/api/tokens/tablet", None, None, false).await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, why) = bearer(&app, "GET", "/api/states", None, &secret).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{why}");
        let (status, why, _) = ask(&app, "DELETE", "/api/tokens/tablet", None, None, false).await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{why}");
        assert!(
            why["error"]
                .as_str()
                .is_some_and(|error| error.contains("there's no token called `tablet`")),
            "{why}"
        );
        Ok(())
    }

    /// Setting the home up again is also how a forgotten password is replaced, so every token
    /// from before that stops working.
    #[tokio::test]
    async fn setting_the_home_up_revokes_every_token() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let app = server.app()?;
        let (status, created, _) = ask(
            &app,
            "POST",
            "/api/tokens",
            Some(serde_json::json!({ "name": "Tablet", "scopes": ["states:read"] })),
            None,
            false,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let secret = created["secret"].as_str().expect("shown once").to_owned();
        assert_eq!(token_count(server.dir.path())?, 1);

        let (status, _, _) = ask(
            &app,
            "POST",
            "/api/setup",
            Some(serde_json::json!({ "name": "Nico", "password": "correct horse" })),
            None,
            false,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(token_count(server.dir.path())?, 0);
        let (status, _) = bearer(&app, "GET", "/api/states", None, &secret).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        Ok(())
    }

    /// A command from a program is the program's, not the person's who made the token. Changing
    /// that person's password signs their browsers out and leaves the token in place.
    #[tokio::test]
    async fn a_token_calls_a_service_as_itself_and_survives_a_password_change() -> anyhow::Result<()>
    {
        let (core, host) = demo().await?;
        let mut events = core.subscribe();
        let server = Server::new(core)?;
        let (app, owner) = locked_home(&server).await?;
        let (status, created, _) = ask(
            &app,
            "POST",
            "/api/tokens",
            Some(serde_json::json!({
                "name": "Tablet",
                "scopes": ["services:call", "states:read", "registry:read"],
            })),
            Some(&owner),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let secret = created["secret"].as_str().expect("shown once").to_owned();

        let (status, state) = bearer(
            &app,
            "POST",
            "/api/command",
            Some(
                serde_json::json!({ "entity_id": "light.living_room_lamp", "command": "turn_on" }),
            ),
            &secret,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{state}");
        // The device changed. The call that asked it to is the token's, on the event.
        assert_eq!(state["context"]["origin"]["type"], "device", "{state}");
        assert!(state["context"]["parent_id"].is_string(), "{state}");
        let mut saw_token = false;
        while let Ok(event) = events.try_recv() {
            if let Event::ServiceCalled { context, .. } = event {
                saw_token |= matches!(
                    context.origin,
                    Origin::Api { ref token_id } if token_id.as_str() == "tablet"
                );
            }
        }
        assert!(
            saw_token,
            "the service call should be attributed to the token"
        );

        // The page's cookie still needs the header only the page sends. The token did not.
        let (status, why, _) = ask(
            &app,
            "POST",
            "/api/command",
            Some(
                serde_json::json!({ "entity_id": "light.living_room_lamp", "command": "turn_off" }),
            ),
            Some(&owner),
            false,
        )
        .await?;
        assert_eq!(status, StatusCode::FORBIDDEN, "{why}");
        assert!(
            why["error"]
                .as_str()
                .is_some_and(|error| error.contains("x-irori-ui")),
            "{why}"
        );

        let (status, _, _) = ask(
            &app,
            "PATCH",
            "/api/users/nico",
            Some(serde_json::json!({
                "password": "a longer phrase",
                "current_password": "correct horse",
            })),
            Some(&owner),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::OK);
        let (status, state) = bearer(
            &app,
            "POST",
            "/api/command",
            Some(
                serde_json::json!({ "entity_id": "light.living_room_lamp", "command": "turn_off" }),
            ),
            &secret,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{state}");

        host.shutdown().await;
        Ok(())
    }

    /// The socket pushes the home as its first frame and takes nothing from the client.
    #[tokio::test]
    async fn the_live_socket_opens_with_the_home() -> anyhow::Result<()> {
        use futures_util::StreamExt as _;

        let server = Server::new(core())?;
        let app = server.app()?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let serve = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let url = format!("ws://{address}/api/ws");
        let mut socket = None;
        for _ in 0..50 {
            match tokio_tungstenite::connect_async(&url).await {
                Ok((connected, _)) => {
                    socket = Some(connected);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
        let mut socket = socket.ok_or_else(|| anyhow::anyhow!("the live socket did not open"))?;
        let frame = socket
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("the socket closed before a frame"))??;
        let text = match frame {
            tokio_tungstenite::tungstenite::Message::Text(text) => text.to_string(),
            other => anyhow::bail!("the first frame should be text, got {other:?}"),
        };
        let message: serde_json::Value = serde_json::from_str(&text)?;
        assert_eq!(message["type"], "snapshot");
        assert!(message["home"]["devices"].is_array(), "{message}");
        serve.abort();
        Ok(())
    }

    /// `irori-client` against the real guard, not a stand-in for it: `irori login` signs in
    /// with no header, a change on that session passes because the client sends the page's
    /// header, and the token it mints is refused a setup change and pointed at `irori login`.
    #[tokio::test]
    async fn the_cli_signs_in_and_a_token_is_still_only_a_token() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (app, _) = locked_home(&server).await?;
        let origin = format!("http://{}", listen(app).await?);
        let client = |origin: &str| irori_client::Builder::new(origin).map_err(anyhow::Error::new);

        let (session, cookie) = client(&origin)?
            .build()?
            .sign_in("Nico", "correct horse")
            .await?;
        assert!(session.owner);

        let signed_in = client(&origin)?.session(cookie).build()?;
        let body =
            serde_json::json!({ "name": "tablet", "scopes": ["registry:read", "states:read"] });
        let created: irori_client::CreatedToken = signed_in.post("/api/tokens", &body).await?;

        let token = client(&origin)?.bearer(created.secret).build()?;
        let _: serde_json::Value = token.get("/api/home").await?;
        let refused = token
            .post::<serde_json::Value>("/api/tokens", &body)
            .await
            .expect_err("a token can't mint a token");
        assert_eq!(refused.status, Some(403), "{refused}");
        let said = crate::cli::explain(&refused);
        assert!(said.contains("irori login"), "{said}");
        Ok(())
    }

    /// Holds an exclusive lock on the database so the next write fails at once.
    fn hold_database(dir: &std::path::Path) -> anyhow::Result<rusqlite::Connection> {
        let conn = rusqlite::Connection::open(dir.join("irori.db"))?;
        conn.execute_batch("BEGIN EXCLUSIVE")?;
        Ok(conn)
    }

    async fn listen(app: Router) -> anyhow::Result<std::net::SocketAddr> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(address)
    }

    /// A header that names some other scheme is the cookie. A bearer token that isn't one
    /// Irori issued is refused, and does not fall through onto a cookie that would have worked.
    #[tokio::test]
    async fn basic_auth_is_the_cookie_and_a_bad_bearer_is_not() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (status, bytes) = server
            .send(
                Request::get("/api/home")
                    .header("authorization", "Basic bm9wZTpub3Bl")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let (status, bytes) = server
            .send(
                Request::get("/api/health")
                    .header("authorization", "Basic bm9wZTpub3Bl")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );

        let (app, cookie) = locked_home(&server).await?;
        let (status, body, _) = ask(&app, "GET", "/api/home", None, Some(&cookie), false).await?;
        // The cookie is what gets in. Basic came along and changed nothing.
        let request = Request::get("/api/home")
            .header("authorization", "Basic bm9wZTpub3Bl")
            .header("cookie", &cookie)
            .body(Body::empty())?;
        let response = app.clone().oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let blocked = Request::get("/api/home")
            .header("authorization", "Bearer nope")
            .header("cookie", &cookie)
            .body(Body::empty())?;
        let response = app.clone().oneshot(blocked).await?;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let alone = Request::get("/api/home")
            .header("authorization", "Basic bm9wZTpub3Bl")
            .body(Body::empty())?;
        let response = app.oneshot(alone).await?;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{status} {body}"
        );
        Ok(())
    }

    /// Revoking has to fail where the token can still be used. A database that won't take
    /// the delete leaves the token in place, and says so, instead of pretending it's gone.
    #[tokio::test]
    async fn a_revoke_that_cant_be_written_leaves_the_token_working() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let app = server.app()?;
        let (status, created, _) = ask(
            &app,
            "POST",
            "/api/tokens",
            Some(serde_json::json!({ "name": "Tablet", "scopes": ["states:read"] })),
            None,
            false,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let secret = created["secret"].as_str().expect("shown once").to_owned();
        let _lock = hold_database(server.dir.path())?;

        let (status, why, _) = ask(&app, "DELETE", "/api/tokens/tablet", None, None, false).await?;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{why}");
        assert!(
            why["error"]
                .as_str()
                .is_some_and(|error| error.contains("couldn't write it down")),
            "{why}"
        );
        let (status, _) = bearer(&app, "GET", "/api/states", None, &secret).await?;
        assert_eq!(status, StatusCode::OK);

        let (status, why, _) = ask(
            &app,
            "POST",
            "/api/setup",
            Some(serde_json::json!({ "name": "Nico", "password": "correct horse" })),
            None,
            false,
        )
        .await?;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{why}");
        let (status, home, _) = ask(&app, "GET", "/api/home", None, None, false).await?;
        assert_eq!(status, StatusCode::OK, "{home}");
        let (status, _) = bearer(&app, "GET", "/api/states", None, &secret).await?;
        assert_eq!(status, StatusCode::OK);
        Ok(())
    }

    /// Removing somebody revokes their tokens first. If that can't be written, they stay.
    #[tokio::test]
    async fn a_person_stays_when_their_tokens_cant_be_revoked() -> anyhow::Result<()> {
        let server = Server::new(core())?;
        let (app, owner) = locked_home(&server).await?;
        // Somebody else is here, so the only owner can't be removed. That refusal has to
        // come before any token of theirs is forgotten.
        ask(
            &app,
            "POST",
            "/api/users",
            Some(serde_json::json!({ "name": "Guest", "password": "let me in!" })),
            Some(&owner),
            true,
        )
        .await?;
        let (status, created, _) = ask(
            &app,
            "POST",
            "/api/tokens",
            Some(serde_json::json!({ "name": "Phone", "scopes": ["states:read"] })),
            Some(&owner),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let phone = created["secret"].as_str().expect("shown once").to_owned();
        let (status, why, _) =
            ask(&app, "DELETE", "/api/users/nico", None, Some(&owner), true).await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{why}");
        let (status, _) = bearer(&app, "GET", "/api/states", None, &phone).await?;
        assert_eq!(status, StatusCode::OK);

        let (status, _, _) = ask(
            &app,
            "POST",
            "/api/users",
            Some(serde_json::json!({
                "name": "Ada",
                "role": "owner",
                "password": "correct horse",
            })),
            Some(&owner),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED);
        let (status, _, ada_cookie) = ask(
            &app,
            "POST",
            "/api/session",
            Some(serde_json::json!({ "user": "Ada", "password": "correct horse" })),
            None,
            false,
        )
        .await?;
        assert_eq!(status, StatusCode::OK);
        let ada_cookie = ada_cookie.expect("Ada is signed in");
        let (status, created, _) = ask(
            &app,
            "POST",
            "/api/tokens",
            Some(serde_json::json!({ "name": "Tablet", "scopes": ["states:read"] })),
            Some(&ada_cookie),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let secret = created["secret"].as_str().expect("shown once").to_owned();
        let _lock = hold_database(server.dir.path())?;

        let (status, why, _) =
            ask(&app, "DELETE", "/api/users/ada", None, Some(&owner), true).await?;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{why}");
        let (status, users, _) = ask(&app, "GET", "/api/users", None, Some(&owner), false).await?;
        assert_eq!(status, StatusCode::OK, "{users}");
        assert!(
            users
                .as_array()
                .is_some_and(|users| { users.iter().any(|user| user["id"] == "ada") }),
            "{users}"
        );
        let (status, _) = bearer(&app, "GET", "/api/states", None, &secret).await?;
        assert_eq!(status, StatusCode::OK);

        Ok(())
    }

    async fn open_live(
        address: std::net::SocketAddr,
        headers: &[(&str, &str)],
    ) -> anyhow::Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    > {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut request = format!("ws://{address}/api/ws").into_client_request()?;
        for (name, value) in headers {
            request.headers_mut().insert(
                axum::http::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|error| anyhow::anyhow!("{error}"))?,
                axum::http::HeaderValue::from_str(value)
                    .map_err(|error| anyhow::anyhow!("{error}"))?,
            );
        }
        let mut socket = None;
        let mut last = None;
        for _ in 0..50 {
            match tokio_tungstenite::connect_async(request.clone()).await {
                Ok((connected, _)) => {
                    socket = Some(connected);
                    break;
                }
                Err(error) => {
                    last = Some(error);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
        socket.ok_or_else(|| anyhow::anyhow!("the live socket did not open: {last:?}"))
    }

    /// The upgrade was refused, once the server is accepting. A connection that hasn't
    /// started yet is tried again.
    async fn refused_upgrade(
        address: std::net::SocketAddr,
        origin: &str,
    ) -> anyhow::Result<StatusCode> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut last = None;
        for _ in 0..50 {
            let mut request = format!("ws://{address}/api/ws").into_client_request()?;
            request.headers_mut().insert("origin", origin.parse()?);
            match tokio_tungstenite::connect_async(request).await {
                Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                    return Ok(response.status());
                }
                Err(error) => {
                    last = Some(error);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Ok(_) => anyhow::bail!("the socket opened for origin {origin}"),
            }
        }
        Err(anyhow::anyhow!("the socket was not refused: {last:?}"))
    }

    /// Another website's page is refused. This server's page, and a program with no
    /// origin, are not.
    #[tokio::test]
    async fn a_socket_from_another_site_is_refused() -> anyhow::Result<()> {
        use futures_util::StreamExt as _;
        let server = Server::new(core())?;
        let address = listen(server.app()?).await?;
        assert_eq!(
            refused_upgrade(address, "http://evil.example").await?,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            refused_upgrade(address, "null").await?,
            StatusCode::FORBIDDEN
        );

        let mut socket = open_live(address, &[("origin", &format!("http://{address}"))]).await?;
        let frame = socket.next().await;
        drop(socket);
        let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) = frame else {
            anyhow::bail!("this server's page should get a snapshot, got {frame:?}");
        };
        assert!(text.to_string().contains("snapshot"));
        let mut program = open_live(address, &[]).await?;
        let frame = program.next().await;
        assert!(
            matches!(
                frame,
                Some(Ok(tokio_tungstenite::tungstenite::Message::Text(_)))
            ),
            "a program with no origin still connects, got {frame:?}"
        );
        Ok(())
    }

    /// Signing out, and revoking a token, closes the socket that was opened with it.
    #[tokio::test]
    async fn signing_out_and_revoking_close_the_socket() -> anyhow::Result<()> {
        use futures_util::StreamExt as _;
        let server = Server::new(core())?;
        let (app, cookie) = locked_home(&server).await?;
        let (status, created, _) = ask(
            &app,
            "POST",
            "/api/tokens",
            Some(serde_json::json!({
                "name": "Tablet",
                "scopes": ["registry:read", "states:read", "events:read"],
            })),
            Some(&cookie),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let secret = created["secret"].as_str().expect("shown once").to_owned();
        let address = listen(app.clone()).await?;

        let mut browser = open_live(address, &[("cookie", cookie.as_str())]).await?;
        let mut program =
            open_live(address, &[("authorization", &format!("Bearer {secret}"))]).await?;
        for (who, socket) in [("the page", &mut browser), ("the program", &mut program)] {
            let snapshot = socket.next().await;
            assert!(
                matches!(
                    snapshot,
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(_)))
                ),
                "{who} should get a snapshot, got {snapshot:?}"
            );
        }

        // The token's socket closes. The page's, opened with the cookie, stays.
        let (status, _, _) = ask(
            &app,
            "DELETE",
            "/api/tokens/tablet",
            None,
            Some(&cookie),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let closed = tokio::time::timeout(Duration::from_secs(2), program.next()).await;
        assert!(
            matches!(
                closed,
                Ok(None
                    | Some(Err(_))
                    | Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))))
            ),
            "revoking the token should close its socket, got {closed:?}"
        );
        let still = tokio::time::timeout(Duration::from_millis(200), browser.next()).await;
        assert!(
            still.is_err(),
            "the page's socket stays until its sign-in ends, got {still:?}"
        );

        let (status, _, _) =
            ask(&app, "DELETE", "/api/session", None, Some(&cookie), false).await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let closed = tokio::time::timeout(Duration::from_secs(2), browser.next()).await;
        assert!(
            matches!(
                closed,
                Ok(None
                    | Some(Err(_))
                    | Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))))
            ),
            "signing out should close the socket, got {closed:?}"
        );
        Ok(())
    }

    // --- Rooms: what a person who has some of the home is shown (`docs/specs/access.md` §2) ---

    /// Code that answers a request reads the home through [`irori_core::Seen`], which holds only
    /// what the person has. `Core`'s own getters are the whole home's: a call to one in this code
    /// has to say why, in a `// whole-home:` comment on the line before, and the reason is always
    /// that only an owner gets that far. A new handler that reaches for the whole home fails here
    /// until somebody has said it is meant to.
    #[test]
    fn the_whole_home_is_read_only_where_it_says_why() {
        let sources: &[(&str, &str)] = &[
            ("server.rs", include_str!("server.rs")),
            ("server/people.rs", include_str!("server/people.rs")),
            ("server/live.rs", include_str!("server/live.rs")),
            ("server/apps.rs", include_str!("server/apps.rs")),
            ("server/tokens.rs", include_str!("server/tokens.rs")),
            ("server/activity.rs", include_str!("server/activity.rs")),
            ("server/backups.rs", include_str!("server/backups.rs")),
            ("server/trail.rs", include_str!("server/trail.rs")),
            ("server/auth.rs", include_str!("server/auth.rs")),
            ("assistant.rs", include_str!("assistant.rs")),
        ];
        #[cfg(feature = "assist")]
        let assist = ("server/assistant.rs", include_str!("server/assistant.rs"));
        let getters = [
            "devices",
            "entities",
            "states",
            "areas",
            "floors",
            "floorplan",
            "state",
            "entity",
        ];
        let mut unexplained = Vec::new();
        let mut read = 0;
        #[allow(unused_mut)]
        let mut all = sources.to_vec();
        #[cfg(feature = "assist")]
        all.push(assist);
        for (file, source) in all {
            let production = source.split("\nmod tests {").next().unwrap_or(source);
            let lines: Vec<&str> = production.lines().collect();
            for (at, line) in lines.iter().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                let asks = getters
                    .iter()
                    .any(|getter| line.contains(&format!("core.{getter}(")));
                if !asks {
                    continue;
                }
                read += 1;
                let said = at > 0 && lines[at - 1].contains("// whole-home:");
                if !said {
                    unexplained.push(format!("{file}:{}: {}", at + 1, line.trim()));
                }
            }
        }
        assert!(
            read >= 8,
            "read too little of the source ({read} calls found)"
        );
        assert!(
            unexplained.is_empty(),
            "these read the whole home without saying why; use `core.seen(&reach)`, or say why \
             with a `// whole-home:` comment on the line before: {unexplained:#?}"
        );
    }

    /// What an address must do for a person who has only some rooms.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Leak {
        /// Open to anyone who can reach Irori.
        Open,
        /// Answers with the home, and what it answers holds nothing outside their rooms.
        Filtered,
        /// One thing by its id: outside their rooms it answers as a thing that was never there.
        Hidden,
        /// The home as a whole, or the machine it runs on: for people who have all of it.
        Whole,
        /// How the home is set up: an owner's.
        Run,
        /// Somebody's own name and password, and nobody else's.
        Own,
        /// The live feed, which has a test of its own.
        Socket,
        /// A question to the assistant, which is answered from what they have of the home; it
        /// has a test of its own.
        Asks,
    }

    use Leak::{Asks, Filtered, Hidden, Open, Own, Run, Socket, Whole};

    /// Every address Irori answers under `/api/`, and what a person with rooms gets from it. A
    /// new address has to be put here, or `every_address_is_decided_for_a_person_with_rooms`
    /// fails: nothing is let through by being forgotten.
    const ADDRESSES: &[(&str, &str, Leak)] = &[
        ("GET", "/api/health", Open),
        ("GET", "/api/session", Open),
        ("POST", "/api/session", Open),
        ("DELETE", "/api/session", Open),
        ("POST", "/api/setup", Open),
        ("GET", "/api/users", Filtered),
        ("POST", "/api/users", Run),
        ("GET", "/api/audit", Run),
        ("PATCH", "/api/users/{id}", Own),
        ("DELETE", "/api/users/{id}", Run),
        ("GET", "/api/place", Whole),
        ("PUT", "/api/place", Run),
        ("GET", "/api/home", Filtered),
        ("POST", "/api/command", Hidden),
        ("GET", "/api/ws", Socket),
        ("GET", "/api/extension", Run),
        ("GET", "/api/tokens", Run),
        ("POST", "/api/tokens", Run),
        ("DELETE", "/api/tokens/{id}", Run),
        ("GET", "/api/devices", Filtered),
        ("GET", "/api/entities", Filtered),
        ("GET", "/api/states", Filtered),
        ("GET", "/api/history/{entity_id}/summary", Hidden),
        ("GET", "/api/history/{entity_id}", Hidden),
        ("GET", "/api/recorder", Whole),
        ("GET", "/api/network", Whole),
        ("PUT", "/api/recorder", Run),
        ("GET", "/api/system", Whole),
        ("GET", "/api/system/usage", Whole),
        ("GET", "/api/system/log", Run),
        ("GET", "/api/serial-ports", Whole),
        ("POST", "/api/restart", Run),
        ("GET", "/api/extensions", Filtered),
        ("GET", "/api/areas", Filtered),
        ("POST", "/api/areas", Run),
        ("GET", "/api/floors", Filtered),
        ("POST", "/api/floors", Run),
        ("PATCH", "/api/floors/{id}", Run),
        ("DELETE", "/api/floors/{id}", Run),
        ("PATCH", "/api/areas/{id}", Run),
        ("DELETE", "/api/areas/{id}", Run),
        ("GET", "/api/floorplan", Filtered),
        ("PUT", "/api/floorplan", Run),
        ("PATCH", "/api/devices/{id}", Run),
        ("DELETE", "/api/devices/{id}", Run),
        ("PATCH", "/api/entities/{id}", Run),
        ("GET", "/api/entities/{id}/picture", Hidden),
        ("PUT", "/api/extensions/{id}/secrets", Run),
        ("POST", "/api/extensions/{id}/settings", Run),
        ("POST", "/api/extensions/{id}/actions/{action_id}", Run),
        ("DELETE", "/api/extensions/{id}/actions/{action_id}", Run),
        ("GET", "/api/catalog", Whole),
        ("POST", "/api/extensions/{id}/install", Run),
        ("POST", "/api/extensions/install", Run),
        ("DELETE", "/api/extensions/{id}", Run),
        ("GET", "/api/helpers", Run),
        ("POST", "/api/helpers/{table}", Run),
        ("POST", "/api/helpers/{table}/check", Run),
        ("POST", "/api/helpers/{table}/preview", Run),
        ("PATCH", "/api/helpers/{table}/{id}", Run),
        ("DELETE", "/api/helpers/{table}/{id}", Run),
        ("GET", "/api/extensions/{id}/icon.svg", Open),
        ("GET", "/api/extensions/{id}/log", Run),
        ("GET", "/api/apps", Filtered),
        ("POST", "/api/apps/{id}/rpc", Hidden),
        ("GET", "/api/assistant", Filtered),
        ("PUT", "/api/assistant", Run),
        ("POST", "/api/assistant/turns", Asks),
        ("GET", "/api/assistant/turns/{scope}", Filtered),
        ("POST", "/api/assistant/turns/{scope}/stop", Asks),
        ("POST", "/api/assistant/check", Run),
        ("POST", "/api/assistant/pull", Run),
        ("POST", "/api/assistant/forget", Run),
        ("POST", "/api/assistant/install", Run),
        ("POST", "/api/assistant/uninstall", Run),
        ("GET", "/api/assistant/log", Run),
        ("POST", "/api/assistant/load", Run),
        ("POST", "/api/assistant/unload", Run),
        ("GET", "/api/assistant/transcript/{scope}", Filtered),
        ("DELETE", "/api/assistant/transcript/{scope}", Asks),
        ("GET", "/api/backups", Run),
        ("POST", "/api/backups", Run),
        ("POST", "/api/backups/key", Run),
        ("DELETE", "/api/backups/{name}", Run),
        ("POST", "/api/backups/upload", Run),
        ("PUT", "/api/backups/settings", Run),
        ("GET", "/api/backups/key", Run),
        ("POST", "/api/backups/restore/cancel", Run),
        ("POST", "/api/backups/restore/seen", Run),
        ("POST", "/api/backups/destinations/{place}/test", Run),
        ("GET", "/api/backups/destinations/{place}", Run),
        ("POST", "/api/backups/destinations/{place}/fetch", Run),
        ("GET", "/api/backups/{name}", Run),
        ("POST", "/api/backups/{name}/verify", Run),
        ("POST", "/api/backups/{name}/restore", Run),
        ("POST", "/api/setup/backup/upload", Open),
        ("POST", "/api/setup/backup/restore", Open),
        ("POST", "/api/setup/backup/verify", Open),
    ];

    /// The `/api/` addresses the router is built with, each with its method, read out of the
    /// source, so that an address (or a second method on one) somebody adds shows up here without
    /// anyone remembering to say so. The methods are the calls at the top of a route's arguments:
    /// `get(..).post(..)`, not whatever a handler's own code calls.
    fn routed_addresses() -> std::collections::BTreeSet<(String, String)> {
        let mut found = std::collections::BTreeSet::new();
        for source in [include_str!("server.rs"), include_str!("server/backups.rs")] {
            for after in source.split(".route(").skip(1) {
                let Some(rest) = after.trim_start().strip_prefix('"') else {
                    continue;
                };
                let Some(end) = rest.find('"') else {
                    continue;
                };
                let path = &rest[..end];
                if !path.starts_with("/api/") {
                    continue;
                }
                let (mut depth, mut word, mut in_string) = (0_u32, String::new(), false);
                for c in rest[end + 1..].chars() {
                    if in_string {
                        in_string = c != '"';
                        continue;
                    }
                    match c {
                        '"' => {
                            in_string = true;
                            word.clear();
                        }
                        '(' => {
                            if depth == 0
                                && matches!(
                                    word.as_str(),
                                    "get" | "post" | "put" | "patch" | "delete"
                                )
                            {
                                found.insert((word.to_uppercase(), path.to_owned()));
                            }
                            word.clear();
                            depth += 1;
                        }
                        ')' => {
                            if depth == 0 {
                                break;
                            }
                            depth -= 1;
                            word.clear();
                        }
                        c if c.is_alphanumeric() || c == '_' => word.push(c),
                        _ => word.clear(),
                    }
                }
            }
        }
        found
    }

    #[test]
    fn every_address_is_decided_for_a_person_with_rooms() {
        let routed = routed_addresses();
        assert!(
            routed.len() > 60,
            "read too little of the router: {routed:?}"
        );
        let decided: std::collections::BTreeSet<(String, String)> = ADDRESSES
            .iter()
            .map(|(method, path, _)| ((*method).to_owned(), (*path).to_owned()))
            .collect();
        let forgotten: Vec<_> = routed.difference(&decided).collect();
        assert!(
            forgotten.is_empty(),
            "these addresses are served and nobody has said what a person with rooms gets from \
             them; add them to ADDRESSES: {forgotten:?}"
        );
        let gone: Vec<_> = decided.difference(&routed).collect();
        assert!(
            gone.is_empty(),
            "these are in ADDRESSES and no longer served: {gone:?}"
        );
    }

    /// A home with a hall and a vault in the cellar, a lamp in the hall and a camera in the vault,
    /// an owner, Leo who has the hall, and Mia who is a regular user with the whole home.
    struct House {
        server: Server,
        _host: irori_core::ExtensionHost,
        app: Router,
        owner: String,
        leo: String,
        mia: String,
        rooms: Vec<String>,
        floors: Vec<String>,
        camera_device: String,
        lamp_entities: Vec<String>,
        /// Every id Leo has no room for, and the owner's own.
        hidden: Vec<String>,
        entities: serde_json::Value,
    }

    const CAMERA: &str = "camera.garden_camera_1";
    const LAMP: &str = "demo_living_room_lamp";

    async fn house_with_a_vault() -> anyhow::Result<House> {
        let core = core();
        let host = irori_core::ExtensionHost::start(
            &core,
            vec![
                irori_protocol::builtin::<irori_protocol_demo::Demo>()
                    .map_err(anyhow::Error::msg)?,
                irori_protocol::builtin::<crate::system_device::IroriDevice>()
                    .map_err(anyhow::Error::msg)?,
            ],
            irori_core::Timing::default(),
        )
        .map_err(anyhow::Error::msg)?;
        for _ in 0..500 {
            let has = |id: &str| core.entities().iter().any(|e| e.id.to_string() == id);
            if has("update.irori") && has("camera.garden_camera_1") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let server = Server::new(core.clone())?;
        let (app, owner) = locked_home(&server).await?;
        let call = |method: &str, path: &str, body: Option<serde_json::Value>, cookie: &str| {
            let (app, method, path, cookie) = (
                app.clone(),
                method.to_owned(),
                path.to_owned(),
                cookie.to_owned(),
            );
            async move { ask(&app, &method, &path, body, Some(&cookie), true).await }
        };

        // A home with two floors and two rooms, a lamp in the hall and a camera in the vault.
        let mut floors = Vec::new();
        for (name, level) in [("Zqground", 0), ("Zqcellar", -1)] {
            let (status, made, _) = call(
                "POST",
                "/api/floors",
                Some(serde_json::json!({ "name": name, "level": level })),
                &owner,
            )
            .await?;
            assert_eq!(status, StatusCode::CREATED, "{made}");
            floors.push(made["id"].as_str().unwrap_or_default().to_owned());
        }
        let mut rooms = Vec::new();
        for (name, floor) in [("Hall", &floors[0]), ("Zqvault", &floors[1])] {
            let (status, made, _) = call(
                "POST",
                "/api/areas",
                Some(serde_json::json!({ "name": name, "floor": floor })),
                &owner,
            )
            .await?;
            assert_eq!(status, StatusCode::CREATED, "{made}");
            rooms.push(made["id"].as_str().unwrap_or_default().to_owned());
        }
        let (_, entities, _) = call("GET", "/api/entities", None, &owner).await?;
        let camera = "camera.garden_camera_1";
        let camera_device = entities
            .as_array()
            .and_then(|all| all.iter().find(|entity| entity["id"] == camera))
            .and_then(|entity| entity["device_id"].as_str())
            .unwrap_or_default()
            .to_owned();
        let lamp_device = "demo_living_room_lamp";
        for (device, room, note) in [
            (lamp_device, &rooms[0], None),
            (camera_device.as_str(), &rooms[1], Some("Zqsecret note")),
        ] {
            let mut edit = serde_json::json!({ "area": room });
            if let Some(note) = note {
                edit["description"] = note.into();
            }
            let (status, why, _) = call(
                "PATCH",
                &format!("/api/devices/{device}"),
                Some(edit),
                &owner,
            )
            .await?;
            assert_eq!(status, StatusCode::OK, "{device}: {why}");
        }
        let plan = serde_json::json!({
            "floors": {
                floors[0].clone(): {
                    "walls": [{ "from": [0, 0], "to": [400, 0] }],
                    "areas": [{ "area": rooms[0], "points": [[0, 0], [400, 0], [400, 300]] }],
                    "devices": [{ "device": lamp_device, "at": [100, 50] }],
                },
                floors[1].clone(): {
                    "walls": [{ "from": [0, 0], "to": [90, 0] }],
                    "areas": [{ "area": rooms[1], "points": [[0, 0], [90, 0], [90, 90]] }],
                    "devices": [{ "device": camera_device, "at": [10, 5] }],
                },
            },
        });
        let (status, why, _) = call("PUT", "/api/floorplan", Some(plan), &owner).await?;
        assert_eq!(status, StatusCode::OK, "{why}");

        // Leo has the hall. Mia has the whole home, and is a regular user all the same.
        let (status, why, _) = call(
            "POST",
            "/api/users",
            Some(serde_json::json!({
                "name": "Leo", "password": "let me in!", "areas": [rooms[0]],
            })),
            &owner,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{why}");
        let (status, why, leo) = ask(
            &app,
            "POST",
            "/api/session",
            Some(serde_json::json!({ "user": "Leo", "password": "let me in!" })),
            None,
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{why}");
        let leo = leo.ok_or_else(|| anyhow::anyhow!("a cookie"))?;
        let mia = person(&app, &owner, "Mia", "user").await?;

        // What is not Leo's: every entity and device he has no room for.
        let (_, devices, _) = call("GET", "/api/devices", None, &owner).await?;
        let ids = |list: &serde_json::Value, key: &str| -> Vec<String> {
            list.as_array()
                .map(|all| {
                    all.iter()
                        .filter_map(|item| item[key].as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        let lamp_entities: Vec<String> = entities
            .as_array()
            .map(|all| {
                all.iter()
                    .filter(|entity| entity["device_id"] == lamp_device)
                    .filter_map(|entity| entity["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        assert!(!lamp_entities.is_empty(), "the lamp has entities");
        let mut hidden: Vec<String> = ids(&entities, "id")
            .into_iter()
            .chain(ids(&devices, "id"))
            .filter(|id| !lamp_entities.contains(id) && id != lamp_device)
            .collect();
        hidden.push("nico".to_owned());
        assert!(hidden.contains(&camera.to_owned()));
        Ok(House {
            server,
            _host: host,
            app,
            owner,
            leo,
            mia,
            rooms,
            floors,
            camera_device,
            lamp_entities,
            hidden,
            entities,
        })
    }

    /// Whatever isn't in a person's rooms isn't there for them: not in a list, not by its id, not
    /// by a message that names it, not by the difference between "no" and "not found".
    #[tokio::test]
    async fn a_person_with_rooms_finds_nothing_of_the_rest_of_the_home() -> anyhow::Result<()> {
        let House {
            server: _server,
            _host,
            app,
            owner,
            leo,
            mia,
            rooms,
            floors,
            lamp_entities,
            hidden,
            ..
        } = house_with_a_vault().await?;
        let camera = CAMERA;
        let lamp_device = LAMP;
        let call = |method: &str, path: &str, body: Option<serde_json::Value>, cookie: &str| {
            let (app, method, path, cookie) = (
                app.clone(),
                method.to_owned(),
                path.to_owned(),
                cookie.to_owned(),
            );
            async move { ask(&app, &method, &path, body, Some(&cookie), true).await }
        };
        let ids = |list: &serde_json::Value, key: &str| -> Vec<String> {
            list.as_array()
                .map(|all| {
                    all.iter()
                        .filter_map(|item| item[key].as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        let words = ["Zqvault", "Zqcellar", "Zqsecret", "Nico"];
        let holds_nothing_hidden = |what: &str, value: &serde_json::Value| {
            let text = value.to_string();
            for id in &hidden {
                assert!(
                    !text.contains(&format!("\"{id}\"")),
                    "{what} shows `{id}` to a person who has no room for it: {text}"
                );
            }
            for word in words {
                assert!(
                    !text.contains(word),
                    "{what} shows `{word}` to a person who has no room for it: {text}"
                );
            }
        };

        for (method, pattern, kind) in ADDRESSES {
            let path = pattern
                .replace("{scope}", "general")
                .replace("{entity_id}", lamp_entities[0].as_str())
                .replace("{id}", "zq")
                .replace("{action_id}", "zq")
                .replace("{table}", "zq")
                .replace("{place}", "zq")
                .replace("{name}", "zq");
            let what = format!("{method} {pattern}");
            // Without the assistant there is nothing at its addresses to answer.
            if !cfg!(feature = "assist") && path.starts_with("/api/assistant") && *kind == Filtered
            {
                continue;
            }
            match kind {
                Open | Socket | Asks => {}
                Filtered => {
                    let (status, got, _) = call(method, &path, None, &leo).await?;
                    assert_eq!(status, StatusCode::OK, "{what}: {got}");
                    holds_nothing_hidden(&what, &got);
                }
                Run => {
                    let (status, got, _) = call(method, &path, None, &leo).await?;
                    // The socket an extension dials in on wants its own token, whoever asks.
                    let refusal = if *pattern == "/api/extension" {
                        StatusCode::UNAUTHORIZED
                    } else {
                        StatusCode::FORBIDDEN
                    };
                    assert_eq!(status, refusal, "{what}: {got}");
                }
                Whole => {
                    let (status, got, _) = call(method, &path, None, &leo).await?;
                    assert_eq!(status, StatusCode::FORBIDDEN, "{what}: {got}");
                    // And a regular user who has the whole home is not shut out of it.
                    let (status, got, _) = call(method, &path, None, &mia).await?;
                    assert_ne!(status, StatusCode::FORBIDDEN, "{what}: {got}");
                }
                // A thing by its id: what it answers for one that is his must hold nothing of
                // the rest either. (What it answers for one that is not his is checked below.)
                Hidden if *method == "GET" => {
                    let (status, got, _) = call(method, &path, None, &leo).await?;
                    if status.is_success() {
                        holds_nothing_hidden(&what, &got);
                    }
                }
                // His own name and password: the answer is a list of people, and he is told
                // of himself alone, even when what he sent changed nothing. A name is not
                // his to change, and the refusal must not say whether it is taken.
                Own => {
                    let (status, got, _) =
                        call(method, "/api/users/leo", Some(serde_json::json!({})), &leo).await?;
                    assert_eq!(status, StatusCode::OK, "{what}: {got}");
                    holds_nothing_hidden(&what, &got);
                    assert_eq!(ids(&got, "id"), ["leo"], "{what}: {got}");
                    for name in ["Mia", "Leonard"] {
                        let (status, got, _) = call(
                            method,
                            "/api/users/leo",
                            Some(serde_json::json!({ "name": name })),
                            &leo,
                        )
                        .await?;
                        assert_eq!(status, StatusCode::FORBIDDEN, "{what}: {got}");
                        assert!(
                            !got.to_string().contains("already"),
                            "{what}: a refusal that says a name is taken: {got}"
                        );
                    }
                }
                Hidden => {}
            }
        }

        // What Leo is shown is what is in the hall, and not an empty answer that is safe because
        // it is empty.
        let (_, seen, _) = call("GET", "/api/home", None, &leo).await?;
        assert_eq!(ids(&seen["devices"], "id"), [lamp_device]);
        let mut seen_entities = ids(&seen["entities"], "id");
        seen_entities.sort();
        let mut want = lamp_entities.clone();
        want.sort();
        assert_eq!(seen_entities, want);
        assert_eq!(ids(&seen["states"], "entity_id").len(), want.len());
        assert_eq!(ids(&seen["areas"], "id"), [rooms[0].as_str()]);
        assert_eq!(ids(&seen["floors"], "id"), [floors[0].as_str()]);
        assert_eq!(
            seen["floorplan"]["floors"]
                .as_object()
                .map(|all| all.keys().cloned().collect::<Vec<_>>()),
            Some(vec![floors[0].clone()])
        );
        assert_eq!(
            seen["floorplan"]["floors"][floors[0].as_str()]["devices"][0]["device"],
            lamp_device
        );
        assert_eq!(seen["held"], serde_json::Value::Null);
        assert_eq!(seen["installs"], serde_json::Value::Null);
        assert_eq!(seen["extensions"], serde_json::json!({}));
        let (_, people, _) = call("GET", "/api/users", None, &leo).await?;
        assert_eq!(
            ids(&people, "id"),
            ["leo"],
            "a person is told of themselves"
        );

        // By its id, a thing that isn't Leo's answers as one that was never there.
        let unknown = |id: &str| id.replace("garden_camera_1", "zq_nothing_here");
        for (method, pattern) in [
            ("GET", "/api/history/{entity_id}"),
            (
                "GET",
                "/api/history/{entity_id}/summary?since=2020-01-01T00:00:00Z",
            ),
            ("GET", "/api/entities/{entity_id}/picture"),
        ] {
            let (status, body, _) =
                call(method, &pattern.replace("{entity_id}", camera), None, &leo).await?;
            let (gone_status, gone_body, _) = call(
                method,
                &pattern.replace("{entity_id}", &unknown(camera)),
                None,
                &leo,
            )
            .await?;
            assert_eq!(status, StatusCode::NOT_FOUND, "{pattern}: {body}");
            assert_eq!(status, gone_status, "{pattern}");
            assert_eq!(
                body.to_string().replace(camera, "ID"),
                gone_body.to_string().replace(&unknown(camera), "ID"),
                "{pattern}: a hidden camera answers in words a missing one would not"
            );
            // The owner, and a person who has the room, do get it.
            let (status, _, _) = call(
                method,
                &pattern.replace("{entity_id}", camera),
                None,
                &owner,
            )
            .await?;
            assert_ne!(status, StatusCode::NOT_FOUND, "{pattern} for the owner");
        }
        let (status, _, _) = call(
            "GET",
            &format!("/api/history/{}", lamp_entities[0]),
            None,
            &leo,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "the lamp's history is Leo's");

        // A command to a thing that isn't his is 404, and says the same for an update (which is
        // an owner's, and so would otherwise answer 403 for a thing that exists).
        for hidden_id in [camera, "update.irori"] {
            let gone_id = unknown(hidden_id).replace("irori", "zq_nothing_here");
            let ask_for =
                |entity: &str| serde_json::json!({ "entity_id": entity, "command": "turn_on" });
            let (status, body, _) =
                call("POST", "/api/command", Some(ask_for(hidden_id)), &leo).await?;
            let (gone_status, gone_body, _) =
                call("POST", "/api/command", Some(ask_for(&gone_id)), &leo).await?;
            assert_eq!(status, StatusCode::NOT_FOUND, "{hidden_id}: {body}");
            assert_eq!(status, gone_status);
            assert_eq!(
                body.to_string().replace(hidden_id, "ID"),
                gone_body.to_string().replace(&gone_id, "ID"),
                "{hidden_id}"
            );
        }
        let (status, body, _) = call(
            "POST",
            "/api/command",
            Some(serde_json::json!({ "entity_id": lamp_entities[0], "command": "turn_on" })),
            &leo,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "a lamp in his room is his: {body}");

        // Somebody else's name and password: the same answer for a person who is there and one
        // who isn't.
        let mut answers = Vec::new();
        for other in ["nico", "zq_nobody"] {
            let (status, body, _) = call(
                "PATCH",
                &format!("/api/users/{other}"),
                Some(serde_json::json!({ "name": "Taken" })),
                &leo,
            )
            .await?;
            answers.push((status, body));
        }
        assert_eq!(answers[0], answers[1]);
        assert_eq!(answers[0].0, StatusCode::FORBIDDEN);
        Ok(())
    }

    #[cfg(feature = "assist")]
    /// A model that does what the question says: `call: {"name": .., "arguments": {..}}` reaches
    /// for that tool and reports what it was told; anything else is answered with the request
    /// as the model was sent it, so that a test can read what it was shown.
    async fn echoing_model() -> String {
        let app = Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(|Json(body): Json<serde_json::Value>| async move {
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let told = messages
                    .iter()
                    .rev()
                    .find(|message| message["role"] == "tool")
                    .and_then(|message| message["content"].as_str().map(str::to_owned));
                let asked = messages
                    .iter()
                    .rev()
                    .find(|message| message["role"] == "user")
                    .and_then(|message| message["content"].as_str().map(str::to_owned))
                    .unwrap_or_default();
                let line = match (told, asked.strip_prefix("call:")) {
                    (Some(told), _) => {
                        serde_json::json!({"choices": [{"delta": {"content": format!("Told: {told}")}}]})
                    }
                    (None, Some(call)) => {
                        let call: serde_json::Value =
                            serde_json::from_str(call).unwrap_or_default();
                        serde_json::json!({"choices": [{"delta": {"tool_calls": [{
                            "index": 0, "id": "a", "function": {
                                "name": call["name"],
                                "arguments": call["arguments"].to_string(),
                            },
                        }]}}]})
                    }
                    (None, None) => {
                        serde_json::json!({"choices": [{"delta": {"content": body.to_string()}}]})
                    }
                };
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    format!("data: {line}\n\ndata: [DONE]\n\n"),
                )
                    .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a local port");
        let address = listener.local_addr().expect("the port it bound");
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{address}/v1")
    }

    /// A question to the assistant as somebody signed in, and everything it streamed back, joined.
    #[cfg(feature = "assist")]
    async fn asked(
        app: &Router,
        cookie: &str,
        scope: &str,
        message: &str,
    ) -> anyhow::Result<String> {
        let request = Request::post("/api/assistant/turns")
            .header("cookie", cookie)
            .header(UI_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&serde_json::json!({
                "scope": scope,
                "message": message,
            }))?))?;
        let response = app.clone().oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await?.to_bytes();
        let mut said = String::new();
        for line in String::from_utf8(bytes.to_vec())?.lines() {
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let event: serde_json::Value = serde_json::from_str(data.trim())?;
            for key in ["delta", "error"] {
                if let Some(text) = event[key].as_str() {
                    said.push_str(text);
                }
            }
        }
        Ok(said)
    }

    /// Whether `text` names `id` (and not a longer id that starts the same).
    #[cfg(feature = "assist")]
    fn names(text: &str, id: &str) -> bool {
        text.match_indices(id).any(|(at, _)| {
            !text[at + id.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    }

    /// The assistant answers a person with some rooms from those rooms: what the model is told,
    /// what its tools find, what is kept of the chat, and what the owner has set up stay out of
    /// their reach (`docs/specs/access.md` §2).
    #[cfg(feature = "assist")]
    #[tokio::test]
    async fn the_assistant_of_a_person_with_rooms_knows_only_their_rooms() -> anyhow::Result<()> {
        let House {
            server: _server,
            _host,
            app,
            owner,
            leo,
            mia,
            lamp_entities,
            hidden,
            camera_device,
            rooms,
            ..
        } = house_with_a_vault().await?;
        let call = |method: &str, path: &str, body: Option<serde_json::Value>, cookie: &str| {
            let (app, method, path, cookie) = (
                app.clone(),
                method.to_owned(),
                path.to_owned(),
                cookie.to_owned(),
            );
            async move { ask(&app, &method, &path, body, Some(&cookie), true).await }
        };
        let base = echoing_model().await;
        let (status, set, _) = call(
            "PUT",
            "/api/assistant",
            Some(serde_json::json!({
                "mode": "cloud", "preset": "compatible", "base_url": base,
                "model": "echo", "api_key": "sk-test-key-should-not-leak",
                // The owner's standing notes may name what a person with rooms has no room for.
                "instructions": "Never mention the Zqsecret freezer to the guests.",
            })),
            &owner,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{set}");
        let secret_ids: Vec<&String> = hidden
            .iter()
            .filter(|id| id.contains('.') || id.contains('_'))
            .collect();
        let nothing_hidden = |what: &str, text: &str| {
            for id in &secret_ids {
                assert!(!names(text, id), "{what} names `{id}`: {text}");
            }
            for word in ["Zqvault", "Zqcellar", "Zqsecret"] {
                assert!(!text.contains(word), "{what} says `{word}`: {text}");
            }
        };

        // What the model is shown: the owner's whole home, Leo's hall and nothing more.
        let for_owner = asked(&app, &owner, "general", "What is on?").await?;
        assert!(names(&for_owner, CAMERA), "the owner is shown the camera");
        assert!(
            for_owner.contains("Zqsecret"),
            "the owner's own notes are in the owner's prompt"
        );
        assert!(for_owner.contains("read_settings") && for_owner.contains("read_logs"));
        let for_leo = asked(&app, &leo, "general", "What is on?").await?;
        assert!(names(&for_leo, &lamp_entities[0]), "Leo is shown his lamp");
        nothing_hidden("what Leo's model is told", &for_leo);
        for setup in [
            "read_settings",
            "read_logs",
            "list_helpers",
            "propose_helper",
        ] {
            assert!(!for_leo.contains(setup), "Leo's model is offered `{setup}`");
        }
        assert!(for_leo.contains("list_devices"));
        // And the chat of a device he has no room for is not there to open.
        let gone = asked(&app, &leo, &format!("device:{camera_device}"), "Hi").await?;
        let never = asked(&app, &leo, "device:zq_never_was", "Hi").await?;
        assert_eq!(
            gone.replace(&camera_device, "ID"),
            never.replace("zq_never_was", "ID")
        );

        // What its tools find.
        let tool = |name: &str, arguments: serde_json::Value| {
            format!(
                "call: {}",
                serde_json::json!({ "name": name, "arguments": arguments })
            )
        };
        let entities = asked(
            &app,
            &leo,
            "general",
            &tool("list_entities", serde_json::json!({})),
        )
        .await?;
        assert!(names(&entities, &lamp_entities[0]), "{entities}");
        nothing_hidden("list_entities", &entities);
        let owners_entities = asked(
            &app,
            &owner,
            "general",
            &tool("list_entities", serde_json::json!({})),
        )
        .await?;
        assert!(names(&owners_entities, CAMERA));
        let plan = asked(
            &app,
            &leo,
            "general",
            &tool("read_floorplan", serde_json::json!({})),
        )
        .await?;
        nothing_hidden("read_floorplan", &plan);
        let device = asked(
            &app,
            &leo,
            "general",
            &tool("get_device", serde_json::json!({ "id": camera_device })),
        )
        .await?;
        let no_device = asked(
            &app,
            &leo,
            "general",
            &tool("get_device", serde_json::json!({ "id": "zq_never_was" })),
        )
        .await?;
        assert_eq!(
            device.replace(&camera_device, "ID"),
            no_device.replace("zq_never_was", "ID"),
            "a device he has no room for is answered as one that was never there"
        );
        let history = asked(
            &app,
            &leo,
            "general",
            &tool("recent_states", serde_json::json!({ "entity_id": CAMERA })),
        )
        .await?;
        let no_history = asked(
            &app,
            &leo,
            "general",
            &tool(
                "recent_states",
                serde_json::json!({ "entity_id": "camera.zq_never_was" }),
            ),
        )
        .await?;
        assert_eq!(
            history.replace(CAMERA, "ID"),
            no_history.replace("camera.zq_never_was", "ID")
        );
        // The log holds everybody's sign-ins, and a model is not a way round that, whoever asks
        // it, an owner excepted.
        let log = tool("read_logs", serde_json::json!({ "source": "irori" }));
        for (who, cookie) in [("Leo", &leo), ("Mia", &mia)] {
            let told = asked(&app, cookie, "general", &log).await?;
            assert!(
                told.contains("there is no tool `read_logs`"),
                "{who}: {told}"
            );
        }
        let told = asked(&app, &owner, "general", &log).await?;
        assert!(
            !told.contains("there is no tool"),
            "the owner reads the log: {told}"
        );

        // How the home is set up is an owner's, in the chats as it is in Settings.
        for (who, cookie) in [("Leo", &leo), ("Mia", &mia)] {
            for scope in ["settings", "helpers"] {
                let told = asked(&app, cookie, scope, "Hi").await?;
                assert_eq!(told, "that conversation doesn't exist", "{who} {scope}");
                let (status, _, _) = call(
                    "GET",
                    &format!("/api/assistant/transcript/{scope}"),
                    None,
                    cookie,
                )
                .await?;
                assert_eq!(status, StatusCode::BAD_REQUEST, "{who} {scope}");
            }
        }
        let told = asked(&app, &owner, "settings", "Hi").await?;
        assert!(
            told.contains("read_logs"),
            "the owner's Settings chat: {told}"
        );

        // A chat is the person's own when they have only some rooms: nobody else reads it, and
        // clearing it clears theirs.
        let transcript = |cookie: String| {
            let call = &call;
            async move {
                let (_, got, _) =
                    call("GET", "/api/assistant/transcript/general", None, &cookie).await?;
                anyhow::Ok(got["turns"].to_string())
            }
        };
        let owners = transcript(owner.clone()).await?;
        let leos = transcript(leo.clone()).await?;
        let mias = transcript(mia.clone()).await?;
        assert!(leos.contains("What is on?"));
        assert!(owners.contains("What is on?"), "the owner's own chat");
        assert!(
            owners.contains("read_settings") && !leos.contains("read_settings"),
            "Leo's chat is not the owner's"
        );
        // Mia has the whole home and is not an owner: what the model told the owner, from
        // Settings and the logs, is not hers to read, clear or stop. Her chat is her own.
        assert!(mias.contains("read_logs"), "Mia's own chat: {mias}");
        assert!(
            !mias.contains("read_settings"),
            "the owner's chat is not Mia's"
        );
        let (status, _, _) =
            call("DELETE", "/api/assistant/transcript/general", None, &mia).await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(transcript(mia.clone()).await?, "[]");
        assert_eq!(
            transcript(owner.clone()).await?,
            owners,
            "clearing Mia's chat left the owner's"
        );
        let (_, mias_view, _) = call("GET", "/api/assistant", None, &mia).await?;
        assert!(mias_view["base_url"].as_str().is_none_or(str::is_empty));
        let (status, _, _) =
            call("DELETE", "/api/assistant/transcript/general", None, &leo).await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(transcript(leo.clone()).await?, "[]");
        assert_eq!(
            transcript(owner.clone()).await?,
            owners,
            "clearing Leo's chat left the owner's"
        );

        // Asking for it by its key is asking for a conversation that isn't one.
        let (status, _, _) = call(
            "GET",
            "/api/assistant/transcript/general%23leo",
            None,
            &owner,
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // What Leo is told of how it is set up is whether he can ask, and not how.
        let (status, seen, _) = call("GET", "/api/assistant", None, &leo).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(seen["ready"], true);
        for field in [
            "base_url",
            "credential",
            "cloud_model",
            "local_tag",
            "instructions",
        ] {
            assert!(
                seen[field].as_str().is_none_or(str::is_empty),
                "{field}: {seen}"
            );
        }
        assert_eq!(seen["pulled"], serde_json::json!([]));
        assert!(!seen.to_string().contains("sk-test-key"));
        let (_, owners_view, _) = call("GET", "/api/assistant", None, &owner).await?;
        assert_eq!(owners_view["base_url"], base);

        // What was said about a room is forgotten once it is not theirs, however that came
        // about: here the owner moves the lamp out of the hall. Gaining a room loses nothing.
        let (_, _, _) = call("POST", "/api/assistant/turns/general/stop", None, &leo).await?;
        asked(&app, &leo, "general", "Again?").await?;
        assert!(transcript(leo.clone()).await?.contains("Again?"));
        let move_to = |device: String, room: String| {
            let call = &call;
            let owner = owner.clone();
            async move {
                let (status, why, _) = call(
                    "PATCH",
                    &format!("/api/devices/{device}"),
                    Some(serde_json::json!({ "area": room })),
                    &owner,
                )
                .await?;
                assert_eq!(status, StatusCode::OK, "{why}");
                anyhow::Ok(())
            }
        };
        move_to(LAMP.to_owned(), rooms[1].clone()).await?;
        assert_eq!(
            transcript(leo.clone()).await?,
            "[]",
            "the lamp left the hall"
        );
        move_to(LAMP.to_owned(), rooms[0].clone()).await?;
        asked(&app, &leo, "general", "After?").await?;
        assert!(transcript(leo.clone()).await?.contains("After?"));
        move_to(camera_device.clone(), rooms[0].clone()).await?;
        assert!(
            transcript(leo.clone()).await?.contains("After?"),
            "a room gained takes nothing away"
        );
        // A room with nothing in it is theirs too: losing it forgets the chat all the same,
        // though no entity or device went anywhere.
        let (status, empty, _) = call(
            "POST",
            "/api/areas",
            Some(serde_json::json!({ "name": "Zqempty" })),
            &owner,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{empty}");
        let empty = empty["id"].as_str().unwrap_or_default().to_owned();
        let (status, why, _) = call(
            "PATCH",
            "/api/users/leo",
            Some(serde_json::json!({ "areas": [rooms[0], empty] })),
            &owner,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{why}");
        asked(&app, &leo, "general", "Empty?").await?;
        assert!(transcript(leo.clone()).await?.contains("Empty?"));
        let (status, why, _) = call("DELETE", &format!("/api/areas/{empty}"), None, &owner).await?;
        assert_eq!(status, StatusCode::NO_CONTENT, "{why}");
        assert_eq!(
            transcript(leo.clone()).await?,
            "[]",
            "an empty room is theirs, and was taken from them"
        );
        // Changing the person's rooms forgets them at once.
        asked(&app, &leo, "general", "Before?").await?;
        let (status, why, _) = call(
            "PATCH",
            "/api/users/leo",
            Some(serde_json::json!({ "areas": [] })),
            &owner,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{why}");
        assert_eq!(transcript(leo.clone()).await?, "[]");
        assert_eq!(
            transcript(owner.clone()).await?,
            owners,
            "and the owner's is left"
        );
        Ok(())
    }

    /// What a person with rooms is sent on the live feed is theirs: the home as they have it,
    /// the changes of what is in their rooms, and nothing of what is in the others. A thing moved
    /// out of their room stops being sent; their rooms changing ends the feed, and the next one
    /// is the new view (`docs/specs/access.md` §2).
    #[tokio::test]
    async fn the_live_feed_of_a_person_with_rooms_is_theirs_alone() -> anyhow::Result<()> {
        use futures_util::StreamExt as _;
        use tokio_tungstenite::tungstenite::Message;

        let House {
            server: _server,
            _host,
            app,
            owner,
            leo,
            rooms,
            lamp_entities,
            hidden,
            entities,
            camera_device,
            ..
        } = house_with_a_vault().await?;
        let address = listen(app.clone()).await?;
        let mut feed = open_live(address, &[("cookie", leo.as_str())]).await?;

        // Frames until the feed has been quiet for a moment.
        async fn drain(
            feed: &mut tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        ) -> anyhow::Result<Vec<String>> {
            let mut frames = Vec::new();
            while let Ok(Some(frame)) =
                tokio::time::timeout(Duration::from_millis(700), feed.next()).await
            {
                if let Message::Text(text) = frame? {
                    frames.push(text.to_string());
                }
            }
            Ok(frames)
        }
        let nothing_hidden = |frames: &[String]| {
            for frame in frames {
                for id in &hidden {
                    assert!(
                        !frame.contains(&format!("\"{id}\"")),
                        "the feed told a person with rooms of `{id}`: {frame}"
                    );
                }
            }
        };
        let frames = drain(&mut feed).await?;
        assert!(frames[0].contains("\"type\":\"snapshot\""), "{}", frames[0]);
        assert!(frames[0].contains(&format!("\"{}\"", lamp_entities[0])));
        nothing_hidden(&frames);

        // The owner switches a light Leo has no room for, and then his lamp.
        let elsewhere = entities
            .as_array()
            .and_then(|all| {
                all.iter()
                    .filter_map(|entity| entity["id"].as_str())
                    .find(|id| id.starts_with("light.") && !lamp_entities.iter().any(|l| l == id))
            })
            .ok_or_else(|| anyhow::anyhow!("the demo has another light"))?
            .to_owned();
        let toggle = |entity: String| {
            let (app, owner) = (app.clone(), owner.clone());
            async move {
                let (status, why, _) = ask(
                    &app,
                    "POST",
                    "/api/command",
                    Some(serde_json::json!({ "entity_id": entity, "command": "toggle" })),
                    Some(&owner),
                    true,
                )
                .await?;
                assert_eq!(status, StatusCode::OK, "{why}");
                anyhow::Ok(())
            }
        };
        toggle(elsewhere).await?;
        toggle(lamp_entities[0].clone()).await?;
        let frames = drain(&mut feed).await?;
        nothing_hidden(&frames);
        assert!(
            frames
                .iter()
                .any(|frame| frame.contains("\"type\":\"state\"")
                    && frame.contains(&lamp_entities[0])),
            "his own lamp is reported: {frames:?}"
        );

        // The lamp moves to the vault. He is told the home changed, and then not told of it.
        let (status, why, _) = ask(
            &app,
            "PATCH",
            &format!("/api/devices/{LAMP}"),
            Some(serde_json::json!({ "area": rooms[1] })),
            Some(&owner),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{why}");
        let frames = drain(&mut feed).await?;
        assert!(
            frames
                .iter()
                .any(|frame| frame.contains("\"type\":\"changed\"")),
            "the page is told to read the home again: {frames:?}"
        );
        toggle(lamp_entities[0].clone()).await?;
        let frames = drain(&mut feed).await?;
        assert!(
            !frames.iter().any(|frame| frame.contains(&lamp_entities[0])),
            "a lamp that has left his room is not reported to him: {frames:?}"
        );

        // His rooms change: the feed ends, and the next one has the vault in it.
        let (status, why, _) = ask(
            &app,
            "PATCH",
            "/api/users/leo",
            Some(serde_json::json!({ "areas": [rooms[0], rooms[1]] })),
            Some(&owner),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{why}");
        let closed = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(Ok(frame)) = feed.next().await {
                if matches!(frame, Message::Close(_)) {
                    return;
                }
            }
        })
        .await;
        assert!(closed.is_ok(), "his feed should end when his rooms change");
        let mut feed = open_live(address, &[("cookie", leo.as_str())]).await?;
        let frames = drain(&mut feed).await?;
        assert!(frames[0].contains(&camera_device), "{}", frames[0]);

        // His password is taken out of the file by hand: the feed that was opened with it ends,
        // though nothing else about him changed.
        let file = _server.config_dir().join("secrets.toml");
        let text = std::fs::read_to_string(&file)?;
        let without: String = text
            .lines()
            .filter(|line| !line.trim_start().starts_with("leo ="))
            .map(|line| format!("{line}\n"))
            .collect();
        assert_ne!(text, without, "Leo had a password in the file");
        std::fs::write(&file, without)?;
        _server.config.home().await;
        let closed = tokio::time::timeout(Duration::from_secs(8), async {
            while let Some(Ok(frame)) = feed.next().await {
                if matches!(frame, Message::Close(_)) {
                    return;
                }
            }
        })
        .await;
        assert!(closed.is_ok(), "his feed should end when his password goes");
        Ok(())
    }
    // ---- who did what (`audit.rs`, `server/trail.rs`) ---------------------------------------

    /// The log as an owner reads it, once the writer has caught up with what `wanted` looks for.
    async fn log_of(
        app: &Router,
        owner: &str,
        wanted: impl Fn(&[serde_json::Value]) -> bool,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        for _ in 0..300 {
            let (status, page, _) = ask(
                app,
                "GET",
                "/api/audit?limit=1000",
                None,
                Some(owner),
                false,
            )
            .await?;
            assert_eq!(status, StatusCode::OK, "{page}");
            let rows = page["entries"].as_array().cloned().unwrap_or_default();
            if wanted(&rows) {
                return Ok(rows);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        anyhow::bail!("the activity log never held what was looked for")
    }

    /// The newest row with this action by this person (or token) name.
    fn row_by<'a>(
        rows: &'a [serde_json::Value],
        action: &str,
        who: &str,
    ) -> Option<&'a serde_json::Value> {
        rows.iter()
            .find(|row| row["action"] == action && row["actor"]["name"] == who)
    }

    /// Everything a person does that changes the home leaves a row with who, what and whether it
    /// was allowed; a refusal is a row too; only an owner reads them; and none of them holds a
    /// password (`docs/specs/access.md` §3).
    #[tokio::test]
    async fn what_people_do_is_written_down_and_only_an_owner_reads_it() -> anyhow::Result<()> {
        let House {
            server: _server,
            _host,
            app,
            owner,
            leo,
            mia,
            rooms,
            lamp_entities,
            ..
        } = house_with_a_vault().await?;
        let lamp = lamp_entities
            .iter()
            .find(|id| id.starts_with("light."))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the lamp has a light"))?;

        // Leo turns the lamp on, asks for the camera he has no room for, and tries to add a room.
        let command =
            |entity: &str, name: &str| serde_json::json!({ "entity_id": entity, "command": name });
        let (status, why, _) = ask(
            &app,
            "POST",
            "/api/command",
            Some(command(&lamp, "turn_on")),
            Some(&leo),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{why}");
        let (status, _, _) = ask(
            &app,
            "POST",
            "/api/command",
            Some(command(CAMERA, "toggle")),
            Some(&leo),
            true,
        )
        .await?;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "answered as if it was never there"
        );
        let (status, _, _) = ask(
            &app,
            "POST",
            "/api/areas",
            Some(serde_json::json!({ "name": "Attic" })),
            Some(&leo),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // Mia has the whole home and is not an owner: an update is the owner's.
        let (status, _, _) = ask(
            &app,
            "POST",
            "/api/command",
            Some(command("update.irori", "install")),
            Some(&mia),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // Somebody who is nobody types a wrong password for Leo, and one for a name nobody has,
        // which may well be a password typed in the wrong box.
        for typed in ["Leo", "Zqhunter2"] {
            let (status, _, _) = ask(
                &app,
                "POST",
                "/api/session",
                Some(serde_json::json!({ "user": typed, "password": "Zqwrong-password" })),
                None,
                true,
            )
            .await?;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }

        let rows = log_of(&app, &owner, |rows| {
            rows.iter()
                .any(|row| row["action"] == "sign_in" && row["outcome"] == "denied")
                && rows.iter().filter(|row| row["action"] == "command").count() >= 3
                && rows
                    .iter()
                    .any(|row| row["action"] == "room_added" && row["outcome"] == "denied")
        })
        .await?;

        let setup = row_by(&rows, "owner_set_up", "Nico").expect("the owner being set up");
        assert_eq!(setup["outcome"], "ok");
        let added = rows
            .iter()
            .find(|row| row["action"] == "person_added" && row["target"] == "leo")
            .expect("Leo being added");
        assert_eq!(added["actor"]["name"], "Nico");
        assert!(
            added["detail"]
                .as_str()
                .is_some_and(|d| d.contains("Leo") && d.contains(&rooms[0])),
            "{added}"
        );
        // What Leo did: the lamp, with the context of the call so history can say who.
        let on = rows
            .iter()
            .find(|row| {
                row["action"] == "command"
                    && row["actor"]["name"] == "Leo"
                    && row["outcome"] == "ok"
            })
            .expect("Leo turning the lamp on");
        assert_eq!(on["target"], lamp.as_str());
        assert_eq!(on["detail"], "turn_on");
        assert!(
            on["context_id"].as_str().is_some_and(|id| !id.is_empty()),
            "{on}"
        );
        // The camera: denied, with the reason only an owner is told.
        let camera = rows
            .iter()
            .find(|row| row["action"] == "command" && row["target"] == CAMERA)
            .expect("Leo asking for the camera");
        assert_eq!(camera["outcome"], "denied");
        assert_eq!(camera["actor"]["name"], "Leo");
        assert!(
            camera["detail"]
                .as_str()
                .is_some_and(|d| d.contains("not in their rooms")),
            "{camera}"
        );
        // A change that was refused is a row.
        let room = row_by(&rows, "room_added", "Leo").expect("Leo's refused room");
        assert_eq!(room["outcome"], "denied");
        assert!(
            room["detail"].as_str().is_some_and(|d| d.contains("owner")),
            "{room}"
        );
        let update = rows
            .iter()
            .find(|row| row["target"] == "update.irori" && row["actor"]["name"] == "Mia")
            .expect("Mia's update");
        assert_eq!(update["outcome"], "denied");
        // Wrong passwords: nobody signed in, the person it was typed for only if they are one.
        let wrong: Vec<_> = rows
            .iter()
            .filter(|row| row["action"] == "sign_in" && row["outcome"] == "denied")
            .collect();
        assert_eq!(wrong.len(), 2, "{wrong:?}");
        assert!(wrong.iter().all(|row| row["actor"]["kind"] == "nobody"));
        assert!(
            wrong
                .iter()
                .any(|row| row["target"] == "leo" && row["detail"] == "wrong password")
        );
        assert!(wrong.iter().any(|row| row["detail"] == "a name nobody has"));
        // And no secret anywhere: not a password somebody set or typed, nor what a nobody typed.
        let all = serde_json::Value::Array(rows.clone()).to_string();
        for secret in [
            "let me in!",
            "correct horse",
            "Zqwrong-password",
            "Zqhunter2",
        ] {
            assert!(!all.contains(secret), "the log holds {secret:?}: {all}");
        }

        // Who reads it: an owner. Nobody else, whatever rooms they have.
        for who in [&leo, &mia] {
            let (status, _, _) = ask(&app, "GET", "/api/audit", None, Some(who), false).await?;
            assert_eq!(status, StatusCode::FORBIDDEN);
        }
        let (status, _, _) = ask(&app, "GET", "/api/audit", None, None, false).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        Ok(())
    }

    /// A page reads the newest rows first, narrowed by what it asks, and goes on from where it
    /// left off.
    #[tokio::test]
    async fn the_log_is_read_newest_first_by_who_and_what_and_a_page_at_a_time()
    -> anyhow::Result<()> {
        let House {
            server: _server,
            _host,
            app,
            owner,
            leo,
            lamp_entities,
            ..
        } = house_with_a_vault().await?;
        let lamp = lamp_entities
            .iter()
            .find(|id| id.starts_with("light."))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the lamp has a light"))?;
        for name in ["turn_on", "turn_off", "turn_on"] {
            let (status, why, _) = ask(
                &app,
                "POST",
                "/api/command",
                Some(serde_json::json!({ "entity_id": lamp, "command": name })),
                Some(&leo),
                true,
            )
            .await?;
            assert_eq!(status, StatusCode::OK, "{why}");
        }
        log_of(&app, &owner, |rows| {
            rows.iter().filter(|row| row["action"] == "command").count() == 3
        })
        .await?;

        let (status, page, _) = ask(
            &app,
            "GET",
            "/api/audit?user=leo&action=command&limit=2",
            None,
            Some(&owner),
            false,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{page}");
        let first = page["entries"].as_array().cloned().unwrap_or_default();
        assert_eq!(first.len(), 2);
        assert_eq!(first[0]["detail"], "turn_on", "the newest first");
        assert_eq!(first[1]["detail"], "turn_off");
        let older = page["older"].as_u64().expect("there is one more");
        let (_, page, _) = ask(
            &app,
            "GET",
            &format!("/api/audit?user=leo&action=command&limit=2&before={older}"),
            None,
            Some(&owner),
            false,
        )
        .await?;
        let rest = page["entries"].as_array().cloned().unwrap_or_default();
        assert_eq!(rest.len(), 1, "{page}");
        assert!(page.get("older").is_none(), "the last page says so");

        // Anything it can't read as what it is meant to be is said, not ignored.
        for query in ["outcome=maybe", "since=yesterday", "before=x", "limit=lots"] {
            let (status, why, _) = ask(
                &app,
                "GET",
                &format!("/api/audit?{query}"),
                None,
                Some(&owner),
                false,
            )
            .await?;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{query}: {why}");
        }
        Ok(())
    }

    /// A program is written down as the program, with the person it was made for, and as it was
    /// called: the day it is revoked it is gone from its table, and the log still says who it was.
    /// A program is not the person to ask who did what.
    #[tokio::test]
    async fn a_program_is_written_down_as_itself_and_does_not_read_the_log() -> anyhow::Result<()> {
        let (core, _host) = demo().await?;
        let server = Server::new(core)?;
        let (app, owner) = locked_home(&server).await?;
        let (status, created, _) = ask(
            &app,
            "POST",
            "/api/tokens",
            Some(
                serde_json::json!({ "name": "Tablet", "scopes": ["services:call", "states:read"] }),
            ),
            Some(&owner),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let secret = created["secret"].as_str().unwrap_or_default().to_owned();
        let (_, entities, _) = ask(&app, "GET", "/api/entities", None, Some(&owner), false).await?;
        let light = entities
            .as_array()
            .and_then(|all| {
                all.iter()
                    .filter_map(|entity| entity["id"].as_str())
                    .find(|id| id.starts_with("light."))
            })
            .unwrap_or_default()
            .to_owned();
        let (status, why) = bearer(
            &app,
            "POST",
            "/api/command",
            Some(serde_json::json!({ "entity_id": light, "command": "turn_on" })),
            &secret,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{why}");
        let (status, _) = bearer(&app, "GET", "/api/audit", None, &secret).await?;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, _) = ask(
            &app,
            "DELETE",
            "/api/tokens/tablet",
            None,
            Some(&owner),
            true,
        )
        .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let rows = log_of(&app, &owner, |rows| {
            rows.iter().any(|row| row["action"] == "token_revoked")
                && rows.iter().any(|row| row["action"] == "command")
        })
        .await?;
        let command = rows
            .iter()
            .find(|row| row["action"] == "command")
            .expect("the program's command");
        assert_eq!(command["actor"]["kind"], "token");
        assert_eq!(command["actor"]["name"], "Tablet");
        assert_eq!(command["actor"]["person"]["name"], "Nico");
        // The refused read of the log is the program's too.
        let revoked = row_by(&rows, "token_revoked", "Nico").expect("the revoking");
        assert_eq!(revoked["target"], "tablet");
        assert!(
            revoked["detail"]
                .as_str()
                .is_some_and(|d| d.contains("Tablet")),
            "{revoked}"
        );
        let made = row_by(&rows, "token_made", "Nico").expect("the making");
        assert!(
            !made.to_string().contains(&secret),
            "the secret is shown once and never written down"
        );
        Ok(())
    }

    /// A change a command made says who sent the command, through the call it answered: the
    /// owner and a person with the whole home are told, a person with some rooms is told their
    /// own name and "someone" for anybody else.
    #[tokio::test]
    async fn history_says_who_sent_the_command_a_change_answered() -> anyhow::Result<()> {
        let House {
            server,
            _host,
            app,
            owner,
            leo,
            mia,
            lamp_entities,
            ..
        } = house_with_a_vault().await?;
        let light = lamp_entities
            .iter()
            .find(|id| id.starts_with("light."))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the lamp has a light"))?;
        let entity: EntityId = light.parse()?;
        for cookie in [&leo, &mia] {
            let (status, changed, _) = ask(
                &app,
                "POST",
                "/api/command",
                Some(serde_json::json!({ "entity_id": light, "command": "toggle" })),
                Some(cookie),
                true,
            )
            .await?;
            assert_eq!(status, StatusCode::OK, "{changed}");
            // The server under test isn't subscribed to the core, so the change is handed to the
            // diary the endpoint reads.
            server
                .history
                .record(entity.clone(), serde_json::from_value(changed)?);
        }
        let told = |cookie: String| {
            let (app, light) = (app.clone(), light.clone());
            async move {
                for _ in 0..300 {
                    let (status, history, _) = ask(
                        &app,
                        "GET",
                        &format!("/api/history/{light}"),
                        None,
                        Some(&cookie),
                        false,
                    )
                    .await?;
                    assert_eq!(status, StatusCode::OK, "{history}");
                    let mut said: Vec<String> = history["by"]
                        .as_object()
                        .map(|by| {
                            by.values()
                                .filter_map(|name| name.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default();
                    if said.len() == 2 {
                        said.sort();
                        return anyhow::Ok(said);
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                anyhow::bail!("history never said who")
            }
        };
        assert_eq!(told(owner).await?, ["Leo", "Mia"]);
        assert_eq!(told(leo).await?, ["Leo", "someone"]);
        Ok(())
    }
}
