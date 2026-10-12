//! `irori watch`: the public socket. The client sends nothing after it opens.

use futures_util::StreamExt;
use irori_client::Client;
use irori_types::LiveMessage;
use std::sync::Arc;

use tokio_rustls::rustls;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{Connector, connect_async, connect_async_tls_with_config};

use super::output;

pub async fn run(client: &Client, json: bool) -> Result<(), String> {
    if client.insecure() && client.origin().starts_with("https://") {
        return Err(
            "watch checks certificates. Use http on this machine, or trust the certificate with --ca."
                .to_owned(),
        );
    }
    let (url, headers) = client.websocket("/api/ws");
    let mut request = url
        .into_client_request()
        .map_err(|error| format!("couldn't open the socket: {error}"))?;
    for (name, value) in headers {
        let name = name
            .parse::<tokio_tungstenite::tungstenite::http::header::HeaderName>()
            .map_err(|error| error.to_string())?;
        let value = value
            .parse::<tokio_tungstenite::tungstenite::http::header::HeaderValue>()
            .map_err(|error| error.to_string())?;
        request.headers_mut().insert(name, value);
    }
    // The socket has its own TLS setup, so `--ca` is handed to it here. Without this the
    // certificate Irori made for itself could never be trusted for `watch`.
    let opened = match client.ca_pem() {
        Some(pem) if client.origin().starts_with("https://") => {
            connect_async_tls_with_config(request, None, false, Some(trusting(pem)?)).await
        }
        _ => connect_async(request).await,
    };
    let (mut socket, _) =
        opened.map_err(|error| format!("couldn't reach {}: {error}", client.origin()))?;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            message = socket.next() => {
                let Some(message) = message else { return Ok(()) };
                let message = message.map_err(|error| format!("the socket closed: {error}"))?;
                let text = match message {
                    tokio_tungstenite::tungstenite::Message::Text(text) => text.to_string(),
                    tokio_tungstenite::tungstenite::Message::Ping(_) => continue,
                    tokio_tungstenite::tungstenite::Message::Close(_) => return Ok(()),
                    _ => continue,
                };
                let frame: LiveMessage = serde_json::from_str(&text)
                    .map_err(|error| format!("Irori sent something this client can't read: {error}"))?;
                match frame {
                    LiveMessage::Snapshot { home } => {
                        if json {
                            output::json_out(&serde_json::json!({ "type": "snapshot", "home": home }))?;
                        } else {
                            let devices = home.get("devices").and_then(|v| v.as_array()).map(|v| v.len()).unwrap_or(0);
                            let entities = home.get("entities").and_then(|v| v.as_array()).map(|v| v.len()).unwrap_or(0);
                            println!("snapshot\t{devices} devices\t{entities} entities");
                        }
                    }
                    LiveMessage::State { entity_id, state } => {
                        if json {
                            output::json_out(&serde_json::json!({
                                "type": "state",
                                "entity_id": entity_id.as_str(),
                                "state": state,
                            }))?;
                        } else {
                            let brief = serde_json::to_string(&state.state).unwrap_or_else(|_| "unknown".into());
                            println!("state\t{entity_id}\t{brief}");
                        }
                    }
                    LiveMessage::Changed => {
                        // The page reads the home again. One line is enough here: the next
                        // state frames carry the values, and a dump would bury them.
                        if json {
                            output::json_out(&serde_json::json!({ "type": "changed" }))?;
                        } else {
                            println!("changed");
                        }
                    }
                }
            }
        }
    }
}

/// The usual roots plus the certificates in the `--ca` file, as the HTTP client trusts them.
fn trusting(pem: &[u8]) -> Result<Connector, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut added = 0;
    for cert in rustls_pemfile::certs(&mut &*pem) {
        let cert = cert.map_err(|error| format!("couldn't read the CA certificate: {error}"))?;
        roots
            .add(cert)
            .map_err(|error| format!("couldn't use the CA certificate: {error}"))?;
        added += 1;
    }
    if added == 0 {
        return Err("the --ca file has no certificate in it".to_owned());
    }
    // `ring`, named, as the server's own TLS does (`tls.rs`): nothing here depends on
    // which providers the rest of the build happens to link.
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| format!("couldn't prepare TLS: {error}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Connector::Rustls(Arc::new(config)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_ca_file_is_trusted_by_the_socket() {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["localhost".to_owned()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        assert!(matches!(
            trusting(cert.pem().as_bytes()),
            Ok(Connector::Rustls(_))
        ));
        let error = trusting(b"not a certificate").err().unwrap();
        assert!(error.contains("no certificate"), "{error}");
    }
}
