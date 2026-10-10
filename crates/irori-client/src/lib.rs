//! HTTP client for a running Irori (`docs/specs/api.md`, `docs/specs/cli.md`).
//!
//! A bearer token is sent as `Authorization` and never with `x-irori-ui`. A session cookie
//! is the owner's sign-in: changes also send `x-irori-ui: 1`, the same tripwire the page
//! sends so a browser on another site cannot use the cookie. The header is not a secret.

use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

mod types;

pub use types::*;

const UI_HEADER: &str = "x-irori-ui";
const SESSION_COOKIE: &str = "irori_session";

/// How a call is signed, if it is.
#[derive(Debug, Clone)]
enum Auth {
    None,
    Bearer(String),
    Session(String),
}

/// A connection to one Irori.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    origin: String,
    auth: Auth,
    /// `true` when certificate checks are off. `watch` needs to know.
    insecure: bool,
    /// Send `x-irori-ui` on a change that has no session. Restart on an open home is the
    /// one caller: a bearer token never sends the header, so it stays refused.
    ui_header: bool,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("origin", &self.origin)
            .field("insecure", &self.insecure)
            .finish_non_exhaustive()
    }
}

/// Why a call did not succeed. `message` is the server's sentence when it sent one.
#[derive(Debug, Clone)]
pub struct Error {
    pub status: Option<u16>,
    pub message: String,
    /// `unpair_failed` and anything else the body names. Absent when it doesn't.
    pub code: Option<String>,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl Error {
    fn message(message: impl Into<String>) -> Self {
        Self {
            status: None,
            message: message.into(),
            code: None,
        }
    }
}

/// Builds a [`Client`]. The origin is `http://127.0.0.1:8480` with no path.
pub struct Builder {
    origin: String,
    auth: Auth,
    insecure: bool,
    ca_pem: Option<Vec<u8>>,
    ui_header: bool,
}

impl std::fmt::Debug for Builder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Builder")
            .field("origin", &self.origin)
            .field("insecure", &self.insecure)
            .field("ui_header", &self.ui_header)
            .finish_non_exhaustive()
    }
}

impl Builder {
    pub fn new(origin: impl Into<String>) -> Result<Self, Error> {
        Ok(Self {
            origin: normalize_origin(&origin.into())?,
            auth: Auth::None,
            insecure: false,
            ca_pem: None,
            ui_header: false,
        })
    }

    pub fn bearer(mut self, token: impl Into<String>) -> Self {
        self.auth = Auth::Bearer(token.into());
        self
    }

    pub fn session(mut self, cookie: impl Into<String>) -> Self {
        self.auth = Auth::Session(cookie.into());
        self
    }

    /// Skip certificate checks. For the certificate Irori made for itself.
    pub fn insecure(mut self, yes: bool) -> Self {
        self.insecure = yes;
        self
    }

    pub fn ca_pem(mut self, pem: Vec<u8>) -> Self {
        self.ca_pem = Some(pem);
        self
    }

    /// Send `x-irori-ui: 1` on changes even with no session cookie. Ignored for a bearer token.
    pub fn ui_header(mut self, yes: bool) -> Self {
        self.ui_header = yes;
        self
    }

    pub fn build(self) -> Result<Client, Error> {
        let mut builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
        if self.insecure {
            builder = builder.danger_accept_invalid_certs(true);
        }
        if let Some(pem) = &self.ca_pem {
            let cert = reqwest::Certificate::from_pem(pem).map_err(|error| {
                Error::message(format!("couldn't read the CA certificate: {error}"))
            })?;
            builder = builder.add_root_certificate(cert);
        }
        let http = builder.build().map_err(|error| {
            Error::message(format!("couldn't prepare the HTTP client: {error}"))
        })?;
        Ok(Client {
            http,
            origin: self.origin,
            auth: self.auth,
            insecure: self.insecure,
            ui_header: self.ui_header,
        })
    }
}

/// `http://host:port` with no path and no trailing slash.
pub fn normalize_origin(raw: &str) -> Result<String, Error> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(Error::message(
            "the server address is empty; pass --url http://127.0.0.1:8480",
        ));
    }
    let Some((scheme, rest)) = raw.split_once("://") else {
        return Err(Error::message(format!(
            "`{raw}` needs a scheme, like http://127.0.0.1:8480"
        )));
    };
    if scheme != "http" && scheme != "https" {
        return Err(Error::message(format!("`{raw}` must be http or https")));
    }
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() || rest.contains('/') {
        return Err(Error::message(format!(
            "`{raw}` is the server itself, like http://127.0.0.1:8480, not a path on it"
        )));
    }
    Ok(format!("{scheme}://{rest}"))
}

impl Client {
    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn insecure(&self) -> bool {
        self.insecure
    }

    /// The same connection, sending `x-irori-ui` on changes. A bearer token still does not.
    pub fn with_ui_header(&self) -> Self {
        let mut client = self.clone();
        client.ui_header = true;
        client
    }

    /// `ws://` or `wss://` for `path`, plus the headers a call would send. No UI header:
    /// the socket checks the credential on open and the client sends nothing after that.
    pub fn websocket(&self, path: &str) -> (String, Vec<(&'static str, String)>) {
        let scheme = if self.origin.starts_with("https://") {
            "wss"
        } else {
            "ws"
        };
        let host = self
            .origin
            .split_once("://")
            .map(|(_, host)| host)
            .unwrap_or(&self.origin);
        let mut headers = Vec::new();
        match &self.auth {
            Auth::None => {}
            Auth::Bearer(token) => headers.push(("authorization", format!("Bearer {token}"))),
            Auth::Session(cookie) => headers.push(("cookie", format!("{SESSION_COOKIE}={cookie}"))),
        }
        (format!("{scheme}://{host}{path}"), headers)
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        self.round_trip(reqwest::Method::GET, path, None::<&()>, true)
            .await
    }

    pub async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T, Error> {
        self.round_trip(reqwest::Method::POST, path, Some(body), true)
            .await
    }

    pub async fn put<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T, Error> {
        self.round_trip(reqwest::Method::PUT, path, Some(body), true)
            .await
    }

    pub async fn patch<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T, Error> {
        self.round_trip(reqwest::Method::PATCH, path, Some(body), true)
            .await
    }

    /// POST/PUT/PATCH/DELETE that succeeds with an empty body.
    pub async fn empty(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<(), Error> {
        let response = self.send(method, path, body, true).await?;
        read_error(response).await?;
        Ok(())
    }

    /// The body as JSON, or `null` when the server sent none.
    pub async fn value(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<Value, Error> {
        let response = self.send(method, path, body, true).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(error_from(status.as_u16(), response).await);
        }
        let bytes = response.bytes().await.map_err(|error| {
            Error::message(format!("the answer from {} stopped: {error}", self.origin))
        })?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|error| {
            Error::message(format!(
                "Irori sent something this client can't read: {error}"
            ))
        })
    }

    /// Reads a server-sent event stream until it closes. `on_data` gets each `data:` payload.
    pub async fn sse<F>(
        &self,
        path: &str,
        body: &impl Serialize,
        mut on_data: F,
    ) -> Result<(), Error>
    where
        F: FnMut(&str),
    {
        let mut response = self
            .send(reqwest::Method::POST, path, Some(body), false)
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(error_from(status.as_u16(), response).await);
        }
        let mut pending = String::new();
        loop {
            let chunk = response.chunk().await.map_err(|error| {
                Error::message(format!("the stream from {} stopped: {error}", self.origin))
            })?;
            let Some(chunk) = chunk else { break };
            pending.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(split) = pending.find('\n') {
                let line = pending[..split].trim_end_matches('\r').to_owned();
                pending.drain(..=split);
                if let Some(data) = line.strip_prefix("data:") {
                    on_data(data.trim());
                }
            }
        }
        Ok(())
    }

    pub async fn sign_in(&self, user: &str, password: &str) -> Result<(Session, String), Error> {
        let response = self
            .send(
                reqwest::Method::POST,
                "/api/session",
                Some(&serde_json::json!({ "user": user, "password": password })),
                true,
            )
            .await?;
        let status = response.status();
        let cookie = session_cookie(response.headers()).map(str::to_owned);
        if !status.is_success() {
            return Err(error_from(status.as_u16(), response).await);
        }
        let Some(cookie) = cookie else {
            return Err(Error::message(
                "Irori signed in without a session cookie".to_owned(),
            ));
        };
        let session = response.json().await.map_err(|error| {
            Error::message(format!(
                "Irori sent something this client can't read: {error}"
            ))
        })?;
        Ok((session, cookie))
    }

    async fn round_trip<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&impl Serialize>,
        timeout: bool,
    ) -> Result<T, Error> {
        let response = self.send(method, path, body, timeout).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(error_from(status.as_u16(), response).await);
        }
        response.json().await.map_err(|error| {
            Error::message(format!(
                "Irori sent something this client can't read: {error}"
            ))
        })
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&impl Serialize>,
        timeout: bool,
    ) -> Result<reqwest::Response, Error> {
        let url = format!("{}{path}", self.origin);
        let mut request = self.http.request(method.clone(), &url);
        if timeout {
            request = request.timeout(Duration::from_secs(30));
        }
        let changing = method != reqwest::Method::GET && method != reqwest::Method::HEAD;
        match &self.auth {
            Auth::None => {
                // An open home has no cookie. Restart still checks this header on the request.
                if self.ui_header && changing {
                    request = request.header(UI_HEADER, "1");
                }
            }
            Auth::Bearer(token) => {
                // No UI header. A token that tries a setup change is refused as a token.
                request = request.header("authorization", format!("Bearer {token}"));
            }
            Auth::Session(cookie) => {
                request = request.header("cookie", format!("{SESSION_COOKIE}={cookie}"));
                // A cookie is sent by a browser whoever wrote the page, so a change also
                // carries the header only Irori's own programs send.
                if changing {
                    request = request.header(UI_HEADER, "1");
                }
            }
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        request
            .send()
            .await
            .map_err(|error| Error::message(format!("couldn't reach {}: {error}", self.origin)))
    }
}

fn session_cookie(headers: &reqwest::header::HeaderMap) -> Option<&str> {
    headers.get_all("set-cookie").iter().find_map(|value| {
        let text = value.to_str().ok()?;
        let (name, rest) = text.split_once('=')?;
        if name.trim() != SESSION_COOKIE {
            return None;
        }
        let token = rest.split(';').next()?.trim();
        (!token.is_empty()).then_some(token)
    })
}

async fn read_error(response: reqwest::Response) -> Result<(), Error> {
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(error_from(status.as_u16(), response).await)
    }
}

async fn error_from(status: u16, response: reqwest::Response) -> Error {
    let bytes = response.bytes().await.unwrap_or_default();
    if let Ok(value) = serde_json::from_slice::<Value>(&bytes)
        && let Some(message) = value.get("error").and_then(Value::as_str)
    {
        return Error {
            status: Some(status),
            message: message.to_owned(),
            code: value.get("code").and_then(Value::as_str).map(str::to_owned),
        };
    }
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim();
    Error {
        status: Some(status),
        message: if text.is_empty() {
            format!("Irori refused that ({status})")
        } else {
            text.to_owned()
        },
        code: None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    use axum::Router;
    use axum::body::Bytes;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::post;
    use std::sync::{Arc, Mutex};

    #[test]
    fn origin_drops_a_trailing_slash_and_refuses_a_path() {
        assert_eq!(
            normalize_origin("http://127.0.0.1:8480/").unwrap(),
            "http://127.0.0.1:8480"
        );
        assert!(normalize_origin("127.0.0.1:8480").is_err());
        assert!(normalize_origin("http://127.0.0.1:8480/api").is_err());
        assert!(normalize_origin("ftp://127.0.0.1").is_err());
    }

    /// A one-route server. `seen` is the headers of the last call.
    async fn serve(
        status: StatusCode,
        body: &'static str,
        cookie: bool,
    ) -> (String, Arc<Mutex<HeaderMap>>) {
        let seen = Arc::new(Mutex::new(HeaderMap::new()));
        let handler = {
            let recorded = seen.clone();
            move |headers: HeaderMap, _body: Bytes| {
                let recorded = recorded.clone();
                async move {
                    *recorded.lock().expect("headers") = headers;
                    let mut response = axum::http::Response::builder().status(status);
                    if cookie {
                        response = response.header("set-cookie", "irori_session=sess; HttpOnly");
                    }
                    response
                        .header("content-type", "application/json")
                        .body(body.to_owned())
                        .expect("response")
                }
            }
        };
        let app = Router::new()
            .route("/api/thing", post(handler.clone()))
            .route("/api/session", post(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        (format!("http://127.0.0.1:{port}"), seen)
    }

    #[tokio::test]
    async fn a_token_is_sent_without_the_ui_header() {
        let (origin, seen) = serve(StatusCode::NO_CONTENT, "", false).await;
        let client = Builder::new(origin)
            .unwrap()
            .bearer("irori_test")
            .build()
            .unwrap();
        client
            .empty(reqwest::Method::POST, "/api/thing", None::<&()>)
            .await
            .unwrap();
        let headers = seen.lock().expect("headers");
        assert_eq!(headers.get("authorization").unwrap(), "Bearer irori_test");
        assert!(headers.get("x-irori-ui").is_none());
    }

    #[tokio::test]
    async fn a_session_change_sends_the_ui_header() {
        let (origin, seen) = serve(StatusCode::NO_CONTENT, "", false).await;
        let client = Builder::new(origin)
            .unwrap()
            .session("sess")
            .build()
            .unwrap();
        client
            .empty(reqwest::Method::POST, "/api/thing", None::<&()>)
            .await
            .unwrap();
        let headers = seen.lock().expect("headers");
        assert_eq!(headers.get("cookie").unwrap(), "irori_session=sess");
        assert_eq!(headers.get("x-irori-ui").unwrap(), "1");
    }

    #[tokio::test]
    async fn an_open_home_can_send_the_header_without_a_cookie() {
        let (origin, seen) = serve(StatusCode::NO_CONTENT, "", false).await;
        let client = Builder::new(origin)
            .unwrap()
            .ui_header(true)
            .build()
            .unwrap();
        client
            .empty(reqwest::Method::POST, "/api/thing", None::<&()>)
            .await
            .unwrap();
        let headers = seen.lock().expect("headers");
        assert!(headers.get("cookie").is_none());
        assert_eq!(headers.get("x-irori-ui").unwrap(), "1");
    }

    #[tokio::test]
    async fn a_token_keeps_the_header_off_even_when_asked() {
        let (origin, seen) = serve(StatusCode::NO_CONTENT, "", false).await;
        let client = Builder::new(origin)
            .unwrap()
            .bearer("irori_test")
            .ui_header(true)
            .build()
            .unwrap();
        client
            .empty(reqwest::Method::POST, "/api/thing", None::<&()>)
            .await
            .unwrap();
        let headers = seen.lock().expect("headers");
        assert!(headers.get("x-irori-ui").is_none());
    }

    #[tokio::test]
    async fn an_error_body_is_the_servers_sentence() {
        let (origin, _) = serve(
            StatusCode::FORBIDDEN,
            r#"{"error":"a token can't change how the home is set up","code":"token"}"#,
            false,
        )
        .await;
        let client = Builder::new(origin).unwrap().build().unwrap();
        let error = client
            .empty(reqwest::Method::POST, "/api/thing", None::<&()>)
            .await
            .expect_err("refused");
        assert_eq!(error.status, Some(403));
        assert!(error.message.contains("token can't change"));
        assert_eq!(error.code.as_deref(), Some("token"));
    }

    #[tokio::test]
    async fn sign_in_reads_the_session_cookie() {
        let (origin, _) = serve(
            StatusCode::OK,
            r#"{"locked":true,"user":null,"owner":true,"setup":{"owner":true,"place":false}}"#,
            true,
        )
        .await;
        let client = Builder::new(origin).unwrap().build().unwrap();
        let (session, cookie) = client.sign_in("ada", "secret").await.unwrap();
        assert_eq!(cookie, "sess");
        assert!(session.owner);
        assert!(session.locked);
    }
}
