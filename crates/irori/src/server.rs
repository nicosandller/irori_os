//! HTTP server: health, a temporary unauthenticated view of the core under `/api/dev/` (reads,
//! plus the commands the Devices page sends), and the embedded UI. Nothing here checks who is
//! asking, which is why `serve` binds loopback unless told otherwise; auth and the WebSocket API
//! arrive with `irori-api` in M1.5.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use irori_core::{CallError, Command, Core, Event, ExtensionOverview};
use irori_types::{
    ContextId, Device, Entity, EntityId, EntityState, ExtensionId, LightTurnOn, Origin, UserId,
};
use tokio::sync::broadcast;

use crate::build_info::{BuildInfo, VERSION};
use crate::db::Database;

/// Who commands are attributed to until there are accounts to attribute them to (M1.5).
static UNAUTHENTICATED: LazyLock<UserId> =
    LazyLock::new(|| UserId::try_from("unauthenticated").expect("a valid user id"));

#[derive(Debug, Clone)]
pub struct AppState(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    started: Instant,
    db: Database,
    build: BuildInfo,
    core: Core,
}

impl AppState {
    pub fn new(db: Database, core: Core) -> Self {
        Self(Arc::new(Inner {
            started: Instant::now(),
            db,
            build: BuildInfo::current(),
            core,
        }))
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        // Unstable, for the UI and for trying things out until the real API (M0.5, M1.5) exists.
        // Everything the Devices page shows, in one response; the rest are the same data split up.
        .route("/api/dev/home", get(home))
        .route("/api/dev/command", post(command))
        .route(
            "/api/dev/devices",
            get(|State(s): State<AppState>| async move { Json(s.0.core.devices()) }),
        )
        .route(
            "/api/dev/entities",
            get(|State(s): State<AppState>| async move { Json(s.0.core.entities()) }),
        )
        .route(
            "/api/dev/states",
            get(|State(s): State<AppState>| async move { Json(s.0.core.states()) }),
        )
        .route(
            "/api/dev/extensions",
            get(|State(s): State<AppState>| async move { Json(s.0.core.extensions()) }),
        )
        .fallback(get(ui::serve))
        .with_state(state)
}

/// Everything the Devices page shows, in one response: what exists, what it's doing, and how the
/// extensions behind it are faring. The page asks for it every couple of seconds; it starts
/// listening for changes instead once the WebSocket API lands (M1.5).
#[derive(Debug, Serialize)]
struct HomeView {
    devices: Vec<Device>,
    entities: Vec<Entity>,
    states: Vec<EntityState>,
    extensions: BTreeMap<ExtensionId, ExtensionOverview>,
}

async fn home(State(state): State<AppState>) -> Json<HomeView> {
    let core = &state.0.core;
    Json(HomeView {
        devices: core.devices(),
        entities: core.entities(),
        states: core.states(),
        extensions: core.extensions(),
    })
}

/// A command from the UI: `{"entity_id": "light.hallway", "command": "toggle"}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandRequest {
    entity_id: EntityId,
    command: CommandName,
    /// Brightness or color, for `turn_on` on a light that supports them.
    #[serde(default)]
    data: Option<LightTurnOn>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CommandName {
    TurnOn,
    TurnOff,
    Toggle,
}

/// Asks an entity to do something and answers with its state once the integration confirms, so
/// the page can show the result without waiting for its next refresh.
///
/// There's no sign-in yet (ROADMAP D12, M1.5), so every command is attributed to one
/// unauthenticated person; with accounts it becomes the user who clicked.
async fn command(State(state): State<AppState>, Json(request): Json<CommandRequest>) -> Response {
    let command = match (request.command, request.data) {
        (CommandName::TurnOn, data) => Command::TurnOn(data.unwrap_or_default()),
        (CommandName::TurnOff, None) => Command::TurnOff,
        (CommandName::Toggle, None) => Command::Toggle,
        (_, Some(_)) => {
            return refused(
                StatusCode::BAD_REQUEST,
                "`data` is only for `turn_on`".to_owned(),
            );
        }
    };
    let core = &state.0.core;
    let who = core.new_context(Origin::User {
        user_id: UNAUTHENTICATED.clone(),
    });
    // Subscribed before the call: the integration reports the new state and answers the call in
    // the same breath, and the report must not slip past while the call is still in flight.
    let changes = core.subscribe();
    match core
        .call_service(&request.entity_id, command, who.clone())
        .await
    {
        Ok(()) => {
            let settled = settled(changes, &request.entity_id, &who.id).await;
            Json(settled.or_else(|| core.state(&request.entity_id))).into_response()
        }
        Err(e) => refused(status_for(&e), e.to_string()),
    }
}

/// How long to wait for the change a command caused before answering with what's on file. The
/// integration has already confirmed by this point, so the report is usually a moment away.
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
    uptime_ms: u128,
    features: &'a [&'static str],
    sqlite: SqliteHealth<'a>,
}

#[derive(Debug, Serialize)]
struct SqliteHealth<'a> {
    version: &'static str,
    journal_mode: &'a str,
}

/// Liveness: the process is up and serving. The `sqlite` fields describe the database as it was
/// opened at startup; they are not a live check. Real database health (read-only, disk full)
/// arrives with `irori-recorder` in M1.3, which keeps the connection open.
async fn health(State(state): State<AppState>) -> Response {
    let inner = &state.0;
    Json(Health {
        status: "ok",
        version: VERSION,
        uptime_ms: inner.started.elapsed().as_millis(),
        features: &inner.build.features,
        sqlite: SqliteHealth {
            version: inner.build.sqlite_version,
            journal_mode: &inner.db.journal_mode,
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

    async fn get(path: &str) -> anyhow::Result<(StatusCode, Option<String>, Vec<u8>)> {
        get_from(core(), path).await
    }

    async fn get_from(
        core: Core,
        path: &str,
    ) -> anyhow::Result<(StatusCode, Option<String>, Vec<u8>)> {
        let dir = tempfile::tempdir()?;
        let app = router(AppState::new(crate::db::open(dir.path())?, core));
        let res = app.oneshot(Request::get(path).body(Body::empty())?).await?;
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
    #[cfg(feature = "int-demo")]
    async fn demo() -> anyhow::Result<(Core, irori_core::ExtensionHost)> {
        let core = core();
        let host = irori_core::ExtensionHost::start(
            &core,
            crate::extensions::builtins()?,
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

    #[cfg(feature = "int-demo")]
    async fn post(
        core: Core,
        path: &str,
        body: serde_json::Value,
    ) -> anyhow::Result<(StatusCode, serde_json::Value)> {
        let dir = tempfile::tempdir()?;
        let app = router(AppState::new(crate::db::open(dir.path())?, core));
        let request = Request::post(path)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body)?))?;
        let res = app.oneshot(request).await?;
        let status = res.status();
        let bytes = res.into_body().collect().await?.to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        Ok((status, json))
    }

    /// Everything the Devices page needs arrives in one response.
    #[cfg(feature = "int-demo")]
    #[tokio::test]
    async fn dev_home_describes_the_whole_home() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let (status, _, body) = get_from(core.clone(), "/api/dev/home").await?;
        assert_eq!(status, StatusCode::OK);
        let home: serde_json::Value = serde_json::from_slice(&body)?;

        let named = |key: &str, id: &str| {
            home[key]
                .as_array()
                .is_some_and(|all| all.iter().any(|item| item["name"] == id))
        };
        assert!(named("devices", "Demo lamp"), "no demo lamp in {home}");
        assert!(named("entities", "Demo lamp"), "no lamp entity in {home}");
        assert_eq!(home["extensions"]["demo"]["state"], "running");
        let lamp = home["states"]
            .as_array()
            .and_then(|all| all.iter().find(|s| s["entity_id"] == "light.demo_lamp"))
            .ok_or_else(|| anyhow::anyhow!("no demo lamp state in {home}"))?;
        assert_eq!(lamp["availability"], "available");

        host.shutdown().await;
        Ok(())
    }

    /// The page turns the lamp on and is told what it became, without waiting for a refresh.
    #[cfg(feature = "int-demo")]
    #[tokio::test]
    async fn a_command_answers_with_the_state_it_produced() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let lamp = EntityId::try_from("light.demo_lamp")?;

        for on in [true, false] {
            let (status, body) = post(
                core.clone(),
                "/api/dev/command",
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

    /// A refused command says why, with a status that matches the reason.
    #[cfg(feature = "int-demo")]
    #[tokio::test]
    async fn refusals_explain_themselves() -> anyhow::Result<()> {
        let (core, host) = demo().await?;

        let (status, body) = post(
            core.clone(),
            "/api/dev/command",
            serde_json::json!({"entity_id": "light.nowhere", "command": "toggle"}),
        )
        .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "there's no entity `light.nowhere`");

        // A sensor has nothing to turn on.
        let (status, body) = post(
            core.clone(),
            "/api/dev/command",
            serde_json::json!({"entity_id": "sensor.demo_hallway_sensor_temperature", "command": "toggle"}),
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        // Brightness belongs to `turn_on`, and nowhere else.
        let (status, body) = post(
            core.clone(),
            "/api/dev/command",
            serde_json::json!({
                "entity_id": "light.demo_lamp", "command": "turn_off", "data": {"brightness": 5},
            }),
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "`data` is only for `turn_on`");

        host.shutdown().await;
        Ok(())
    }

    /// The demo's virtual devices show up in the read-only view.
    #[cfg(feature = "int-demo")]
    #[tokio::test]
    async fn dev_view_lists_demo_devices_and_states() -> anyhow::Result<()> {
        let (core, host) = demo().await?;
        let (status, _, body) = get_from(core.clone(), "/api/dev/states").await?;
        assert_eq!(status, StatusCode::OK);
        let states: serde_json::Value = serde_json::from_slice(&body)?;
        let lamp = states
            .as_array()
            .and_then(|all| all.iter().find(|s| s["entity_id"] == "light.demo_lamp"))
            .ok_or_else(|| anyhow::anyhow!("no demo lamp in {states}"))?;
        assert_eq!(lamp["state"]["kind"], "light");

        let (_, _, body) = get_from(core.clone(), "/api/dev/extensions").await?;
        let extensions: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(extensions["demo"]["state"], "running");

        host.shutdown().await;
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
        for path in ["/api/dev/not-real", "/api/nope", "/api"] {
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
}
