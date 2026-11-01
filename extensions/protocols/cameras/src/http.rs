//! Plain HTTP to a camera, with the logins cameras ask for: Digest (MD5), or Basic when that's
//! all the camera offers.
//!
//! A login is only sent once the camera has asked for one, and a camera that still says no
//! after seeing it has refused it ([`HttpError::Refused`]). That's the one answer that counts
//! against the account, so it's kept apart from everything else that can go wrong.
//!
//! Every address goes to the camera this was made for: whatever host a camera writes into an
//! address it hands back, the request goes to the address it was found at. A camera can't point
//! this at anything else, and one that reports the wrong address of its own still works.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HOST, WWW_AUTHENTICATE};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use md5::{Digest as _, Md5};

use crate::settings::Login;

/// How long a camera has to answer one request, connection included.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The most a camera's answer may be: a picture's limit with room to spare.
pub const MAX_BODY: usize = 4 * 1024 * 1024;

/// What went wrong asking a camera something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    /// It saw the login and said no. Never try the same one again.
    Refused,
    /// It couldn't be reached, or stopped answering. Safe to try again later.
    Unreachable(String),
    /// It answered, with something other than what was asked for.
    Failed(String),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused => f.write_str("the camera refused its login"),
            Self::Unreachable(why) | Self::Failed(why) => f.write_str(why),
        }
    }
}

/// An answer from a camera.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

/// Where on the camera to send a request: the path (and query) of an address it gave, and its
/// port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub port: u16,
    /// Starts with `/`.
    pub path: String,
}

impl Target {
    /// Reads `http://host[:port]/path`. The host is dropped (see the module docs). Anything but
    /// plain `http` is refused: cameras' own addresses are, and there is no certificate to check
    /// one against anyway.
    pub fn parse(url: &str) -> Result<Self, String> {
        let url = url.trim();
        let rest = match url.get(..7) {
            Some(scheme) if scheme.eq_ignore_ascii_case("http://") => &url[7..],
            _ => return Err("the camera gave an address that isn't plain http".into()),
        };
        let (authority, path) = match rest.find('/') {
            Some(at) => (&rest[..at], &rest[at..]),
            None => (rest, "/"),
        };
        let authority = authority.rsplit('@').next().unwrap_or(authority);
        let port = if let Some(end) = authority.strip_prefix('[') {
            // [v6]:port
            match end.split_once(']') {
                Some((_, "")) => 80,
                Some((_, port)) => parse_port(port.strip_prefix(':').unwrap_or(port))?,
                None => return Err("the camera gave an address that can't be read".into()),
            }
        } else {
            match authority.rsplit_once(':') {
                Some((_, port)) => parse_port(port)?,
                None => 80,
            }
        };
        if path.chars().any(|c| c.is_control() || c == ' ') {
            return Err("the camera gave an address that can't be read".into());
        }
        Ok(Self {
            port,
            path: path.to_owned(),
        })
    }
}

fn parse_port(text: &str) -> Result<u16, String> {
    match text.parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        _ => Err("the camera gave an address with a port that can't be read".into()),
    }
}

/// Talks to one camera.
#[derive(Debug, Clone)]
pub struct Camera {
    pub ip: IpAddr,
}

impl Camera {
    /// Sends one request, logging in if the camera asks. `body` goes with `content_type`.
    pub async fn send(
        &self,
        method: Method,
        target: &Target,
        body: Option<(&str, Vec<u8>)>,
        login: Option<&Login>,
    ) -> Result<Response, HttpError> {
        tokio::time::timeout(TIMEOUT, self.send_now(method, target, body, login))
            .await
            .unwrap_or_else(|_| Err(HttpError::Unreachable("the camera didn't answer".into())))
    }

    async fn send_now(
        &self,
        method: Method,
        target: &Target,
        body: Option<(&str, Vec<u8>)>,
        login: Option<&Login>,
    ) -> Result<Response, HttpError> {
        let first = self.once(&method, target, body.as_ref(), None).await?;
        if first.status != StatusCode::UNAUTHORIZED.as_u16() {
            return Ok(first.response);
        }
        let Some(login) = login else {
            return Err(HttpError::Refused);
        };
        let Some(answer) = Challenge::pick(&first.challenges)
            .map(|challenge| challenge.answer(login, method.as_str(), &target.path))
        else {
            return Err(HttpError::Failed(
                "the camera asked for a login in a way this doesn't speak".into(),
            ));
        };
        let second = self
            .once(&method, target, body.as_ref(), Some(answer))
            .await?;
        if second.status == StatusCode::UNAUTHORIZED.as_u16() {
            return Err(HttpError::Refused);
        }
        Ok(second.response)
    }

    async fn once(
        &self,
        method: &Method,
        target: &Target,
        body: Option<&(&str, Vec<u8>)>,
        authorization: Option<String>,
    ) -> Result<Exchange, HttpError> {
        let stream = self.connect(target.port).await?;
        self.exchange(
            stream,
            target.port,
            method,
            &target.path,
            body,
            authorization,
        )
        .await
    }

    /// POSTs `json` over HTTPS to `path` (with its query), with no HTTP login: for an API that
    /// logs in inside the body.
    pub async fn post_tls(
        &self,
        port: u16,
        path: &str,
        json: Vec<u8>,
    ) -> Result<Response, HttpError> {
        let exchange = async {
            let stream = self.connect(port).await?;
            let name = rustls::pki_types::ServerName::IpAddress(self.ip.into());
            let stream = tokio_rustls::TlsConnector::from(Arc::clone(tls_config()))
                .connect(name, stream)
                .await
                .map_err(|_| {
                    HttpError::Unreachable("the camera's HTTPS couldn't be opened".into())
                })?;
            let body = ("application/json", json);
            self.exchange(stream, port, &Method::POST, path, Some(&body), None)
                .await
        };
        let exchange = tokio::time::timeout(TIMEOUT, exchange)
            .await
            .unwrap_or_else(|_| Err(HttpError::Unreachable("the camera didn't answer".into())))?;
        Ok(exchange.response)
    }

    async fn connect(&self, port: u16) -> Result<tokio::net::TcpStream, HttpError> {
        tokio::net::TcpStream::connect(SocketAddr::new(self.ip, port))
            .await
            .map_err(|_| HttpError::Unreachable("the camera can't be reached".into()))
    }

    async fn exchange<S>(
        &self,
        stream: S,
        port: u16,
        method: &Method,
        path: &str,
        body: Option<&(&str, Vec<u8>)>,
        authorization: Option<String>,
    ) -> Result<Exchange, HttpError>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|_| HttpError::Unreachable("the camera can't be reached".into()))?;
        let driver = tokio::spawn(connection);
        let host = match (self.ip, port) {
            (IpAddr::V6(ip), 80) => format!("[{ip}]"),
            (IpAddr::V6(ip), port) => format!("[{ip}]:{port}"),
            (ip, 80) => ip.to_string(),
            (ip, port) => format!("{ip}:{port}"),
        };
        let mut request = Request::builder()
            .method(method.clone())
            .uri(path)
            .header(HOST, host);
        if let Some((content_type, _)) = body {
            request = request.header(CONTENT_TYPE, *content_type);
        }
        if let Some(authorization) = authorization {
            request = request.header(AUTHORIZATION, authorization);
        }
        let payload = body.map(|(_, bytes)| bytes.clone()).unwrap_or_default();
        let request = request.body(Full::new(Bytes::from(payload))).map_err(|_| {
            HttpError::Failed("the camera gave an address that can't be used".into())
        })?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|_| HttpError::Unreachable("the camera stopped answering".into()))?;
        let status = response.status().as_u16();
        let challenges: Vec<String> = response
            .headers()
            .get_all(WWW_AUTHENTICATE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .map(str::to_owned)
            .collect();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = Limited::new(response.into_body(), MAX_BODY)
            .collect()
            .await
            .map_err(|_| HttpError::Failed("the camera's answer was too big or cut short".into()))?
            .to_bytes()
            .to_vec();
        driver.abort();
        Ok(Exchange {
            status,
            challenges,
            response: Response {
                status,
                content_type,
                body,
            },
        })
    }
}

/// TLS for cameras: any certificate, since each camera signs its own, but a handshake signature
/// that checks out against it.
fn tls_config() -> &'static Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = Arc::new(OwnCerts {
            provider: Arc::clone(&provider),
        });
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("the ring provider supports TLS")
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        Arc::new(config)
    })
}

/// Checks a handshake signature against the certificate's key read straight out of it, for a
/// certificate the usual parser won't take: Reolinks send X.509 version 1, which it refuses.
fn verify_with_key(
    message: &[u8],
    cert: &rustls::pki_types::CertificateDer<'_>,
    scheme: rustls::SignatureScheme,
    signature: &[u8],
    algorithms: &rustls::crypto::WebPkiSupportedAlgorithms,
) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
    let bad = || rustls::Error::InvalidCertificate(rustls::CertificateError::BadSignature);
    let (algorithm, key) = public_key(cert.as_ref()).ok_or(rustls::Error::InvalidCertificate(
        rustls::CertificateError::BadEncoding,
    ))?;
    let candidates = algorithms
        .mapping
        .iter()
        .find(|(known, _)| *known == scheme)
        .map(|(_, candidates)| *candidates)
        .ok_or_else(bad)?;
    let verifier = candidates
        .iter()
        .find(|candidate| candidate.public_key_alg_id().as_ref() == algorithm)
        .ok_or_else(bad)?;
    verifier
        .verify_signature(key, message, signature)
        .map(|()| rustls::client::danger::HandshakeSignatureValid::assertion())
        .map_err(|_| bad())
}

/// A certificate's public key: its algorithm (the inside of its `AlgorithmIdentifier`) and the
/// key's own bytes. Reads any X.509 version.
fn public_key(cert: &[u8]) -> Option<(&[u8], &[u8])> {
    let (0x30, cert, _) = der(cert)? else {
        return None;
    };
    let (0x30, mut tbs, _) = der(cert)? else {
        return None;
    };
    if tbs.first() == Some(&0xa0) {
        tbs = der(tbs)?.2;
    }
    // Serial, signature algorithm, issuer, validity, subject.
    for _ in 0..5 {
        tbs = der(tbs)?.2;
    }
    let (0x30, info, _) = der(tbs)? else {
        return None;
    };
    let (0x30, algorithm, rest) = der(info)? else {
        return None;
    };
    let (0x03, [0, key @ ..], _) = der(rest)? else {
        return None;
    };
    Some((algorithm, key))
}

/// One DER element: its tag, what's inside it, and what comes after it.
fn der(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (length, rest) = match first {
        0..=0x7f => (usize::from(first), rest),
        0x81..=0x83 => {
            let bytes = usize::from(first - 0x80);
            let (length, rest) = rest.split_at_checked(bytes)?;
            (
                length
                    .iter()
                    .fold(0usize, |sum, byte| (sum << 8) | usize::from(*byte)),
                rest,
            )
        }
        _ => return None,
    };
    let (inside, after) = rest.split_at_checked(length)?;
    Some((tag, inside, after))
}

#[derive(Debug)]
struct OwnCerts {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for OwnCerts {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
        .or_else(|_| {
            verify_with_key(
                message,
                cert,
                dss.scheme,
                dss.signature(),
                &self.provider.signature_verification_algorithms,
            )
        })
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
        .or_else(|_| {
            verify_with_key(
                message,
                cert,
                dss.scheme,
                dss.signature(),
                &self.provider.signature_verification_algorithms,
            )
        })
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

struct Exchange {
    status: u16,
    challenges: Vec<String>,
    response: Response,
}

/// How a camera asked for a login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Challenge {
    Digest {
        realm: String,
        nonce: String,
        opaque: Option<String>,
        /// Whether it takes `qop=auth`.
        auth: bool,
        /// `MD5-sess` rather than `MD5`.
        session: bool,
    },
    Basic,
}

impl Challenge {
    /// The one to answer: Digest when it's on offer, since Basic sends the password as it is.
    pub fn pick(headers: &[String]) -> Option<Self> {
        let parsed: Vec<Self> = headers.iter().filter_map(|h| Self::parse(h)).collect();
        parsed
            .iter()
            .find(|challenge| matches!(challenge, Self::Digest { .. }))
            .or_else(|| parsed.first())
            .cloned()
    }

    pub fn parse(header: &str) -> Option<Self> {
        let header = header.trim();
        let (scheme, rest) = header.split_once(' ').unwrap_or((header, ""));
        if scheme.eq_ignore_ascii_case("basic") {
            return Some(Self::Basic);
        }
        if !scheme.eq_ignore_ascii_case("digest") {
            return None;
        }
        let params = params(rest);
        let get = |key: &str| {
            params
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(key))
                .map(|(_, value)| value.clone())
        };
        let algorithm = get("algorithm").unwrap_or_else(|| "MD5".into());
        let session = if algorithm.eq_ignore_ascii_case("md5") {
            false
        } else if algorithm.eq_ignore_ascii_case("md5-sess") {
            true
        } else {
            return None;
        };
        let auth = match get("qop") {
            Some(qop) => {
                if !qop
                    .split(',')
                    .any(|q| q.trim().eq_ignore_ascii_case("auth"))
                {
                    return None;
                }
                true
            }
            None => false,
        };
        Some(Self::Digest {
            realm: get("realm")?,
            nonce: get("nonce")?,
            opaque: get("opaque"),
            auth,
            session,
        })
    }

    /// The `Authorization` header that answers it.
    pub fn answer(&self, login: &Login, method: &str, uri: &str) -> String {
        match self {
            Self::Basic => {
                use base64::Engine as _;
                let pair = format!("{}:{}", login.username, login.password);
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(pair)
                )
            }
            Self::Digest {
                realm,
                nonce,
                opaque,
                auth,
                session,
            } => {
                let cnonce = cnonce();
                digest_header(
                    login,
                    method,
                    uri,
                    &DigestParts {
                        realm,
                        nonce,
                        opaque: opaque.as_deref(),
                        auth: *auth,
                        session: *session,
                        cnonce: &cnonce,
                    },
                )
            }
        }
    }
}

pub(crate) struct DigestParts<'a> {
    pub realm: &'a str,
    pub nonce: &'a str,
    pub opaque: Option<&'a str>,
    pub auth: bool,
    pub session: bool,
    pub cnonce: &'a str,
}

/// RFC 2617's answer, for one request on a fresh nonce (`nc=00000001`).
pub(crate) fn digest_header(login: &Login, method: &str, uri: &str, parts: &DigestParts) -> String {
    let nc = "00000001";
    let mut ha1 = md5_hex(&format!(
        "{}:{}:{}",
        login.username, parts.realm, login.password
    ));
    if parts.session {
        ha1 = md5_hex(&format!("{ha1}:{}:{}", parts.nonce, parts.cnonce));
    }
    let ha2 = md5_hex(&format!("{method}:{uri}"));
    let response = if parts.auth {
        md5_hex(&format!(
            "{ha1}:{}:{nc}:{}:auth:{ha2}",
            parts.nonce, parts.cnonce
        ))
    } else {
        md5_hex(&format!("{ha1}:{}:{ha2}", parts.nonce))
    };
    let mut header = format!(
        "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", response=\"{response}\"",
        quote(&login.username),
        quote(parts.realm),
        quote(parts.nonce),
    );
    if parts.session {
        header.push_str(", algorithm=MD5-sess");
    } else {
        header.push_str(", algorithm=MD5");
    }
    if parts.auth {
        header.push_str(&format!(", qop=auth, nc={nc}, cnonce=\"{}\"", parts.cnonce));
    }
    if let Some(opaque) = parts.opaque {
        header.push_str(&format!(", opaque=\"{}\"", quote(opaque)));
    }
    header
}

fn quote(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn md5_hex(text: &str) -> String {
    hex(&Md5::digest(text.as_bytes()))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A fresh client nonce: 16 random bytes, as hex.
fn cnonce() -> String {
    let mut bytes = [0u8; 16];
    // A failed draw leaves zeros: the camera's own nonce still makes the answer one-off.
    let _ = getrandom::fill(&mut bytes);
    hex(&bytes)
}

/// `key=value, key="quoted, value"` pairs.
pub(crate) fn params(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        while matches!(chars.peek(), Some(c) if *c == ',' || c.is_whitespace()) {
            chars.next();
        }
        let mut key = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c == ',' {
                break;
            }
            key.push(c);
            chars.next();
        }
        if key.is_empty() {
            break;
        }
        let mut value = String::new();
        if chars.peek() == Some(&'=') {
            chars.next();
            if chars.peek() == Some(&'"') {
                chars.next();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            if let Some(next) = chars.next() {
                                value.push(next);
                            }
                        }
                        '"' => break,
                        c => value.push(c),
                    }
                }
            } else {
                while let Some(&c) = chars.peek() {
                    if c == ',' {
                        break;
                    }
                    value.push(c);
                    chars.next();
                }
            }
        }
        out.push((key.trim().to_owned(), value.trim().to_owned()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login() -> Login {
        Login {
            username: "Mufasa".into(),
            password: "Circle Of Life".into(),
        }
    }

    /// RFC 2617 §3.5's worked example.
    #[test]
    fn a_digest_answer_matches_the_rfc() {
        let header = digest_header(
            &login(),
            "GET",
            "/dir/index.html",
            &DigestParts {
                realm: "testrealm@host.com",
                nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093",
                opaque: Some("5ccc069c403ebaf9f0171e9517f40e41"),
                auth: true,
                session: false,
                cnonce: "0a4f113b",
            },
        );
        assert!(
            header.contains("response=\"6629fae49393a05397450978507c4ef1\""),
            "{header}"
        );
        assert!(header.contains("qop=auth, nc=00000001, cnonce=\"0a4f113b\""));
    }

    #[test]
    fn digest_is_preferred_over_basic() {
        let picked = Challenge::pick(&[
            "Basic realm=\"cam\"".into(),
            "Digest realm=\"cam\", qop=\"auth,auth-int\", nonce=\"abc\", algorithm=MD5".into(),
        ]);
        assert_eq!(
            picked,
            Some(Challenge::Digest {
                realm: "cam".into(),
                nonce: "abc".into(),
                opaque: None,
                auth: true,
                session: false,
            })
        );
        assert_eq!(
            Challenge::pick(&["Basic realm=\"cam\"".into()]),
            Some(Challenge::Basic)
        );
        assert_eq!(
            Challenge::pick(&["Digest realm=\"cam\", nonce=\"n\", algorithm=SHA-256".into()]),
            None,
            "only MD5 is spoken"
        );
    }

    #[test]
    fn an_address_keeps_its_port_and_path_but_not_its_host() {
        assert_eq!(
            Target::parse("http://192.168.68.63:8000/onvif/device_service"),
            Ok(Target {
                port: 8000,
                path: "/onvif/device_service".into()
            })
        );
        assert_eq!(
            Target::parse("http://10.0.0.9/cgi-bin/api.cgi?cmd=Snap&channel=0"),
            Ok(Target {
                port: 80,
                path: "/cgi-bin/api.cgi?cmd=Snap&channel=0".into()
            })
        );
        assert_eq!(
            Target::parse("HTTP://[fe80::1]:2020"),
            Ok(Target {
                port: 2020,
                path: "/".into()
            })
        );
        assert!(Target::parse("https://cam/onvif").is_err());
        assert!(Target::parse("rtsp://cam/stream").is_err());
        assert!(Target::parse("http://cam:99999/").is_err());
    }

    #[test]
    fn a_version_1_certificates_key_still_checks_a_handshake() {
        let cert =
            rustls::pki_types::CertificateDer::from(&include_bytes!("testdata/v1-cert.der")[..]);
        let signature = include_bytes!("testdata/v1-pss-signature.bin");
        let provider = rustls::crypto::ring::default_provider();
        let algorithms = &provider.signature_verification_algorithms;
        let scheme = rustls::SignatureScheme::RSA_PSS_SHA256;

        // The usual parser won't read it, which is why this exists.
        assert!(rustls::server::ParsedCertificate::try_from(&cert).is_err());
        assert!(verify_with_key(b"a handshake", &cert, scheme, signature, algorithms).is_ok());
        assert!(verify_with_key(b"another one", &cert, scheme, signature, algorithms).is_err());
        let other = rustls::SignatureScheme::ECDSA_NISTP256_SHA256;
        assert!(verify_with_key(b"a handshake", &cert, other, signature, algorithms).is_err());
        assert_eq!(public_key(&[0x30, 0x05, 1]), None);
    }
}
