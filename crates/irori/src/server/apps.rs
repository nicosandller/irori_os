//! Extensions' own pages (`docs/specs/automations.md` §B3): which there are, their files, and the
//! questions they ask their engine.
//!
//! The page runs in a sandboxed frame with an opaque origin, so its own files are served with
//! `Access-Control-Allow-Origin: *` — its scripts, wasm and stylesheet are cross-origin fetches
//! from where it stands. Nothing under `/api/` is: until there's auth, a CORS-open API would let
//! any web page switch the house.

use std::path::PathBuf;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use irori_core::{AppRequestError, ExtensionStatus};
use irori_types::{ApiScope, ExtensionId, Name, PackagePath};
use serde::{Deserialize, Serialize};

use super::{AppState, refused};

/// One sidebar entry.
#[derive(Debug, Serialize)]
pub(super) struct AppEntry {
    extension: ExtensionId,
    label: Name,
    has_icon: bool,
    /// What the shell's bridge may hand the page.
    api: Vec<ApiScope>,
    /// Whether there's an engine for the page to ask.
    has_engine: bool,
    /// Whether the page's files are there. A package installed without its page built still
    /// gets an entry, which says so, rather than an empty frame.
    built: bool,
}

/// Running extensions that have a page, by name.
pub(super) async fn list(State(state): State<AppState>) -> Json<Vec<AppEntry>> {
    let packages = state.0.host.packages_dir().to_path_buf();
    let mut entries: Vec<AppEntry> = state
        .0
        .core
        .extensions()
        .into_iter()
        .filter(|(_, overview)| {
            matches!(
                overview.status,
                ExtensionStatus::Running | ExtensionStatus::Degraded { .. }
            )
        })
        .filter_map(|(id, overview)| {
            let info = overview.info?;
            let app = info.app?;
            let built = packages
                .join(id.as_str())
                .join(app.entry.as_str())
                .is_file();
            Some(AppEntry {
                has_icon: info.icon.is_some(),
                label: app.label,
                api: app.api,
                has_engine: info.engine,
                built,
                extension: id,
            })
        })
        .collect();
    entries.sort_by(|a, b| a.label.as_str().cmp(b.label.as_str()));
    Json(entries)
}

#[derive(Debug, Deserialize)]
pub(super) struct RpcRequest {
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

/// A page's question for its engine.
pub(super) async fn rpc(
    State(state): State<AppState>,
    Path(id): Path<ExtensionId>,
    Json(request): Json<RpcRequest>,
) -> Response {
    match state
        .0
        .core
        .app_request(&id, request.method, request.params)
        .await
    {
        Ok(value) => Json(serde_json::json!({ "value": value })).into_response(),
        Err(error) => refused(
            match error {
                AppRequestError::NotRunning(_) => StatusCode::SERVICE_UNAVAILABLE,
                AppRequestError::Failed(_) => StatusCode::UNPROCESSABLE_ENTITY,
                AppRequestError::Timeout => StatusCode::GATEWAY_TIMEOUT,
            },
            error.to_string(),
        ),
    }
}

/// The page itself: `/pages/<id>/`.
pub(super) async fn index(
    state: State<AppState>,
    Path(id): Path<ExtensionId>,
    headers: HeaderMap,
) -> Response {
    serve(state, id, String::new(), &headers)
}

/// One of the page's files: `/pages/<id>/<path>`.
pub(super) async fn file(
    state: State<AppState>,
    Path((id, path)): Path<(ExtensionId, String)>,
    headers: HeaderMap,
) -> Response {
    serve(state, id, path, &headers)
}

fn serve(
    State(state): State<AppState>,
    id: ExtensionId,
    path: String,
    headers: &HeaderMap,
) -> Response {
    if opened_on_its_own(headers) {
        return (
            StatusCode::FORBIDDEN,
            format!("This is part of the `{id}` page. Open it from IroriOS: /apps/{id}/\n"),
        )
            .into_response();
    }
    let Some(entry) = state
        .0
        .core
        .extensions()
        .get(&id)
        .and_then(|overview| overview.info.as_ref())
        .and_then(|info| info.app.as_ref())
        .map(|app| app.entry.clone())
    else {
        return refused(StatusCode::NOT_FOUND, format!("`{id}` has no page"));
    };
    let package = state.0.host.packages_dir().join(id.as_str());
    let Some(file) = locate(&package, &entry, &path) else {
        return refused(
            StatusCode::NOT_FOUND,
            format!("`{path}` isn't part of `{id}`'s page"),
        );
    };
    let request_headers = headers;
    match std::fs::read(&file) {
        Ok(bytes) => {
            let mut response = bytes.into_response();
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(content_type(&file)),
            );
            if let Ok(policy) = HeaderValue::from_str(&page_csp(request_headers)) {
                headers.insert(header::CONTENT_SECURITY_POLICY, policy);
            }
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_static("*"),
            );
            headers.insert(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            );
            // A reinstall replaces these files in place; never let a stale copy outlive it.
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            response
        }
        Err(_) => refused(
            StatusCode::NOT_FOUND,
            format!("`{path}` isn't part of `{id}`'s page"),
        ),
    }
}

/// What a page may load: its own files, and nothing from anywhere else. `wasm-unsafe-eval` is
/// what compiling its own wasm needs, not `eval`.
///
/// This server's own address is named as well as `'self'`: the page runs in a sandboxed frame
/// whose origin is opaque, and while Chrome still takes `'self'` to mean the address the page
/// came from, Safari and Firefox take it to match nothing there — and the page can't even load
/// its own script, a blank frame.
fn page_csp(request: &HeaderMap) -> String {
    let own = request
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
        .filter(|host| {
            !host.is_empty()
                && host
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
        })
        .map(|host| format!(" http://{host}"))
        .unwrap_or_default();
    format!(
        "default-src 'self'{own}; script-src 'self'{own} 'wasm-unsafe-eval'; \
         style-src 'self'{own} 'unsafe-inline'; img-src 'self'{own} data:; \
         connect-src 'self'{own}; frame-ancestors 'self'"
    )
}

/// Whether the browser is opening this as a page of its own, in a tab, and not inside the
/// shell's frame.
///
/// The shell frames an extension's page sandboxed, with an origin of its own: no cookie is
/// sent for it, and all it can reach is what the shell's bridge hands it. Opened directly it
/// would instead be a page of this server's, sent the session cookie and free to ask for
/// anything the person signed in may. So it isn't served that way. The browser says which it
/// is (`Sec-Fetch-Dest`), and a page can't set or change that header. Anything that isn't a
/// browser opening a tab (the frame, the page's own scripts and images, `curl`) is served.
fn opened_on_its_own(request: &HeaderMap) -> bool {
    request
        .get("sec-fetch-dest")
        .is_some_and(|dest| dest == "document")
}

/// The file `path` names beside the page's `entry`, if it's a package path (no `..`, no hidden
/// names) and exists. The empty path is the entry itself.
fn locate(package: &std::path::Path, entry: &PackagePath, path: &str) -> Option<PathBuf> {
    let base = entry.as_str().rsplit_once('/').map_or("", |(dir, _)| dir);
    let relative = if path.is_empty() {
        entry.as_str().to_owned()
    } else if base.is_empty() {
        path.to_owned()
    } else {
        format!("{base}/{path}")
    };
    let relative = PackagePath::try_from(relative).ok()?;
    let file = package.join(relative.as_str());
    file.is_file().then_some(file)
}

fn content_type(file: &std::path::Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_policy_names_this_server_and_nothing_a_host_header_could_smuggle_in() {
        let mut request = HeaderMap::new();
        request.insert(header::HOST, HeaderValue::from_static("192.168.1.20:8480"));
        let policy = page_csp(&request);
        assert!(
            policy.contains("script-src 'self' http://192.168.1.20:8480 'wasm-unsafe-eval'"),
            "{policy}"
        );
        request.insert(header::HOST, HeaderValue::from_static("evil; script-src *"));
        assert!(
            !page_csp(&request).contains("evil"),
            "a Host that isn't a host is left out"
        );
    }

    #[test]
    fn a_page_is_served_into_the_shells_frame_and_not_as_a_tab_of_its_own() {
        let asked_as = |dest: Option<&'static str>| {
            let mut request = HeaderMap::new();
            if let Some(dest) = dest {
                request.insert("sec-fetch-dest", HeaderValue::from_static(dest));
            }
            opened_on_its_own(&request)
        };
        assert!(asked_as(Some("document")));
        // The frame, what the page loads for itself, and whatever isn't a browser.
        for inside in [
            Some("iframe"),
            Some("script"),
            Some("empty"),
            Some("image"),
            None,
        ] {
            assert!(!asked_as(inside), "{inside:?}");
        }
    }

    #[test]
    fn a_page_can_only_reach_its_own_files() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("app"))?;
        std::fs::write(dir.path().join("app/index.html"), "<!doctype html>")?;
        std::fs::write(dir.path().join("app/page.wasm"), [0u8])?;
        std::fs::write(dir.path().join("irori-extension.toml"), "")?;
        let entry = PackagePath::try_from("app/index.html").map_err(anyhow::Error::msg)?;

        assert_eq!(
            locate(dir.path(), &entry, ""),
            Some(dir.path().join("app/index.html"))
        );
        assert_eq!(
            locate(dir.path(), &entry, "page.wasm"),
            Some(dir.path().join("app/page.wasm"))
        );
        assert_eq!(locate(dir.path(), &entry, "../irori-extension.toml"), None);
        assert_eq!(locate(dir.path(), &entry, ".hidden"), None);
        assert_eq!(locate(dir.path(), &entry, "missing.js"), None);
        Ok(())
    }
}
