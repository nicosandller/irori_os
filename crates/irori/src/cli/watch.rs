//! `irori watch`: the public socket. The client sends nothing after it opens.

use futures_util::StreamExt;
use irori_client::Client;
use irori_types::LiveMessage;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

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
    let (mut socket, _) = connect_async(request)
        .await
        .map_err(|error| format!("couldn't reach {}: {error}", client.origin()))?;
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
                        if let Err(error) = client.get::<serde_json::Value>("/api/home").await {
                            eprintln!("error: {}", error.message);
                        }
                    }
                }
            }
        }
    }
}
