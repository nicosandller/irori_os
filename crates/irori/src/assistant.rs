//! The assistant's files, its remembered conversations, and the call out to a model.
//!
//! Weights are not in this binary. A local model is an Ollama tag served by an Ollama already
//! listening on `127.0.0.1:11434`. A cloud model is an endpoint and a key kept in `secrets.toml`.

use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt as _;
use irori_assist::{
    AnthropicParser, AssistantFile, BriefLine, CloudPreset, DEFAULT_TAG, Mode, OllamaParser,
    OpenAiParser, Piece, Role, ToolCall, Turn as Remembered, anthropic_tools, assemble,
    cloud_ready, device_brief, execute_round, home_brief, library_page, openai_tools,
};
use irori_core::Core;
use irori_types::{Availability, DeviceId, EntityId, EntityState, ExtensionId};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::config::{Config, EditError};
use crate::history::History;

const OLLAMA: &str = "http://127.0.0.1:11434";
const ASSISTANT: &str = "assistant";
/// Sent internally when the page has gone. It is not shown, and the turn is not stored.
const CLIENT_LEFT: &str = "client left";

/// What the Settings page and the sidebar need. The key itself is never in here.
#[derive(Debug, Serialize)]
pub struct Status {
    pub ready: bool,
    pub mode: Mode,
    pub model: Option<String>,
    pub detail: String,
    /// `"set"` or `"missing"`.
    pub credential: &'static str,
    pub local_tag: String,
    pub library: String,
    pub library_index: &'static str,
    pub ollama: &'static str,
    pub pulled: Vec<Pulled>,
    pub preset: CloudPreset,
    pub base_url: String,
    pub cloud_model: String,
    /// Whether a cloud answer leaves the house.
    pub cloud: bool,
}

#[derive(Debug, Serialize)]
pub struct Pulled {
    pub name: String,
    pub size: u64,
    pub active: bool,
}

#[derive(Debug)]
pub enum ChatEvent {
    Delta(String),
    Error(String),
    Done,
}

struct Env<'a> {
    core: &'a Core,
    config: &'a Config,
    history: &'a History,
    db: &'a Path,
}

async fn status(env: &Env<'_>, data_dir: &Path) -> Status {
    let file = load_file(&env.config.dir().await);
    let has_key = env.config.assistant_key().await.is_some();
    let (ollama_up, pulled) = match tags().await {
        Ok(models) => (true, models),
        Err(_) => (false, Vec::new()),
    };
    let memory_free = crate::host_info::read(data_dir).memory_free();
    let active = pulled.iter().find(|model| model.name == file.local.tag);
    let fits = active
        .map(|model| irori_assist::local_fits(model.size, memory_free))
        .unwrap_or(false);
    let local_ready = file.mode == Mode::Local && ollama_up && active.is_some() && fits;
    let cloud = cloud_ready(&file, has_key);
    let ready = local_ready || cloud;
    let detail = if ready {
        match file.mode {
            Mode::Local => format!("{} is running on this machine.", file.local.tag),
            Mode::Cloud => format!(
                "Answers come from {} at {}. What you ask, including device names and states, leaves the house.",
                file.cloud.model, file.cloud.base_url
            ),
            Mode::Off => String::new(),
        }
    } else {
        match file.mode {
            Mode::Off => "Choose a model on this machine, or a cloud API.".to_owned(),
            Mode::Local if !ollama_up => format!(
                "Ollama isn't running on this machine. Install it from https://ollama.com/download so it listens on 127.0.0.1:11434, then download {DEFAULT_TAG}, or use a cloud API."
            ),
            Mode::Local if active.is_none() => {
                format!("Download {} to this machine.", file.local.tag)
            }
            Mode::Local => {
                let size = active.map(|model| model.size).unwrap_or(0);
                format!(
                    "{} needs more free memory than this machine has (the model is {}, about {} free).",
                    file.local.tag,
                    bytes(size),
                    bytes(memory_free)
                )
            }
            Mode::Cloud if !has_key => "A cloud model needs its key.".to_owned(),
            Mode::Cloud if file.cloud.model.is_empty() => "Name the cloud model.".to_owned(),
            Mode::Cloud => "A cloud model needs an endpoint.".to_owned(),
        }
    };
    let model = if local_ready {
        Some(file.local.tag.clone())
    } else if cloud {
        Some(file.cloud.model.clone())
    } else {
        None
    };
    Status {
        ready,
        mode: file.mode,
        model,
        detail,
        credential: if has_key { "set" } else { "missing" },
        library: library_page(&file.local.tag),
        local_tag: file.local.tag.clone(),
        library_index: "https://ollama.com/library",
        ollama: if ollama_up { "up" } else { "down" },
        pulled: pulled
            .into_iter()
            .map(|model| Pulled {
                active: model.name == file.local.tag,
                name: model.name,
                size: model.size,
            })
            .collect(),
        preset: file.cloud.preset,
        base_url: file.cloud.base_url,
        cloud_model: file.cloud.model,
        cloud: file.mode == Mode::Cloud,
    }
}

pub async fn save(config: &Config, core: &Core, body: &Value) -> Result<AssistantFile, String> {
    let mut file = load_file(&config.dir().await);
    if let Some(mode) = body.get("mode").and_then(|value| value.as_str()) {
        file.mode = match mode {
            "off" => Mode::Off,
            "local" => Mode::Local,
            "cloud" => Mode::Cloud,
            _ => return Err("mode is off, local, or cloud".to_owned()),
        };
    }
    if let Some(tag) = body.get("local_tag").and_then(|value| value.as_str()) {
        file.local.tag = tag.to_owned();
    }
    if let Some(preset) = body.get("preset").and_then(|value| value.as_str()) {
        file.cloud.preset = match preset {
            "openai" => CloudPreset::Openai,
            "anthropic" => CloudPreset::Anthropic,
            "grok" => CloudPreset::Grok,
            "compatible" => CloudPreset::Compatible,
            _ => return Err("preset is openai, anthropic, grok, or compatible".to_owned()),
        };
        if body
            .get("base_url")
            .and_then(|value| value.as_str())
            .is_none()
        {
            file.cloud.base_url = file.cloud.preset.default_base_url().to_owned();
        }
    }
    if let Some(url) = body.get("base_url").and_then(|value| value.as_str()) {
        file.cloud.base_url = url.to_owned();
    }
    if let Some(model) = body.get("model").and_then(|value| value.as_str()) {
        file.cloud.model = model.to_owned();
    }
    let file = file.validated()?;
    write_file(&config.dir().await, &file)?;
    if let Some(key) = body.get("api_key").and_then(|value| value.as_str()) {
        set_key(config, core, key).await?;
    }
    Ok(file)
}

pub fn transcript(db: &Path, scope: &str) -> Result<Vec<Remembered>, String> {
    check_scope(scope)?;
    load_turns(db, scope)
}

pub fn clear(db: &Path, scope: &str) -> Result<(), String> {
    check_scope(scope)?;
    let conn = open_db(db)?;
    conn.execute("DELETE FROM assistant_message WHERE scope = ?1", [scope])
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Pulls `tag` through the local Ollama and remembers it as the on-device model.
pub async fn pull(config: &Config, tag: &str, tx: mpsc::Sender<ChatEvent>) {
    let tag = tag.trim();
    if tag.is_empty() {
        let _ = tx
            .send(ChatEvent::Error("Name the model to download.".into()))
            .await;
        return;
    }
    let client = client();
    let response = client
        .post(format!("{OLLAMA}/api/pull"))
        .json(&json!({ "model": tag, "stream": true }))
        .send()
        .await;
    let response = match response {
        Ok(response) if response.status().is_success() => response,
        Ok(response) => {
            let _ = tx
                .send(ChatEvent::Error(format!(
                    "Ollama refused the download ({}).",
                    response.status()
                )))
                .await;
            return;
        }
        Err(_) => {
            let _ = tx
                .send(ChatEvent::Error(
                    "Ollama isn't running on this machine, so there's nowhere to download the model."
                        .into(),
                ))
                .await;
            return;
        }
    };
    let mut stream = response.bytes_stream();
    let mut failed = false;
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            failed = true;
            break;
        };
        let text = String::from_utf8_lossy(&chunk);
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let Some(error) = value.get("error").and_then(|error| error.as_str()) {
                let _ = tx.send(ChatEvent::Error(error.to_owned())).await;
                return;
            }
            if tx.send(ChatEvent::Delta(value.to_string())).await.is_err() {
                return;
            }
        }
    }
    if failed {
        let _ = tx
            .send(ChatEvent::Error("The download stopped early.".into()))
            .await;
        return;
    }
    let mut file = load_file(&config.dir().await);
    file.mode = Mode::Local;
    file.local.tag = tag.to_owned();
    if let Ok(file) = file.validated() {
        let _ = write_file(&config.dir().await, &file);
    }
    let _ = tx.send(ChatEvent::Done).await;
}

async fn answer(env: Env<'_>, scope: String, message: String, tx: mpsc::Sender<ChatEvent>) {
    if let Err(error) = check_scope(&scope) {
        let _ = tx.send(ChatEvent::Error(error)).await;
        return;
    }
    let message = message.trim().to_owned();
    if message.is_empty() {
        let _ = tx
            .send(ChatEvent::Error("Write something to ask.".into()))
            .await;
        return;
    }
    if message.len() > 8_000 {
        let _ = tx
            .send(ChatEvent::Error("That message is too long.".into()))
            .await;
        return;
    }
    let file = load_file(&env.config.dir().await);
    let key = env.config.assistant_key().await.unwrap_or_default();
    let data_dir = env.db.parent().unwrap_or(env.db);
    let picture = status(&env, data_dir).await;
    if !picture.ready {
        let _ = tx.send(ChatEvent::Error(picture.detail)).await;
        return;
    }
    let system = match system_prompt(&env, &scope).await {
        Ok(system) => system,
        Err(error) => {
            let _ = tx.send(ChatEvent::Error(error)).await;
            return;
        }
    };
    let mut turns = match load_turns(env.db, &scope) {
        Ok(turns) => turns,
        Err(error) => {
            let _ = tx.send(ChatEvent::Error(error)).await;
            return;
        }
    };
    let mut conversation = Conversation::from_turns(&system, &turns, &message);
    let provider = match provider_for(&file, &key) {
        Ok(provider) => provider,
        Err(error) => {
            let _ = tx.send(ChatEvent::Error(error)).await;
            return;
        }
    };
    let mut rounds = 0u8;
    let text = loop {
        if tx.is_closed() {
            return;
        }
        let with_tools = execute_round(rounds);
        match complete(&provider, &conversation, with_tools, &tx).await {
            Ok(Outcome::Text(text)) => break text,
            Ok(Outcome::Tools(calls)) if with_tools => {
                for call in calls {
                    let result = run_tool(&env, &call);
                    conversation.tool(&call, &result);
                }
                rounds += 1;
            }
            Ok(Outcome::Tools(_)) => break String::new(),
            Err(error) if error == CLIENT_LEFT => return,
            Err(error) => {
                let _ = tx.send(ChatEvent::Error(scrub(&error, &key))).await;
                return;
            }
        }
    };
    if tx.is_closed() {
        return;
    }
    if text.is_empty() {
        let _ = tx
            .send(ChatEvent::Error("The model didn't answer.".into()))
            .await;
        return;
    }
    irori_assist::append_capped(
        &mut turns,
        Remembered {
            role: Role::User,
            body: message,
        },
    );
    irori_assist::append_capped(
        &mut turns,
        Remembered {
            role: Role::Assistant,
            body: text,
        },
    );
    if let Err(error) = store_turns(env.db, &scope, &turns) {
        let _ = tx.send(ChatEvent::Error(error)).await;
        return;
    }
    let _ = tx.send(ChatEvent::Done).await;
}

enum Outcome {
    Text(String),
    Tools(Vec<ToolCall>),
}

enum Provider {
    OpenAi {
        url: String,
        key: String,
        model: String,
    },
    Anthropic {
        url: String,
        key: String,
        model: String,
    },
    Ollama {
        model: String,
        think: bool,
    },
}

fn provider_for(file: &AssistantFile, key: &str) -> Result<Provider, String> {
    match file.mode {
        Mode::Local => Ok(Provider::Ollama {
            think: file.local.tag == DEFAULT_TAG,
            model: file.local.tag.clone(),
        }),
        Mode::Cloud if file.cloud.preset.is_anthropic() => Ok(Provider::Anthropic {
            url: format!("{}/v1/messages", file.cloud.base_url),
            key: key.to_owned(),
            model: file.cloud.model.clone(),
        }),
        Mode::Cloud => Ok(Provider::OpenAi {
            url: format!("{}/chat/completions", file.cloud.base_url),
            key: key.to_owned(),
            model: file.cloud.model.clone(),
        }),
        Mode::Off => Err("Choose a model in Settings.".into()),
    }
}

struct Conversation {
    system: String,
    items: Vec<Item>,
}

enum Item {
    User(String),
    Assistant(String),
    Call(ToolCall),
    Result {
        id: String,
        name: String,
        body: String,
    },
}

impl Conversation {
    fn from_turns(system: &str, turns: &[Remembered], message: &str) -> Self {
        let mut items = Vec::new();
        for turn in turns {
            items.push(match turn.role {
                Role::User => Item::User(turn.body.clone()),
                Role::Assistant => Item::Assistant(turn.body.clone()),
            });
        }
        items.push(Item::User(message.to_owned()));
        Self {
            system: system.to_owned(),
            items,
        }
    }

    fn tool(&mut self, call: &ToolCall, result: &str) {
        self.items.push(Item::Call(call.clone()));
        self.items.push(Item::Result {
            id: call.id.clone(),
            name: call.name.clone(),
            body: result.chars().take(4_000).collect(),
        });
    }
}

async fn complete(
    provider: &Provider,
    conversation: &Conversation,
    with_tools: bool,
    tx: &mpsc::Sender<ChatEvent>,
) -> Result<Outcome, String> {
    let client = client();
    let (request, key) = match provider {
        Provider::OpenAi { url, key, model } => {
            let mut body = json!({
                "model": model,
                "stream": true,
                "messages": openai_messages(conversation),
            });
            if with_tools {
                body["tools"] = openai_tools();
            }
            (client.post(url).bearer_auth(key).json(&body), key.as_str())
        }
        Provider::Anthropic { url, key, model } => {
            let mut body = json!({
                "model": model,
                "max_tokens": 1024,
                "stream": true,
                "system": conversation.system,
                "messages": anthropic_messages(conversation),
            });
            if with_tools {
                body["tools"] = anthropic_tools();
            }
            (
                client
                    .post(url)
                    .header("x-api-key", key)
                    .header("anthropic-version", "2023-06-01")
                    .json(&body),
                key.as_str(),
            )
        }
        Provider::Ollama { model, think } => {
            let mut body = json!({
                "model": model,
                "stream": true,
                "messages": ollama_messages(conversation),
            });
            if *think {
                body["think"] = json!(false);
            }
            if with_tools {
                body["tools"] = openai_tools();
            }
            (client.post(format!("{OLLAMA}/api/chat")).json(&body), "")
        }
    };
    let response = request.send().await.map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "the model refused ({status}): {}",
            scrub(&body, key)
        ));
    }
    let mut stream = response.bytes_stream();
    let mut text = String::new();
    let mut parts = Vec::new();
    let mut forwarding = false;
    let mut openai = OpenAiParser::default();
    let mut ollama = OllamaParser::default();
    let mut anthropic = AnthropicParser::default();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| error.to_string())?;
        let chunk = String::from_utf8_lossy(&chunk);
        let pieces: Vec<Piece> = match provider {
            Provider::OpenAi { .. } => openai.push(&chunk),
            Provider::Ollama { .. } => ollama.push(&chunk),
            Provider::Anthropic { .. } => anthropic.push(&chunk),
        };
        for piece in pieces {
            match piece {
                Piece::Text(delta) => {
                    if parts.is_empty() {
                        forwarding = true;
                        text.push_str(&delta);
                        if tx.send(ChatEvent::Delta(delta)).await.is_err() {
                            return Err(CLIENT_LEFT.to_owned());
                        }
                    }
                }
                Piece::Tool {
                    index,
                    id,
                    name,
                    arguments,
                } => {
                    if !forwarding {
                        parts.push((index, id, name, arguments));
                    }
                }
                Piece::Done => {}
            }
        }
    }
    if !parts.is_empty() && !forwarding {
        return Ok(Outcome::Tools(assemble(
            &parts
                .iter()
                .map(|(index, id, name, arguments)| {
                    (*index, id.clone(), name.clone(), arguments.clone())
                })
                .collect::<Vec<_>>(),
        )));
    }
    Ok(Outcome::Text(text))
}

fn openai_messages(conversation: &Conversation) -> Vec<Value> {
    let mut messages = vec![json!({"role": "system", "content": conversation.system})];
    for item in &conversation.items {
        messages.push(match item {
            Item::User(text) => json!({"role": "user", "content": text}),
            Item::Assistant(text) => json!({"role": "assistant", "content": text}),
            Item::Call(call) => json!({
                "role": "assistant",
                "content": Value::Null,
                "tool_calls": [{
                    "id": call.id,
                    "type": "function",
                    "function": { "name": call.name, "arguments": call.arguments }
                }]
            }),
            Item::Result { id, body, .. } => {
                json!({"role": "tool", "tool_call_id": id, "content": body})
            }
        });
    }
    messages
}

fn ollama_messages(conversation: &Conversation) -> Vec<Value> {
    let mut messages = vec![json!({"role": "system", "content": conversation.system})];
    for item in &conversation.items {
        messages.push(match item {
            Item::User(text) => json!({"role": "user", "content": text}),
            Item::Assistant(text) => json!({"role": "assistant", "content": text}),
            Item::Call(call) => json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "function": {
                        "name": call.name,
                        "arguments": serde_json::from_str::<Value>(&call.arguments).unwrap_or(json!({}))
                    }
                }]
            }),
            Item::Result { name, body, .. } => {
                json!({"role": "tool", "tool_name": name, "content": body})
            }
        });
    }
    messages
}

fn anthropic_messages(conversation: &Conversation) -> Vec<Value> {
    let mut messages = Vec::new();
    for item in &conversation.items {
        match item {
            Item::User(text) => messages.push(json!({"role": "user", "content": text})),
            Item::Assistant(text) => messages.push(json!({"role": "assistant", "content": text})),
            Item::Call(call) => messages.push(json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": call.id,
                    "name": call.name,
                    "input": serde_json::from_str::<Value>(&call.arguments).unwrap_or(json!({}))
                }]
            })),
            Item::Result { id, body, .. } => messages.push(json!({
                "role": "user",
                "content": [{ "type": "tool_result", "tool_use_id": id, "content": body }]
            })),
        }
    }
    messages
}

fn run_tool(env: &Env<'_>, call: &ToolCall) -> String {
    let args: Value = serde_json::from_str(&call.arguments).unwrap_or(json!({}));
    let text = match call.name.as_str() {
        "list_devices" => home_brief(&lines(env.core, None)),
        "get_device" => {
            let Some(id) = args.get("id").and_then(|id| id.as_str()) else {
                return "get_device needs an id.".to_owned();
            };
            let Ok(id) = id.parse::<DeviceId>() else {
                return format!("there's no device `{id}`");
            };
            match env
                .core
                .devices()
                .into_iter()
                .find(|device| device.id == id)
            {
                Some(device) => {
                    let area = device
                        .area_id
                        .as_ref()
                        .and_then(|area| {
                            env.core.areas().into_iter().find(|known| &known.id == area)
                        })
                        .map(|area| area.name.to_string())
                        .unwrap_or_default();
                    device_brief(
                        device.name.as_ref(),
                        &area,
                        &lines(env.core, Some(&device.id)),
                    )
                }
                None => format!("there's no device `{id}`"),
            }
        }
        "recent_states" => {
            let Some(id) = args.get("entity_id").and_then(|id| id.as_str()) else {
                return "recent_states needs an entity_id.".to_owned();
            };
            let Ok(id) = id.parse::<EntityId>() else {
                return format!("there's no entity `{id}`");
            };
            let states = env.history.for_entity(&id);
            if states.is_empty() {
                format!("{id} has not reported anything since Irori started.")
            } else {
                states
                    .iter()
                    .rev()
                    .take(15)
                    .map(shown)
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        other => format!("there is no tool `{other}`"),
    };
    text.chars().take(4_000).collect()
}

async fn system_prompt(env: &Env<'_>, scope: &str) -> Result<String, String> {
    let mut prompt = String::from(
        "You are Irori's assistant. Talk about this home in plain words. \
         You can read devices and recent values. You cannot change them.\n",
    );
    if scope == "general" {
        prompt.push_str(&home_brief(&lines(env.core, None)));
        return Ok(prompt);
    }
    if let Some(id) = scope.strip_prefix("device:") {
        let id: DeviceId = id
            .parse()
            .map_err(|_| format!("there's no device `{id}`"))?;
        let device = env
            .core
            .devices()
            .into_iter()
            .find(|device| device.id == id)
            .ok_or_else(|| format!("there's no device `{id}`"))?;
        let area = device
            .area_id
            .as_ref()
            .and_then(|area| env.core.areas().into_iter().find(|known| &known.id == area))
            .map(|area| area.name.to_string())
            .unwrap_or_default();
        prompt.push_str(&device_brief(
            device.name.as_ref(),
            &area,
            &lines(env.core, Some(&device.id)),
        ));
        return Ok(prompt);
    }
    if let Some(id) = scope.strip_prefix("automation:") {
        let extension =
            ExtensionId::try_from("automations").expect("automations is an extension id");
        let brief = match env
            .core
            .app_request(&extension, "flow.brief".to_owned(), json!({ "id": id }))
            .await
        {
            Ok(value) => value["text"].as_str().unwrap_or("").to_owned(),
            Err(error) => format!("Automations isn't available ({error})."),
        };
        prompt.push_str("The person is asking about one automation.\n");
        prompt.push_str(&brief);
        return Ok(prompt);
    }
    Err("that conversation doesn't exist".into())
}

fn lines(core: &Core, only: Option<&DeviceId>) -> Vec<BriefLine> {
    let devices = core.devices();
    let areas = core.areas();
    let entities = core.entities();
    let states = core.states();
    let mut lines = Vec::new();
    for device in &devices {
        if only.is_some_and(|id| &device.id != id) {
            continue;
        }
        let area = device
            .area_id
            .as_ref()
            .and_then(|id| areas.iter().find(|area| &area.id == id))
            .map(|area| area.name.to_string())
            .unwrap_or_default();
        let mut any = false;
        for entity in entities
            .iter()
            .filter(|entity| entity.device_id.as_ref() == Some(&device.id))
        {
            any = true;
            let value = states
                .iter()
                .find(|state| state.entity_id == entity.id)
                .map(shown)
                .unwrap_or_else(|| "unknown".to_owned());
            lines.push(BriefLine {
                area: area.clone(),
                name: device.name.to_string(),
                entity: entity.id.to_string(),
                value,
            });
        }
        if !any {
            lines.push(BriefLine {
                area,
                name: device.name.to_string(),
                entity: device.id.to_string(),
                value: "no entities yet".into(),
            });
        }
    }
    lines
}

fn shown(state: &EntityState) -> String {
    if state.availability == Availability::Unavailable {
        return "unavailable".to_owned();
    }
    match &state.state {
        None => "unknown".to_owned(),
        Some(value) => {
            let text = serde_json::to_string(value).unwrap_or_else(|_| "unknown".to_owned());
            text.chars().take(160).collect()
        }
    }
}

struct RemoteModel {
    name: String,
    size: u64,
}

async fn tags() -> Result<Vec<RemoteModel>, ()> {
    let response = client()
        .get(format!("{OLLAMA}/api/tags"))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .map_err(|_| ())?;
    if !response.status().is_success() {
        return Err(());
    }
    let value: Value = response.json().await.map_err(|_| ())?;
    let Some(models) = value.get("models").and_then(|models| models.as_array()) else {
        return Ok(Vec::new());
    };
    Ok(models
        .iter()
        .filter_map(|model| {
            Some(RemoteModel {
                name: model.get("name")?.as_str()?.to_owned(),
                size: model.get("size")?.as_u64()?,
            })
        })
        .collect())
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

fn load_file(dir: &Path) -> AssistantFile {
    let path = dir.join("assistant.toml");
    match std::fs::read_to_string(&path) {
        Ok(text) => AssistantFile::parse(&text).unwrap_or_else(|error| {
            tracing::error!(file = "assistant.toml", %error, "config file ignored");
            AssistantFile::default()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => AssistantFile::default(),
        Err(error) => {
            tracing::error!(file = "assistant.toml", %error, "config file ignored");
            AssistantFile::default()
        }
    }
}

fn write_file(dir: &Path, file: &AssistantFile) -> Result<(), String> {
    let text = file.to_toml()?;
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let path = dir.join("assistant.toml");
    let temporary = dir.join("assistant.toml.writing");
    std::fs::write(&temporary, text).map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, &path).map_err(|error| error.to_string())?;
    Ok(())
}

async fn set_key(config: &Config, core: &Core, key: &str) -> Result<(), String> {
    let id = ExtensionId::try_from(ASSISTANT).map_err(|error| error.to_string())?;
    config
        .edit_secrets(core, |secrets| {
            if key.is_empty() {
                secrets.remove(&id, &["api_key".to_owned()]);
            } else {
                secrets
                    .set(&id, &["api_key".to_owned()], key.to_owned())
                    .map_err(|error| crate::config::Refused(error.to_string()))?;
            }
            Ok(())
        })
        .await
        .map_err(|error| match error {
            EditError::Refused(refused) => refused.0,
            EditError::Io(error) => error.to_string(),
        })
}

fn check_scope(scope: &str) -> Result<(), String> {
    if scope == "general" {
        return Ok(());
    }
    let Some((kind, id)) = scope.split_once(':') else {
        return Err("a conversation is general, a device, or an automation".into());
    };
    let ok = matches!(kind, "device" | "automation")
        && !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
    if ok {
        Ok(())
    } else {
        Err("that conversation doesn't exist".into())
    }
}

fn open_db(db: &Path) -> Result<rusqlite::Connection, String> {
    let conn = rusqlite::Connection::open(db).map_err(|error| error.to_string())?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS assistant_message (
            scope TEXT NOT NULL,
            seq INTEGER NOT NULL,
            role TEXT NOT NULL,
            body TEXT NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (scope, seq)
        )",
    )
    .map_err(|error| error.to_string())?;
    Ok(conn)
}

fn load_turns(db: &Path, scope: &str) -> Result<Vec<Remembered>, String> {
    let conn = open_db(db)?;
    let mut statement = conn
        .prepare("SELECT role, body FROM assistant_message WHERE scope = ?1 ORDER BY seq")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([scope], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| error.to_string())?;
    let mut turns = Vec::new();
    for row in rows {
        let (role, body) = row.map_err(|error| error.to_string())?;
        let Some(role) = Role::parse(&role) else {
            continue;
        };
        turns.push(Remembered { role, body });
    }
    Ok(turns)
}

fn store_turns(db: &Path, scope: &str, turns: &[Remembered]) -> Result<(), String> {
    let mut conn = open_db(db)?;
    let tx = conn.transaction().map_err(|error| error.to_string())?;
    tx.execute("DELETE FROM assistant_message WHERE scope = ?1", [scope])
        .map_err(|error| error.to_string())?;
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    for (seq, turn) in turns.iter().enumerate() {
        tx.execute(
            "INSERT INTO assistant_message (scope, seq, role, body, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                scope,
                seq as i64,
                turn.role.as_str(),
                turn.body.as_str(),
                format!("{created:020}"),
            ),
        )
        .map_err(|error| error.to_string())?;
    }
    trim_global(&tx)?;
    tx.commit().map_err(|error| error.to_string())?;
    Ok(())
}

fn trim_global(tx: &rusqlite::Transaction<'_>) -> Result<(), String> {
    loop {
        let total: i64 = tx
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(body)), 0) FROM assistant_message",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if total <= irori_assist::LIMIT_GLOBAL_BYTES as i64 {
            return Ok(());
        }
        let removed = tx
            .execute(
                "DELETE FROM assistant_message WHERE rowid = (
                    SELECT rowid FROM assistant_message ORDER BY created_at, scope, seq LIMIT 1
                 )",
                [],
            )
            .map_err(|error| error.to_string())?;
        if removed == 0 {
            return Ok(());
        }
    }
}

fn scrub(text: &str, key: &str) -> String {
    let text = if key.is_empty() {
        text.to_owned()
    } else {
        text.replace(key, "…")
    };
    text.chars().take(400).collect()
}

fn bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if n >= GB {
        format!("{:.1} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{} MB", n / MB)
    } else {
        format!("{n} B")
    }
}

/// Free RAM, for deciding whether a pulled model can be served.
trait MemoryFree {
    fn memory_free(&self) -> u64;
}

impl MemoryFree for crate::host_info::HostView {
    fn memory_free(&self) -> u64 {
        self.memory_total.saturating_sub(self.memory_used)
    }
}

/// The pieces a turn needs. Split out so the HTTP layer stays thin.
pub struct Turn<'a> {
    pub core: &'a Core,
    pub config: &'a Config,
    pub history: &'a History,
    pub db: &'a Path,
}

impl<'a> Turn<'a> {
    fn env(&self) -> Env<'a> {
        Env {
            core: self.core,
            config: self.config,
            history: self.history,
            db: self.db,
        }
    }
}

pub async fn read_status(turn: Turn<'_>, data_dir: &Path) -> Status {
    let env = turn.env();
    status(&env, data_dir).await
}

pub async fn take_turn(
    turn: Turn<'_>,
    scope: String,
    message: String,
    tx: mpsc::Sender<ChatEvent>,
) {
    answer(turn.env(), scope, message, tx).await
}
