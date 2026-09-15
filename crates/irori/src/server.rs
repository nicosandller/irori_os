//! HTTP server for the hello-world build: health endpoint plus the embedded static page.
//! Moves into `irori-api` with auth and the WS API in M1.5.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::build_info::{BuildInfo, VERSION};
use crate::db::Database;

#[derive(Debug, Clone)]
pub struct AppState(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    started: Instant,
    db: Database,
    build: BuildInfo,
}

impl AppState {
    pub fn new(db: Database) -> Self {
        Self(Arc::new(Inner {
            started: Instant::now(),
            db,
            build: BuildInfo::current(),
        }))
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .fallback(get(ui::serve))
        .with_state(state)
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

    #[derive(RustEmbed)]
    #[folder = "assets/"]
    struct Assets;

    pub async fn serve(uri: Uri) -> Response {
        let path = uri.path().trim_start_matches('/');
        let path = if path.is_empty() { "index.html" } else { path };
        match Assets::get(path) {
            Some(file) => {
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

    async fn get(path: &str) -> anyhow::Result<(StatusCode, Option<String>, Vec<u8>)> {
        let dir = tempfile::tempdir()?;
        let app = router(AppState::new(crate::db::open(dir.path())?));
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

    #[tokio::test]
    async fn unknown_path_is_not_found() -> anyhow::Result<()> {
        let (status, _, _) = get("/does-not-exist").await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        Ok(())
    }
}
