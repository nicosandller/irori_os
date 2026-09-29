//! What the host does for automation engines (`docs/specs/automations.md` §B2): answers their
//! requests for the registry, states and history, forwards the events they subscribed to, makes
//! their service calls as `Origin::Automation`, and carries their page's questions to them.
//!
//! Every request is checked against the API scopes the extension declared. A protocol extension
//! gets the same treatment: the scopes are what count, not the kind of extension.

use std::collections::HashMap;
use std::path::PathBuf;

use irori_protocol::{FromExt, ToExt, WireCommand};
use irori_types::{ApiScope, Context, ExtensionId, ExtensionManifest, LightTurnOn, Origin};
use serde_json::json;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::{AppCall, AppInfo, CallError, Command, Core, Event};

/// The engine side of one running extension process: its scopes, what it subscribed to, and
/// the messages waiting to go to it.
pub(crate) struct EngineLink {
    scopes: Vec<ApiScope>,
    is_protocol: bool,
    out_tx: mpsc::UnboundedSender<ToExt>,
    out_rx: mpsc::UnboundedReceiver<ToExt>,
    app_tx: mpsc::Sender<AppCall>,
    app_rx: mpsc::Receiver<AppCall>,
    pending_app: HashMap<u64, oneshot::Sender<Result<serde_json::Value, String>>>,
    next_app: u64,
    events: Option<broadcast::Receiver<Event>>,
    states: bool,
    registry: bool,
}

impl EngineLink {
    pub(crate) fn new(manifest: &ExtensionManifest) -> Self {
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let (app_tx, app_rx) = mpsc::channel(64);
        Self {
            scopes: manifest.permissions.api.clone(),
            is_protocol: !manifest.contributes.protocol.is_empty(),
            out_tx,
            out_rx,
            app_tx,
            app_rx,
            pending_app: HashMap::new(),
            next_app: 0,
            events: None,
            states: false,
            registry: false,
        }
    }

    /// Where the core sends the page's questions for this process.
    pub(crate) fn app_sender(&self) -> mpsc::Sender<AppCall> {
        self.app_tx.clone()
    }

    /// The next message for the process: an answer, a subscribed event, or a page's question.
    /// Cancel-safe, so the host can `select!` on it.
    pub(crate) async fn next_outgoing(&mut self, _core: &Core) -> Option<ToExt> {
        loop {
            tokio::select! {
                biased;
                Some(message) = self.out_rx.recv() => return Some(message),
                Some(call) = self.app_rx.recv() => {
                    let id = self.next_app;
                    self.next_app += 1;
                    self.pending_app.insert(id, call.reply);
                    return Some(ToExt::AppRequest {
                        id,
                        method: call.method,
                        params: call.params,
                    });
                }
                event = next_event(&mut self.events) => match event {
                    Ok(event) => {
                        if let Some(message) = self.forward(event) {
                            return Some(message);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::warn!(missed, "an engine fell behind the home's events; it missed some");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        self.events = None;
                    }
                },
            }
        }
    }

    fn forward(&self, event: Event) -> Option<ToExt> {
        match event {
            Event::StateChanged {
                entity_id,
                old_state,
                new_state,
            } if self.states => Some(ToExt::StateChanged {
                entity_id,
                old_state,
                new_state,
            }),
            Event::DeviceAdded { .. }
            | Event::DeviceUpdated { .. }
            | Event::DeviceRemoved { .. }
            | Event::EntityAdded { .. }
            | Event::EntityUpdated { .. }
            | Event::EntityRemoved { .. }
                if self.registry =>
            {
                Some(ToExt::RegistryChanged {})
            }
            _ => None,
        }
    }

    fn answer(&self, id: u64, result: Result<serde_json::Value, String>) {
        let (value, error) = match result {
            Ok(value) => (Some(value), None),
            Err(error) => (None, Some(error)),
        };
        let _ = self.out_tx.send(ToExt::Answer { id, value, error });
    }

    fn allowed(&self, scope: ApiScope) -> Result<(), String> {
        if self.scopes.contains(&scope) {
            Ok(())
        } else {
            Err(format!("this extension didn't ask for {scope}"))
        }
    }

    /// Takes the engine messages, and the protocol messages an engine may not send; hands every
    /// other message back for the protocol path.
    pub(crate) fn handle(
        &mut self,
        core: &Core,
        extension: &ExtensionId,
        from: FromExt,
    ) -> Option<FromExt> {
        match from {
            FromExt::GetRegistry { id } => {
                let result = self.allowed(ApiScope::RegistryRead).map(|()| {
                    json!({
                        "entities": core.entities(),
                        // `irori.toml` has neither yet (rules.md K13).
                        "timezone": false,
                        "location": false,
                    })
                });
                self.answer(id, result);
            }
            FromExt::GetStates { id } => {
                let result = self
                    .allowed(ApiScope::StatesRead)
                    .map(|()| json!(core.states()));
                self.answer(id, result);
            }
            FromExt::GetHistory {
                id,
                entities,
                since,
            } => {
                let result = self.allowed(ApiScope::HistoryRead).map(|()| {
                    let history: serde_json::Map<String, serde_json::Value> = entities
                        .iter()
                        .map(|entity| {
                            (
                                entity.as_str().to_owned(),
                                json!(core.history(entity, since)),
                            )
                        })
                        .collect();
                    serde_json::Value::Object(history)
                });
                self.answer(id, result);
            }
            FromExt::Subscribe {
                id,
                states,
                registry,
            } => {
                let result = self.allowed(ApiScope::EventsRead).map(|()| {
                    self.states |= states;
                    self.registry |= registry;
                    if self.events.is_none() && (self.states || self.registry) {
                        self.events = Some(core.subscribe());
                    }
                    serde_json::Value::Null
                });
                self.answer(id, result);
            }
            FromExt::CallService {
                id,
                entity_id,
                command,
                data,
                run_id,
                parent_id,
            } => {
                if let Err(error) = self.allowed(ApiScope::ServicesCall) {
                    self.answer(id, Err(format!("not_allowed: {error}")));
                    return None;
                }
                let command = match (command, data) {
                    (WireCommand::TurnOn, data) => Command::TurnOn(data.unwrap_or_default()),
                    (WireCommand::TurnOff, None) => Command::TurnOff,
                    (WireCommand::Toggle, None) => Command::Toggle,
                    (_, Some(LightTurnOn { .. })) => {
                        self.answer(
                            id,
                            Err("not_supported: `data` is only for turn_on".to_owned()),
                        );
                        return None;
                    }
                };
                // The core says who's asking: this extension, as this run. An engine can't
                // claim to be a person or another extension.
                let mut context = core.new_context(Origin::Automation {
                    extension: extension.clone(),
                    run_id,
                });
                context.parent_id = parent_id;
                let core = core.clone();
                let out = self.out_tx.clone();
                tokio::spawn(async move {
                    let error = call(&core, &entity_id, command, context).await.err();
                    let _ = out.send(ToExt::Answer {
                        id,
                        value: error.is_none().then_some(serde_json::Value::Null),
                        error,
                    });
                });
            }
            FromExt::AppAnswer { id, value, error } => {
                if let Some(reply) = self.pending_app.remove(&id) {
                    let _ = reply.send(match error {
                        None => Ok(value.unwrap_or(serde_json::Value::Null)),
                        Some(error) => Err(error),
                    });
                }
            }
            FromExt::DescribeDevice { id, .. }
            | FromExt::DescribeEntity { id, .. }
            | FromExt::RemoveDevice { id, .. }
            | FromExt::RemoveEntity { id, .. }
            | FromExt::SetAvailability { id, .. }
                if !self.is_protocol =>
            {
                let _ = self.out_tx.send(ToExt::Reply {
                    id,
                    error: Some(
                        "this extension contributes no protocol, so it has no devices".into(),
                    ),
                });
            }
            FromExt::StateReport { .. }
            | FromExt::SetWaiting { .. }
            | FromExt::SetAvailableActions { .. }
                if !self.is_protocol =>
            {
                tracing::warn!(%extension, "ignored a protocol message from an extension with no protocol");
            }
            other => return Some(other),
        }
        None
    }
}

async fn next_event(
    events: &mut Option<broadcast::Receiver<Event>>,
) -> Result<Event, broadcast::error::RecvError> {
    match events {
        Some(events) => events.recv().await,
        None => std::future::pending().await,
    }
}

/// A service call, with the error given the code the spec promises at its start.
async fn call(
    core: &Core,
    entity_id: &irori_types::EntityId,
    command: Command,
    context: Context,
) -> Result<(), String> {
    core.call_service(entity_id, command, context)
        .await
        .map_err(|error| {
            let code = match &error {
                CallError::UnknownEntity(_) => "unknown_entity",
                CallError::NotSupported(_) => "not_supported",
                CallError::NotRunning(_) => "not_running",
                CallError::Unavailable(_) => "unavailable",
                CallError::Failed(_) => "failed",
                CallError::Timeout => "timeout",
            };
            format!("{code}: {error}")
        })
}

/// The extension's page, as the shell lists it.
pub(crate) fn app_info(manifest: &ExtensionManifest) -> Option<AppInfo> {
    manifest.app().map(|app| AppInfo {
        label: app.label.clone(),
        entry: app.entry.clone(),
        api: manifest.permissions.api.clone(),
    })
}

/// Environment for the extension's process beyond `IRORI_EXTENSION_DATA`: the config directory,
/// when its permissions name it.
pub(crate) fn process_env(core: &Core, manifest: &ExtensionManifest) -> Vec<(String, PathBuf)> {
    let wants_config = manifest.permissions.host_fs.iter().any(|path| {
        let path = path.as_str();
        path == "$CONFIG" || path.starts_with("$CONFIG/")
    });
    match core.config_dir() {
        Some(dir) if wants_config => {
            let dir = std::path::absolute(&dir).unwrap_or(dir);
            vec![("IRORI_CONFIG_DIR".to_owned(), dir)]
        }
        _ => Vec::new(),
    }
}
