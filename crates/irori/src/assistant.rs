//! The assistant's files, its remembered conversations, and the call out to a model.
//!
//! Weights are not in this binary. A local model is an Ollama tag served by the Ollama
//! listening on `127.0.0.1:11434`: one the person installed, or the one Irori downloads on
//! request (`ollama.rs`). A cloud model is an endpoint and a key kept in `secrets.toml`.

use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt as _;
use irori_assist::{
    AnthropicParser, AssistantFile, BriefLine, CloudPreset, DEFAULT_TAG, LOCAL_CONTEXT, Lines,
    Mode, OllamaParser, OpenAiParser, Piece, Role, Shape, ToolCall, Turn as Remembered,
    anthropic_tools, assemble, cloud_ready, device_brief, execute_round, home_brief, library_page,
    model_tag, openai_tools,
};
use irori_core::Core;
use irori_types::{Availability, DeviceId, EntityId, EntityState, ExtensionId};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::config::{Config, EditError};
use crate::history::History;
use crate::ollama;

const OLLAMA: &str = "http://127.0.0.1:11434";
const ASSISTANT: &str = "assistant";
/// Sent internally when the page has gone. It is not shown, and the turn is not stored.
const CLIENT_LEFT: &str = "client left";
/// How long a model or a download may say nothing before it is given up on. Long, because a
/// small machine can take minutes to load a model before the first word.
const QUIET: Duration = Duration::from_secs(300);
/// The same, for a model on this machine. It says nothing at all while it reads the question,
/// and on a Pi that reading alone takes minutes.
const QUIET_LOCAL: Duration = Duration::from_secs(900);
const WENT_QUIET: &str = "the model stopped answering";
/// How long Ollama keeps a model loaded after it was last used: until it is told to let go.
/// Loaded is what "ready" means for a local model, so it must not lapse on its own.
const KEEP_LOADED: i64 = -1;
/// How much earlier conversation a local model is sent. Its whole context is
/// [`LOCAL_CONTEXT`] tokens, and the picture of the home takes about half.
const LOCAL_HISTORY_BYTES: usize = 3_000;

/// One question at a time for the model on this machine. Ollama would queue a second one
/// anyway; holding it here lets the page say so, and keeps its wait off the clock.
static LOCAL_TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    /// Whether the Ollama on this machine is the one Irori installed, and so can remove.
    pub managed: bool,
    pub pulled: Vec<Pulled>,
    pub preset: CloudPreset,
    pub base_url: String,
    pub cloud_model: String,
    /// Whether a cloud answer leaves the house.
    pub cloud: bool,
    /// Memory a model could be loaded into right now, and how much the machine has.
    pub memory_free: u64,
    pub memory_total: u64,
}

#[derive(Debug, Serialize)]
pub struct Pulled {
    pub name: String,
    pub size: u64,
    pub active: bool,
    /// Whether Ollama has it in memory now.
    pub loaded: bool,
    /// About how much memory it takes once loaded.
    pub needs: u64,
}

#[derive(Debug)]
pub enum ChatEvent {
    Delta(String),
    /// The model has gone to look something up. The tool's name.
    Step(String),
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
    let host = crate::host_info::read(data_dir);
    let memory_free = host.memory_free();
    let in_memory = if ollama_up {
        loaded().await
    } else {
        Vec::new()
    };
    let mut models = Vec::new();
    for model in pulled {
        let needs = irori_assist::local_needs(model.size, shape(&model).await);
        models.push(Pulled {
            active: model.name == file.local.tag,
            loaded: in_memory.contains(&model.name),
            name: model.name,
            size: model.size,
            needs,
        });
    }
    let pulled = models;
    let active = pulled.iter().find(|model| model.active);
    let fits = active
        .map(|model| irori_assist::local_fits(model.needs, memory_free, model.loaded))
        .unwrap_or(false);
    // A local model is ready when it is in memory. Downloaded is not enough: the first
    // question would otherwise wait on a load that may not fit, or take minutes.
    let is_loaded = active.is_some_and(|model| model.loaded);
    let local_ready = file.mode == Mode::Local && ollama_up && is_loaded;
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
                "Ollama isn't running on this machine yet. Install and download puts it here along with {DEFAULT_TAG}."
            ),
            Mode::Local if active.is_none() => {
                format!("Download {} to this machine.", file.local.tag)
            }
            Mode::Local if fits => format!(
                "{} is downloaded but not loaded. Load it to use it.",
                file.local.tag
            ),
            Mode::Local => {
                let needs = active.map(|model| model.needs).unwrap_or(0);
                format!(
                    "{} takes about {} of memory once loaded, and {} is free. Free some, or use a smaller model.",
                    file.local.tag,
                    bytes(needs),
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
        managed: ollama::installed(data_dir),
        pulled,
        memory_free,
        memory_total: host.memory_total,
        preset: file.cloud.preset,
        base_url: file.cloud.base_url,
        cloud_model: file.cloud.model,
        cloud: file.mode == Mode::Cloud,
    }
}

pub async fn save(
    config: &Config,
    core: &Core,
    data_dir: &Path,
    body: &Value,
) -> Result<AssistantFile, String> {
    let mut file = load_file(&config.dir().await);
    let before = (file.mode, file.local.tag.clone());
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
    // The model that stops being the one in use gives its memory back now: two of them
    // don't fit on a small machine.
    if before.0 == Mode::Local
        && (file.mode != Mode::Local || file.local.tag != before.1)
        && loaded().await.contains(&before.1)
    {
        let _ = hold(data_dir, &before.1, false).await;
    }
    if let Some(key) = body.get("api_key").and_then(|value| value.as_str()) {
        set_key(config, core, key).await?;
    }
    // Choosing a downloaded model is asking to use it, so it is loaded. If it won't fit, the
    // card says it is not loaded, and Load says why.
    if file.mode == Mode::Local && (before.0 != Mode::Local || file.local.tag != before.1) {
        let _ = hold(data_dir, &file.local.tag, true).await;
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

/// Makes sure an Ollama is answering: Irori's own is installed if none is on the machine,
/// and started if it isn't running. Progress and failure go to `tx`.
async fn ready_ollama(data_dir: &Path, tx: &mpsc::Sender<ChatEvent>) -> bool {
    if ollama::listening().await {
        return true;
    }
    {
        if !ollama::installed(data_dir) {
            let free = crate::host_info::read(data_dir).disk.available;
            let mut last = 0u64;
            let mut report = |done: u64, total: u64| {
                // A line every couple of megabytes is plenty for a progress bar.
                if done - last >= 2 * 1024 * 1024 || done == total {
                    last = done;
                    let line = json!({
                        "status": "installing Ollama",
                        "completed": done,
                        "total": total,
                    });
                    let _ = tx.try_send(ChatEvent::Delta(line.to_string()));
                }
                !tx.is_closed()
            };
            if let Err(error) = ollama::install(data_dir, free, &mut report).await {
                let _ = tx.send(ChatEvent::Error(error)).await;
                return false;
            }
        }
        let line = json!({ "status": "starting Ollama" });
        let _ = tx.send(ChatEvent::Delta(line.to_string())).await;
        match ollama::start(data_dir).await {
            Ok(true) => true,
            Ok(false) => {
                let _ = tx
                    .send(ChatEvent::Error(
                        "Ollama isn't running on this machine, so there's nowhere to download the model."
                            .into(),
                    ))
                    .await;
                false
            }
            Err(error) => {
                let _ = tx.send(ChatEvent::Error(error)).await;
                false
            }
        }
    }
}

/// Installs and starts Irori's own Ollama, without downloading a model.
pub async fn install(data_dir: &Path, tx: mpsc::Sender<ChatEvent>) {
    if ready_ollama(data_dir, &tx).await {
        let _ = tx.send(ChatEvent::Done).await;
    }
}

/// Pulls `tag` through the local Ollama and remembers it as the on-device model. With no
/// Ollama on the machine, Irori's own is installed and started first.
pub async fn pull(config: &Config, data_dir: &Path, tag: &str, tx: mpsc::Sender<ChatEvent>) {
    let tag = model_tag(tag);
    let tag = tag.as_str();
    if tag.is_empty() {
        let _ = tx
            .send(ChatEvent::Error("Name the model to download.".into()))
            .await;
        return;
    }
    if !ready_ollama(data_dir, &tx).await {
        return;
    }
    let client = client();
    let request = client
        .post(format!("{OLLAMA}/api/pull"))
        .json(&json!({ "model": tag, "stream": true }))
        .send();
    let response = match waited(&tx, QUIET, request).await {
        Ok(Ok(response)) if response.status().is_success() => response,
        Ok(Ok(response)) => {
            let _ = tx
                .send(ChatEvent::Error(format!(
                    "Ollama refused the download ({}).",
                    response.status()
                )))
                .await;
            return;
        }
        Err(error) if error == CLIENT_LEFT => return,
        Ok(Err(_)) | Err(_) => {
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
    let mut lines = Lines::default();
    let mut failed = false;
    loop {
        let chunk = match waited(&tx, QUIET, stream.next()).await {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(None) => break,
            Err(error) if error == CLIENT_LEFT => return,
            Ok(Some(Err(_))) | Err(_) => {
                failed = true;
                break;
            }
        };
        for line in lines.push(&chunk) {
            if line.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
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
    // Downloaded and chosen, it is loaded too, so it is ready when the bar finishes.
    let line = json!({ "status": "loading the model" });
    let _ = tx.send(ChatEvent::Delta(line.to_string())).await;
    if let Err(error) = hold(data_dir, tag, true).await {
        let _ = tx.send(ChatEvent::Error(error)).await;
        return;
    }
    let _ = tx.send(ChatEvent::Done).await;
}

/// Brings the local model back after a start: Irori's Ollama, then the model in use, when
/// that is how the assistant is set. Nothing waits on it.
pub async fn wake(config: &Config, data_dir: &Path) {
    match ollama::start(data_dir).await {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            tracing::warn!(%error, "the local model's Ollama did not start");
            return;
        }
    }
    let file = load_file(&config.dir().await);
    if file.mode == Mode::Local
        && let Err(error) = hold(data_dir, &file.local.tag, true).await
    {
        tracing::warn!(tag = %file.local.tag, %error, "the local model was not loaded");
    }
}

/// The tail of `turns` that fits in `bytes`, whole turns only and never starting on an answer.
fn recent(turns: &[Remembered], bytes: usize) -> &[Remembered] {
    let mut start = turns.len();
    let mut total = 0usize;
    while start > 0 && total + turns[start - 1].body.len() <= bytes {
        start -= 1;
        total += turns[start].body.len();
    }
    if turns
        .get(start)
        .is_some_and(|turn| turn.role == Role::Assistant)
    {
        start += 1;
    }
    &turns[start.min(turns.len())..]
}

/// Loads a downloaded model into memory ahead of the first question, or lets it go.
pub async fn hold(data_dir: &Path, tag: &str, load: bool) -> Result<(), String> {
    let tag = model_tag(tag);
    let free_before = crate::host_info::read(data_dir).memory_free();
    if load {
        let models = tags()
            .await
            .map_err(|()| "Ollama isn't running on this machine.".to_owned())?;
        let model = models
            .iter()
            .find(|model| model.name == tag)
            .ok_or_else(|| format!("{tag} hasn't been downloaded."))?;
        let needs = irori_assist::local_needs(model.size, shape(model).await);
        let free = crate::host_info::read(data_dir).memory_free();
        let already = loaded().await.contains(&tag);
        if !irori_assist::local_fits(needs, free, already) {
            return Err(format!(
                "{tag} takes about {} of memory once loaded, and {} is free.",
                bytes(needs),
                bytes(free)
            ));
        }
    }
    let mut body =
        json!({ "model": tag, "keep_alive": if load { json!(KEEP_LOADED) } else { json!(0) } });
    if load {
        body["options"] = json!({ "num_ctx": LOCAL_CONTEXT });
    }
    let response = client()
        .post(format!("{OLLAMA}/api/generate"))
        .timeout(QUIET)
        .json(&body)
        .send()
        .await
        .map_err(|_| "Ollama isn't running on this machine.".to_owned())?;
    if response.status().is_success() {
        // Ollama answers before it has finished letting go, and the status read next must
        // not catch the model half out.
        for _ in 0..40 {
            if loaded().await.contains(&tag) == load {
                if !load {
                    given_back(data_dir, free_before).await;
                }
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        return Err(if load {
            format!("{tag} was asked to load and isn't in memory.")
        } else {
            format!("{tag} was asked to unload and is still in memory.")
        });
    }
    let status = response.status();
    let said = response.text().await.unwrap_or_default();
    tracing::warn!(%tag, %status, said = %scrub(&said, ""), "the local model would not load");
    Err(explained(&format!("the model refused ({status}): {said}")))
}

/// Waits, briefly, for the memory an unloaded model held to show up as free. The system
/// takes a moment to count it, and a reading taken before then says there is no room for the
/// model that just left.
async fn given_back(data_dir: &Path, free_before: u64) {
    for _ in 0..12 {
        let free = crate::host_info::read(data_dir).memory_free();
        if free >= free_before.saturating_add(256 * 1024 * 1024) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Loads a downloaded model and makes it the one the assistant uses, whatever it was set to
/// before. Any other model in memory is let go first, so the room it held counts.
pub async fn choose(config: &Config, data_dir: &Path, tag: &str) -> Result<(), String> {
    let tag = model_tag(tag);
    for other in loaded().await {
        if other != tag {
            let _ = hold(data_dir, &other, false).await;
        }
    }
    hold(data_dir, &tag, true).await?;
    let dir = config.dir().await;
    let mut file = load_file(&dir);
    file.mode = Mode::Local;
    file.local.tag = tag;
    write_file(&dir, &file.validated()?)
}

/// Deletes a downloaded model. The one in use going turns the assistant off.
pub async fn forget(config: &Config, tag: &str) -> Result<(), String> {
    let tag = model_tag(tag);
    let response = client()
        .delete(format!("{OLLAMA}/api/delete"))
        .timeout(Duration::from_secs(30))
        .json(&json!({ "model": tag }))
        .send()
        .await
        .map_err(|_| "Ollama isn't running on this machine.".to_owned())?;
    if !response.status().is_success() && response.status() != reqwest::StatusCode::NOT_FOUND {
        return Err(format!(
            "Ollama refused to delete it ({}).",
            response.status()
        ));
    }
    let dir = config.dir().await;
    let mut file = load_file(&dir);
    if file.mode == Mode::Local && file.local.tag == tag {
        file.mode = Mode::Off;
        write_file(&dir, &file)?;
    }
    Ok(())
}

/// Removes the Ollama Irori installed, and every model it downloaded.
pub async fn uninstall(config: &Config, data_dir: &Path) -> Result<(), String> {
    if !ollama::installed(data_dir) {
        return Err("This Ollama wasn't installed by Irori, so it is yours to remove.".to_owned());
    }
    ollama::uninstall(data_dir).await?;
    let dir = config.dir().await;
    let mut file = load_file(&dir);
    if file.mode == Mode::Local {
        file.mode = Mode::Off;
        write_file(&dir, &file)?;
    }
    Ok(())
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
    let turns = match load_turns(env.db, &scope) {
        Ok(turns) => turns,
        Err(error) => {
            let _ = tx.send(ChatEvent::Error(error)).await;
            return;
        }
    };
    let provider = match provider_for(&file, &key) {
        Ok(provider) => provider,
        Err(error) => {
            let _ = tx.send(ChatEvent::Error(error)).await;
            return;
        }
    };
    let earlier = if provider.local() {
        recent(&turns, LOCAL_HISTORY_BYTES)
    } else {
        &turns[..]
    };
    // A small model on this machine is not offered the tools. It asks for the list it was
    // just given, then again, and never gets to the answer. The picture of the home is
    // already in front of it, and leaving the tools out is fewer words for it to read.
    let system = if provider.local() {
        format!(
            "{system}There are no tools in this conversation. Answer from what is written above, \
             and say so when it isn't there.\n"
        )
    } else {
        system
    };
    let mut conversation = Conversation::from_turns(&system, earlier, &message);
    let _turn = if provider.local() {
        match LOCAL_TURN.try_lock() {
            Ok(turn) => Some(turn),
            Err(_) => {
                let _ = tx.send(ChatEvent::Step("queued".into())).await;
                tokio::select! {
                    _ = tx.closed() => return,
                    turn = LOCAL_TURN.lock() => Some(turn),
                }
            }
        }
    } else {
        None
    };
    let mut rounds = 0u8;
    let mut tools_work = !provider.local();
    // Everything the page was sent, so what is remembered is what was read.
    let mut text = String::new();
    loop {
        if tx.is_closed() {
            return;
        }
        let with_tools = tools_work && execute_round(rounds);
        match complete(&provider, &conversation, with_tools, &tx).await {
            Ok(Outcome::Text(said)) => {
                text.push_str(&said);
                break;
            }
            Ok(Outcome::Tools { said, calls }) => {
                // A model often says a word before it reaches for a tool. The page already has
                // it, so the answer carries on below it.
                if !said.trim().is_empty() {
                    text.push_str(&said);
                    text.push_str("\n\n");
                    if tx.send(ChatEvent::Delta("\n\n".into())).await.is_err() {
                        return;
                    }
                    conversation.items.push(Item::Assistant(said));
                }
                for call in calls {
                    if tx.send(ChatEvent::Step(call.name.clone())).await.is_err() {
                        return;
                    }
                    let result = run_tool(&env, &call);
                    conversation.tool(&call, &result);
                }
                rounds += 1;
            }
            Err(error) if error == CLIENT_LEFT => return,
            // A small local model may not take tools at all. It still has the picture of the
            // home in its prompt, so it is asked again without them.
            Err(error) if with_tools && error.contains("does not support tools") => {
                tools_work = false;
            }
            Err(error) => {
                // In Irori's own log too, so the reason outlives the chat it was shown in.
                let error = scrub(&error, &key);
                tracing::warn!(%scope, %error, "the assistant's model failed");
                let _ = tx.send(ChatEvent::Error(explained(&error))).await;
                return;
            }
        }
    }
    let text = text.trim_end().to_owned();
    if tx.is_closed() {
        return;
    }
    if text.is_empty() {
        let _ = tx
            .send(ChatEvent::Error("The model didn't answer.".into()))
            .await;
        return;
    }
    let said = [
        Remembered {
            role: Role::User,
            body: message,
        },
        Remembered {
            role: Role::Assistant,
            body: text,
        },
    ];
    if let Err(error) = store_turns(env.db, &scope, said) {
        let _ = tx.send(ChatEvent::Error(error)).await;
        return;
    }
    let _ = tx.send(ChatEvent::Done).await;
}

enum Outcome {
    Text(String),
    /// The model wants tools run. `said` is whatever it wrote first, already sent to the page.
    Tools {
        said: String,
        calls: Vec<ToolCall>,
    },
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

impl Provider {
    fn local(&self) -> bool {
        matches!(self, Self::Ollama { .. })
    }

    fn quiet(&self) -> Duration {
        if self.local() { QUIET_LOCAL } else { QUIET }
    }
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
            body["options"] = json!({ "num_ctx": LOCAL_CONTEXT });
            body["keep_alive"] = json!(KEEP_LOADED);
            if *think {
                body["think"] = json!(false);
            }
            if with_tools {
                body["tools"] = openai_tools();
            }
            (client.post(format!("{OLLAMA}/api/chat")).json(&body), "")
        }
    };
    let quiet = provider.quiet();
    let response = waited(tx, quiet, request.send())
        .await?
        .map_err(|error| error.to_string())?;
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
    let mut openai = OpenAiParser::default();
    let mut ollama = OllamaParser::default();
    let mut anthropic = AnthropicParser::default();
    while let Some(chunk) = waited(tx, quiet, stream.next()).await? {
        let chunk = chunk.map_err(|error| error.to_string())?;
        let pieces: Vec<Piece> = match provider {
            Provider::OpenAi { .. } => openai.push(&chunk),
            Provider::Ollama { .. } => ollama.push(&chunk),
            Provider::Anthropic { .. } => anthropic.push(&chunk),
        };
        for piece in pieces {
            match piece {
                Piece::Text(delta) => {
                    text.push_str(&delta);
                    if tx.send(ChatEvent::Delta(delta)).await.is_err() {
                        return Err(CLIENT_LEFT.to_owned());
                    }
                }
                Piece::Tool {
                    index,
                    id,
                    name,
                    arguments,
                } => parts.push((index, id, name, arguments)),
                Piece::Done => {}
            }
        }
    }
    // Tools asked for on the round that wasn't offered any are ignored: the words stand.
    let calls = if with_tools {
        assemble(&parts)
    } else {
        Vec::new()
    };
    if calls.is_empty() {
        Ok(Outcome::Text(text))
    } else {
        Ok(Outcome::Tools { said: text, calls })
    }
}

/// Waits for `work`, but not past the page leaving or the far end going quiet.
async fn waited<T>(
    tx: &mpsc::Sender<ChatEvent>,
    quiet: Duration,
    work: impl Future<Output = T>,
) -> Result<T, String> {
    tokio::select! {
        _ = tx.closed() => Err(CLIENT_LEFT.to_owned()),
        done = tokio::time::timeout(quiet, work) => done.map_err(|_| WENT_QUIET.to_owned()),
    }
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
         You can read devices and recent values. You cannot change them. \
         Keep answers short. Put every value or state you report in backticks, like `on` or \
         `21.5 °C`, and use **bold** for a device's name. Short lists are fine.\n",
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

/// The names of the models Ollama has in memory right now.
async fn loaded() -> Vec<String> {
    let Ok(response) = client()
        .get(format!("{OLLAMA}/api/ps"))
        .timeout(Duration::from_secs(2))
        .send()
        .await
    else {
        return Vec::new();
    };
    let Ok(value) = response.json::<Value>().await else {
        return Vec::new();
    };
    value["models"]
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter_map(|model| model["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The numbers that decide a model's memory, from its own description. Asked of Ollama once
/// per download and remembered: a model's shape does not change.
async fn shape(model: &RemoteModel) -> Option<Shape> {
    static KNOWN: std::sync::Mutex<Vec<(String, u64, Shape)>> = std::sync::Mutex::new(Vec::new());
    let known = |name: &str, size: u64| {
        KNOWN.lock().ok().and_then(|known| {
            known
                .iter()
                .find(|(n, s, _)| n == name && *s == size)
                .map(|(_, _, shape)| *shape)
        })
    };
    if let Some(shape) = known(&model.name, model.size) {
        return Some(shape);
    }
    let value: Value = client()
        .post(format!("{OLLAMA}/api/show"))
        .timeout(Duration::from_secs(5))
        .json(&json!({ "model": model.name }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let info = &value["model_info"];
    let family = info["general.architecture"].as_str()?;
    let number = |key: &str| info[format!("{family}.{key}")].as_u64();
    let layers = number("block_count")?;
    let heads = number("attention.head_count")?;
    let kv_heads = number("attention.head_count_kv").unwrap_or(heads);
    // A model that doesn't say how long a head's key is splits its width evenly across them.
    let width = number("embedding_length").and_then(|width| width.checked_div(heads));
    let shape = Shape {
        layers,
        kv_heads,
        key_length: number("attention.key_length").or(width)?,
        value_length: number("attention.value_length").or(width)?,
    };
    if let Ok(mut known) = KNOWN.lock() {
        known.push((model.name.clone(), model.size, shape));
    }
    Some(shape)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
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
    // The rest of the app writes this file too. Wait for it instead of failing at once.
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
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

/// Adds one exchange to `scope` and drops what no longer fits.
///
/// The scope is read again here, under the write lock, so an exchange that finished while this
/// one was waiting on the model is kept. Rows that stay keep their `created_at`, which is what
/// [`trim_global`] goes by.
fn store_turns(db: &Path, scope: &str, said: [Remembered; 2]) -> Result<(), String> {
    let mut conn = open_db(db)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    let mut seqs = Vec::new();
    let mut turns = Vec::new();
    {
        let mut statement = tx
            .prepare("SELECT seq, role, body FROM assistant_message WHERE scope = ?1 ORDER BY seq")
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([scope], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| error.to_string())?;
        for row in rows {
            let (seq, role, body) = row.map_err(|error| error.to_string())?;
            let Some(role) = Role::parse(&role) else {
                continue;
            };
            seqs.push(seq);
            turns.push(Remembered { role, body });
        }
    }
    let had = turns.len();
    let next = seqs.last().map_or(0, |seq| seq + 1);
    let added = said.len();
    for turn in said {
        irori_assist::append_capped(&mut turns, turn);
    }
    // `append_capped` only drops from the front, so what is left is the tail of old and new.
    let dropped = had + added - turns.len();
    match seqs.get(dropped) {
        Some(first_kept) => tx.execute(
            "DELETE FROM assistant_message WHERE scope = ?1 AND seq < ?2",
            (scope, first_kept),
        ),
        None => tx.execute("DELETE FROM assistant_message WHERE scope = ?1", [scope]),
    }
    .map_err(|error| error.to_string())?;
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let kept_old = had.saturating_sub(dropped);
    for (n, turn) in turns[kept_old..].iter().enumerate() {
        tx.execute(
            "INSERT INTO assistant_message (scope, seq, role, body, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                scope,
                next + n as i64,
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

/// A provider's failure, in words a person can act on where Irori knows what it means.
fn explained(error: &str) -> String {
    if error == WENT_QUIET {
        return "The model took too long to start answering and was given up on. A model on a \
                small machine can need minutes just to read the question. Ask again, try a \
                smaller model, or use a cloud model."
            .to_owned();
    }
    if error.contains("signal: killed")
        || error.contains("out of memory")
        || error.contains("process has terminated")
    {
        return "The model was stopped while it was loading, which nearly always means the \
                machine ran out of memory for it. Give the machine (or its container) more \
                memory, or download a smaller model. Ollama's own log is under Settings, \
                Assistant, Model log."
            .to_owned();
    }
    error.to_owned()
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

/// What Irori's own Ollama has said lately. Empty when the Ollama here isn't Irori's.
pub fn model_log(data_dir: &Path) -> Vec<String> {
    ollama::log(data_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_model_is_sent_only_the_end_of_a_long_conversation() {
        let said = |role, body: &str| Remembered {
            role,
            body: body.to_owned(),
        };
        let turns = [
            said(Role::User, &"a".repeat(40)),
            said(Role::Assistant, &"b".repeat(40)),
            said(Role::User, "second"),
            said(Role::Assistant, "answer"),
        ];
        assert_eq!(recent(&turns, 1_000).len(), 4);
        // The first answer would fit, but not its question, so neither is sent.
        let kept = recent(&turns, 60);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].body, "second");
        assert!(recent(&turns, 3).is_empty());
    }

    #[test]
    fn a_model_that_went_quiet_is_explained() {
        assert!(explained(WENT_QUIET).contains("too long"));
    }

    #[test]
    fn a_killed_model_is_explained_as_memory() {
        let said = explained(
            "the model refused (500 Internal Server Error): {\"error\":\"llama-server process has terminated: signal: killed\"}",
        );
        assert!(said.contains("memory"), "{said}");
        assert_eq!(explained("no such model"), "no such model");
    }

    fn exchange(ask: &str, reply: &str) -> [Remembered; 2] {
        [
            Remembered {
                role: Role::User,
                body: ask.to_owned(),
            },
            Remembered {
                role: Role::Assistant,
                body: reply.to_owned(),
            },
        ]
    }

    fn stamps(db: &Path, scope: &str) -> anyhow::Result<Vec<String>> {
        let conn = open_db(db).map_err(anyhow::Error::msg)?;
        let mut statement =
            conn.prepare("SELECT created_at FROM assistant_message WHERE scope = ?1 ORDER BY seq")?;
        let rows = statement.query_map([scope], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn store(db: &Path, ask: &str, reply: &str) -> anyhow::Result<()> {
        store_turns(db, "general", exchange(ask, reply)).map_err(anyhow::Error::msg)
    }

    fn kept(db: &Path) -> anyhow::Result<Vec<Remembered>> {
        load_turns(db, "general").map_err(anyhow::Error::msg)
    }

    #[test]
    fn an_exchange_is_added_to_what_is_there_and_not_written_over_it() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("irori.db");
        // Both of these read the scope while it was empty, as two overlapping asks would.
        store(&db, "one", "first")?;
        store(&db, "two", "second")?;
        let bodies: Vec<String> = kept(&db)?.into_iter().map(|turn| turn.body).collect();
        assert_eq!(bodies, ["one", "first", "two", "second"]);
        Ok(())
    }

    #[test]
    fn older_turns_keep_the_time_they_were_said() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("irori.db");
        store(&db, "one", "first")?;
        let before = stamps(&db, "general")?;
        std::thread::sleep(Duration::from_millis(5));
        store(&db, "two", "second")?;
        let after = stamps(&db, "general")?;
        assert_eq!(after[..2], before[..]);
        assert!(after[2] > before[1]);
        Ok(())
    }

    #[test]
    fn a_full_view_drops_its_oldest_turns() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("irori.db");
        for n in 0..irori_assist::LIMIT_MESSAGES {
            store(&db, &format!("ask {n}"), "reply")?;
        }
        let turns = kept(&db)?;
        assert_eq!(turns.len(), irori_assist::LIMIT_MESSAGES);
        assert_eq!(
            turns[0].body,
            format!("ask {}", irori_assist::LIMIT_MESSAGES / 2)
        );
        store(&db, "big", &"x".repeat(irori_assist::LIMIT_BYTES + 10))?;
        let turns = kept(&db)?;
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].body.len(), irori_assist::LIMIT_BYTES);
        Ok(())
    }
}
