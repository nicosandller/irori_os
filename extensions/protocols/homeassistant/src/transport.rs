//! How this extension reaches Home Assistant: plain TCP for an `http://` address, TLS for an
//! `https://` one. The WebSocket API and the HTTP requests for pictures and video both open their
//! connections here, so nothing can carry the token over a connection that wasn't checked.
//!
//! A certificate is trusted when a public authority issued it for the name used, when it chains
//! to the owner's own CA certificate for that name, or when it is exactly the certificate the
//! owner gave (by its SHA-256 fingerprint or as PEM): Home Assistant's self-signed one, which has
//! no authority and often no matching name.

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, RootCertStore, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::settings::{Settings, Trust, is_lan_ip};

/// A connection to Home Assistant, encrypted or not.
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send + fmt::Debug {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + fmt::Debug> Io for T {}
pub type Stream = Box<dyn Io>;

/// Why a connection couldn't be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    Unreachable(String),
    /// Whatever answered isn't trusted to be this Home Assistant. Nothing was sent to it.
    Untrusted(String),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(why) | Self::Untrusted(why) => f.write_str(why),
        }
    }
}

/// Where Home Assistant is and how a connection to it is checked.
#[derive(Clone)]
pub struct Transport {
    host: String,
    port: u16,
    authority: String,
    websocket_url: String,
    tls: Option<(TlsConnector, ServerName<'static>)>,
    /// The owner said this public name resolves locally: only a local answer is connected to.
    lan_only: bool,
}

impl fmt::Debug for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transport")
            .field("authority", &self.authority)
            .field("tls", &self.tls.is_some())
            .finish_non_exhaustive()
    }
}

impl Transport {
    pub fn of(settings: &Settings) -> Result<Self, String> {
        let (host, port, authority) = settings.host_port();
        let tls = if settings.is_secure() {
            let name = ServerName::try_from(host.clone())
                .map_err(|_| format!("`{host}` can't be checked against a certificate"))?;
            Some((connector(&settings.trust()?)?, name))
        } else {
            None
        };
        Ok(Self {
            host,
            port,
            authority,
            websocket_url: settings.websocket_url(),
            tls,
            lan_only: settings.resolves_locally,
        })
    }

    /// `host[:port]`, as an HTTP `Host` header gives it.
    pub fn authority(&self) -> &str {
        &self.authority
    }

    pub fn websocket_url(&self) -> &str {
        &self.websocket_url
    }

    /// Opens a connection, with its certificate checked before anything is sent.
    pub async fn open(&self) -> Result<Stream, OpenError> {
        let tcp = self.tcp().await?;
        let Some((connector, name)) = &self.tls else {
            return Ok(Box::new(tcp));
        };
        match connector.connect(name.clone(), tcp).await {
            Ok(stream) => Ok(Box::new(stream)),
            Err(why) => Err(
                match why
                    .get_ref()
                    .and_then(|e| e.downcast_ref::<rustls::Error>())
                {
                    Some(rustls::Error::InvalidCertificate(reason)) => {
                        OpenError::Untrusted(format!(
                            "Home Assistant's certificate can't be trusted ({reason:?}), so the token wasn't sent. If it's your own certificate, enter its SHA-256 fingerprint or your CA certificate in this extension's settings."
                        ))
                    }
                    _ => OpenError::Unreachable(format!(
                        "couldn't set up an encrypted connection to Home Assistant: {why}"
                    )),
                },
            ),
        }
    }

    async fn tcp(&self) -> Result<TcpStream, OpenError> {
        let unreachable = |why: std::io::Error| {
            OpenError::Unreachable(format!("couldn't reach Home Assistant: {why}"))
        };
        if !self.lan_only {
            return TcpStream::connect((self.host.as_str(), self.port))
                .await
                .map_err(unreachable);
        }
        let local: Vec<SocketAddr> = tokio::net::lookup_host((self.host.as_str(), self.port))
            .await
            .map_err(unreachable)?
            .filter(|address| is_lan_ip(address.ip()))
            .collect();
        if local.is_empty() {
            return Err(OpenError::Untrusted(format!(
                "`{}` doesn't resolve to an address on the local network here, so the token wasn't sent",
                self.host
            )));
        }
        TcpStream::connect(local.as_slice())
            .await
            .map_err(unreachable)
    }
}

fn connector(trust: &Trust) -> Result<TlsConnector, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(|why| format!("couldn't set up encryption: {why}"))?;
    let config = match trust {
        Trust::Public => {
            let mut roots = RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            builder.with_root_certificates(roots).with_no_client_auth()
        }
        Trust::Fingerprint(fingerprint) => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Owner {
                exact: vec![*fingerprint],
                chain: None,
                provider,
            }))
            .with_no_client_auth(),
        Trust::Certificates(certificates) => {
            let mut roots = RootCertStore::empty();
            let ders = certificates
                .iter()
                .map(|der| CertificateDer::from(der.as_slice()));
            let (added, _) = roots.add_parsable_certificates(ders);
            let chain = if added > 0 {
                Some(
                    WebPkiServerVerifier::builder_with_provider(
                        Arc::new(roots),
                        Arc::clone(&provider),
                    )
                    .build()
                    .map_err(|why| format!("couldn't use that certificate: {why}"))?,
                )
            } else {
                None
            };
            let exact = certificates.iter().map(|der| fingerprint(der)).collect();
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(Owner {
                    exact,
                    chain,
                    provider,
                }))
                .with_no_client_auth()
        }
    };
    Ok(TlsConnector::from(Arc::new(config)))
}

/// A certificate's SHA-256 fingerprint.
pub fn fingerprint(der: &[u8]) -> [u8; 32] {
    Sha256::digest(der).into()
}

/// Trusts exactly the owner's certificates, and whatever chains to them for the right name.
#[derive(Debug)]
struct Owner {
    exact: Vec<[u8; 32]>,
    chain: Option<Arc<WebPkiServerVerifier>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Owner {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if self.exact.contains(&fingerprint(end_entity.as_ref())) {
            return Ok(ServerCertVerified::assertion());
        }
        match &self.chain {
            Some(chain) => {
                chain.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
            }
            None => Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            )),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
