//! Serving over https (`--tls`, `docs/specs/config.md` §3.5).
//!
//! A home that asks who is there sends a password and then a cookie with every request. On
//! plain http anything on the same network can read both. With `--tls` they travel encrypted.
//!
//! The certificate is `tls/cert.pem` and `tls/key.pem` in the data directory. If they aren't
//! there, Irori makes its own the first time and keeps it. A certificate a home makes for
//! itself is one no browser has heard of, so the browser warns once and asks to be told it's
//! fine; what travels is encrypted all the same. A certificate from somewhere a browser does
//! trust (a private CA, a DNS-validated one) is put in the same two files and used as it is.
//!
//! The handshake is done off the accept loop, each connection in its own task with a time
//! limit, so one client that opens a connection and says nothing can't hold up the rest.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::server::TlsStream;

/// How long a client has to finish saying hello.
const HANDSHAKE: Duration = Duration::from_secs(10);

/// How many finished handshakes may wait for the server to take them.
const WAITING: usize = 64;

/// Where the certificate and its key are kept.
pub fn files(data: &Path) -> (PathBuf, PathBuf) {
    let dir = data.join("tls");
    (dir.join("cert.pem"), dir.join("key.pem"))
}

/// The names a certificate Irori makes for itself is good for: this machine by the names and
/// addresses it is usually reached by.
fn names(bind: SocketAddr) -> Vec<String> {
    let mut names = vec![
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
        "::1".to_owned(),
    ];
    if !bind.ip().is_unspecified() && !bind.ip().is_loopback() {
        names.push(bind.ip().to_string());
    }
    if let Some(host) = sysinfo::System::host_name().filter(|host| !host.is_empty()) {
        // As mDNS announces it, and bare.
        if !host.contains('.') {
            names.push(format!("{host}.local"));
        }
        names.push(host);
    }
    names.dedup();
    names
}

/// Makes a certificate and its key, and writes both. The key is readable by Irori's own user
/// and nobody else, set before a byte of it is written.
fn make(cert_path: &Path, key_path: &Path, bind: SocketAddr) -> anyhow::Result<()> {
    let made = rcgen::generate_simple_self_signed(names(bind))
        .context("couldn't make a certificate for this machine")?;
    if let Some(dir) = cert_path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    {
        use std::io::Write as _;
        let mut key = options
            .open(key_path)
            .with_context(|| format!("couldn't write {}", key_path.display()))?;
        key.write_all(made.key_pair.serialize_pem().as_bytes())?;
    }
    std::fs::write(cert_path, made.cert.pem())
        .with_context(|| format!("couldn't write {}", cert_path.display()))?;
    Ok(())
}

/// Reads the certificate and key, making them first if there are none.
pub fn config(data: &Path, bind: SocketAddr) -> anyhow::Result<Arc<ServerConfig>> {
    let (cert_path, key_path) = files(data);
    if !cert_path.exists() && !key_path.exists() {
        make(&cert_path, &key_path, bind)?;
        tracing::info!(
            cert = %cert_path.display(),
            "made a certificate for this machine; a browser will ask about it once, because \
             nobody but this home has vouched for it"
        );
    }
    let read = |path: &Path| {
        std::fs::read(path).with_context(|| format!("couldn't read {}", path.display()))
    };
    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut read(&cert_path)?.as_slice())
            .collect::<Result<_, _>>()
            .with_context(|| format!("{} isn't a PEM certificate", cert_path.display()))?;
    anyhow::ensure!(
        !certs.is_empty(),
        "{} holds no certificate",
        cert_path.display()
    );
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut read(&key_path)?.as_slice())
        .with_context(|| format!("{} isn't a PEM key", key_path.display()))?
        .with_context(|| format!("{} holds no key", key_path.display()))?;
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("this build's TLS has no protocol versions to offer")?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("the certificate and the key don't go together")?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// A listener whose connections arrive with the TLS handshake already done.
#[derive(Debug)]
pub struct TlsListener {
    ready: mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
    local: SocketAddr,
}

impl TlsListener {
    pub fn new(listener: TcpListener, config: Arc<ServerConfig>) -> std::io::Result<Self> {
        let local = listener.local_addr()?;
        let acceptor = TlsAcceptor::from(config);
        let (tx, ready) = mpsc::channel(WAITING);
        tokio::spawn(async move {
            loop {
                let (stream, from) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        // Out of file descriptors, most likely: wait rather than spin.
                        tracing::warn!(%error, "couldn't accept a connection");
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        continue;
                    }
                };
                if tx.is_closed() {
                    return;
                }
                let (acceptor, done) = (acceptor.clone(), tx.clone());
                tokio::spawn(async move {
                    // A handshake that fails is somebody speaking plain http to an https
                    // port, or a browser turning the certificate down: not worth a log line each.
                    if let Ok(Ok(stream)) =
                        tokio::time::timeout(HANDSHAKE, acceptor.accept(stream)).await
                    {
                        let _ = done.send((stream, from)).await;
                    }
                });
            }
        });
        Ok(Self { ready, local })
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.ready.recv().await {
            Some(accepted) => accepted,
            // The accept loop only ends with the process.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(self.local)
    }
}

/// Where a request came from, whichever way it was served. One type for both listeners, so
/// what reads it (`server/auth.rs`) doesn't care which.
#[derive(Debug, Clone, Copy)]
pub struct ClientAddr(pub SocketAddr);

impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, TcpListener>>
    for ClientAddr
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, TcpListener>) -> Self {
        Self(*stream.remote_addr())
    }
}

impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, TlsListener>>
    for ClientAddr
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, TlsListener>) -> Self {
        Self(*stream.remote_addr())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind() -> SocketAddr {
        SocketAddr::from(([0, 0, 0, 0], 8480))
    }

    #[test]
    fn a_certificate_is_made_once_and_kept() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        config(dir.path(), bind())?;
        let (cert, key) = files(dir.path());
        let first = std::fs::read(&cert)?;
        assert!(String::from_utf8_lossy(&first).contains("BEGIN CERTIFICATE"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            // Not checked as root, which reads everything whatever the mode says.
            let mode = std::fs::metadata(&key)?.permissions().mode();
            assert_eq!(mode & 0o077, 0, "{mode:o}");
        }
        // The second start uses the one that's there.
        config(dir.path(), bind())?;
        assert_eq!(std::fs::read(&cert)?, first);
        Ok(())
    }

    #[test]
    fn a_certificate_without_its_key_is_refused_and_says_which_file() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        config(dir.path(), bind())?;
        let (_, key) = files(dir.path());
        std::fs::write(&key, "not a key")?;
        let why = config(dir.path(), bind()).expect_err("no key");
        assert!(format!("{why:#}").contains("key.pem"), "{why:#}");
        Ok(())
    }

    #[test]
    fn its_own_certificate_names_this_machine() {
        let names = names(SocketAddr::from(([192, 168, 1, 20], 8480)));
        assert!(names.contains(&"localhost".to_owned()));
        assert!(names.contains(&"192.168.1.20".to_owned()));
    }
}
