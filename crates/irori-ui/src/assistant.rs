//! The assistant: a Settings card, a quiet Ask button, and one chat per view.
//!
//! Ask is a real button before a model is ready. It opens Settings at this card. Once a model
//! is ready it opens the chat, and the sidebar grows an entry for the general one.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_location, use_navigate};

use crate::Assistant;
use crate::api::{self, AssistantStatus};

/// Where Ask goes: the chat at `path` when a model is ready and there is a chat to open, and
/// the Settings card otherwise.
pub fn destination(assistant: Assistant, path: Option<String>) -> String {
    let ready = assistant
        .0
        .get_untracked()
        .is_some_and(|status| status.ready);
    path.filter(|_| ready)
        .unwrap_or_else(|| "/settings#assistant".to_owned())
}

/// Opens the chat for `path` when a model is ready, and Settings otherwise.
#[component]
pub fn Ask(path: String) -> impl IntoView {
    let assistant = expect_context::<Assistant>();
    let navigate = use_navigate();
    let quiet = move || !assistant.0.get().is_some_and(|status| status.ready);
    view! {
        <button
            type="button"
            class:ask-quiet=quiet
            title="Ask"
            on:click=move |_| {
                navigate(&destination(assistant, Some(path.clone())), Default::default());
            }
        >
            "Ask"
        </button>
    }
}

/// `/assistant`, `/assistant/device/:id`, and `/assistant/automation/:id`.
#[component]
pub fn Page() -> impl IntoView {
    let location = use_location();
    let assistant = expect_context::<Assistant>();
    let scope = Memo::new(move |_| scope_of(&location.pathname.get()));
    let heading = Memo::new(move |_| heading_of(&location.pathname.get()));
    let messages = RwSignal::new(Vec::<api::AssistantMessage>::new());
    let draft = RwSignal::new(String::new());
    let asking = RwSignal::new(false);
    let trouble = RwSignal::new(None::<String>);

    Effect::new(move |_| {
        let scope = scope.get();
        trouble.set(None);
        spawn_local(async move {
            match api::assistant_transcript(&scope).await {
                Ok(turns) => messages.set(turns),
                Err(error) => trouble.set(Some(error)),
            }
        });
    });

    let send = move |event: ev::SubmitEvent| {
        event.prevent_default();
        if asking.get_untracked() {
            return;
        }
        let text = draft.get_untracked().trim().to_owned();
        if text.is_empty() {
            return;
        }
        let scope = scope.get_untracked();
        draft.set(String::new());
        asking.set(true);
        trouble.set(None);
        messages.update(|turns| {
            turns.push(api::AssistantMessage {
                role: "user".to_owned(),
                body: text.clone(),
            })
        });
        spawn_local(async move {
            match api::assistant_ask(&scope, &text).await {
                Ok(answer) => messages.update(|turns| {
                    turns.push(api::AssistantMessage {
                        role: "assistant".to_owned(),
                        body: answer,
                    })
                }),
                Err(error) => {
                    trouble.set(Some(error));
                    if let Ok(turns) = api::assistant_transcript(&scope).await {
                        messages.set(turns);
                    }
                }
            }
            asking.set(false);
        });
    };

    let clear = move |_| {
        let scope = scope.get_untracked();
        spawn_local(async move {
            if let Err(error) = api::assistant_clear(&scope).await {
                trouble.set(Some(error));
                return;
            }
            messages.set(Vec::new());
        });
    };

    view! {
        <div class="page-head">
            <h1>{move || heading.get()}</h1>
        </div>
        {move || {
            assistant.0.get().filter(|status| !status.ready).map(|status| view! {
                <p class="why">
                    {status.detail}
                    " "
                    <a href="/settings#assistant">"Set it up in Settings."</a>
                </p>
            })
        }}
        {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}
        <div class="chat">
            {move || {
                messages
                    .get()
                    .into_iter()
                    .map(|message| {
                        let mine = message.role == "user";
                        view! { <p class="chat-turn" class:mine=mine>{message.body}</p> }
                    })
                    .collect_view()
            }}
            <form class="about-form" on:submit=send>
                <label>
                    <span>"MESSAGE"</span>
                    <textarea
                        rows="3"
                        prop:value=move || draft.get()
                        on:input=move |event| draft.set(event_target_value(&event))
                    ></textarea>
                </label>
                <div class="page-actions">
                    <button type="submit">
                        {move || if asking.get() { "Asking…" } else { "Ask" }}
                    </button>
                    <button type="button" class="quiet-button" on:click=clear>"Clear"</button>
                </div>
            </form>
        </div>
    }
}

fn scope_of(path: &str) -> String {
    let mut parts = path
        .trim_start_matches("/assistant")
        .split('/')
        .filter(|part| !part.is_empty());
    match (parts.next(), parts.next()) {
        (Some("device"), Some(id)) => format!("device:{id}"),
        (Some("automation"), Some(id)) => format!("automation:{id}"),
        _ => "general".to_owned(),
    }
}

fn heading_of(path: &str) -> &'static str {
    if path.starts_with("/assistant/device/") {
        "This device"
    } else if path.starts_with("/assistant/automation/") {
        "This automation"
    } else {
        "Assistant"
    }
}

/// The Settings card. Always there; a model is optional.
#[component]
pub fn Section() -> impl IntoView {
    let assistant = expect_context::<Assistant>();
    let mode = RwSignal::new("off".to_owned());
    let local_tag = RwSignal::new("qwen3:1.7b".to_owned());
    let preset = RwSignal::new("openai".to_owned());
    let base_url = RwSignal::new(String::new());
    let model = RwSignal::new(String::new());
    let api_key = RwSignal::new(String::new());
    let clear_key = RwSignal::new(false);
    let loaded = RwSignal::new(false);
    let saving = RwSignal::new(false);
    let pulling = RwSignal::new(false);
    let progress = RwSignal::new(String::new());
    let trouble = RwSignal::new(None::<String>);

    Effect::new(move |_| {
        if loaded.get_untracked() {
            return;
        }
        if let Some(status) = assistant.0.get() {
            apply(&status, mode, local_tag, preset, base_url, model);
            loaded.set(true);
        }
    });

    spawn_local(async move {
        match api::fetch_assistant().await {
            Ok(status) => assistant.0.set(Some(status)),
            Err(error) => trouble.set(Some(error)),
        }
    });

    let save = Callback::new(move |_: ()| {
        if saving.get_untracked() {
            return;
        }
        saving.set(true);
        trouble.set(None);
        let mut body = serde_json::json!({
            "mode": mode.get_untracked(),
            "local_tag": local_tag.get_untracked(),
            "preset": preset.get_untracked(),
            "base_url": base_url.get_untracked(),
            "model": model.get_untracked(),
        });
        if clear_key.get_untracked() {
            body["api_key"] = serde_json::Value::String(String::new());
        } else if !api_key.get_untracked().is_empty() {
            body["api_key"] = serde_json::Value::String(api_key.get_untracked());
        }
        spawn_local(async move {
            match api::save_assistant(&body).await {
                Ok(status) => {
                    assistant.0.set(Some(status.clone()));
                    apply(&status, mode, local_tag, preset, base_url, model);
                    api_key.set(String::new());
                    clear_key.set(false);
                }
                Err(error) => trouble.set(Some(error)),
            }
            saving.set(false);
        });
    });

    let download = move |_| {
        if pulling.get_untracked() {
            return;
        }
        pulling.set(true);
        progress.set(String::new());
        trouble.set(None);
        let tag = local_tag.get_untracked();
        spawn_local(async move {
            match api::assistant_pull(&tag).await {
                Ok(line) => {
                    progress.set(line);
                    if let Ok(status) = api::fetch_assistant().await {
                        assistant.0.set(Some(status.clone()));
                        apply(&status, mode, local_tag, preset, base_url, model);
                    }
                }
                Err(error) => trouble.set(Some(error)),
            }
            pulling.set(false);
        });
    };

    view! {
        <section class="card settings-section" id="assistant" style="--i: 2">
            <h2>"Assistant"</h2>
            <p class="muted small">
                "A model on this machine, through Ollama, or a cloud API. Nothing is downloaded \
                 with Irori. Until one is ready, Ask opens this card."
            </p>
            {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}
            {move || assistant.0.get().map(|status| view! {
                <p class="why">{status.detail}</p>
            })}
            <form class="about-form assistant-form" on:submit=move |event: ev::SubmitEvent| {
                event.prevent_default();
                save.run(());
            }>
                <label>
                    <span>"HOW IT ANSWERS"</span>
                    <select
                        prop:value=move || mode.get()
                        on:change=move |event| mode.set(event_target_value(&event))
                    >
                        <option value="off">"Off"</option>
                        <option value="local">"On this machine"</option>
                        <option value="cloud">"A cloud API"</option>
                    </select>
                </label>
                <label>
                    <span>"OLLAMA MODEL"</span>
                    <input
                        type="text"
                        prop:value=move || local_tag.get()
                        on:input=move |event| local_tag.set(event_target_value(&event))
                    />
                </label>
                <p class="muted small">
                    "The default is "
                    <a href="https://ollama.com/library/qwen3:1.7b" target="_blank" rel="noreferrer">
                        "qwen3:1.7b"
                    </a>
                    " (about 1.4 GB). Any other tag from the "
                    <a href="https://ollama.com/library" target="_blank" rel="noreferrer">
                        "Ollama library"
                    </a>
                    " works the same way. Ollama itself comes from "
                    <a href="https://ollama.com/download" target="_blank" rel="noreferrer">
                        "ollama.com/download"
                    </a>
                    " and listens on 127.0.0.1:11434."
                </p>
                {move || assistant.0.get().map(|status| view! {
                    <ul class="assistant-models">
                        {status.pulled.into_iter().map(|pulled| {
                            let tag = pulled.name.clone();
                            let label = pulled.name.clone();
                            let active = pulled.active;
                            view! {
                                <li>
                                    <button type="button" on:click=move |_| {
                                        mode.set("local".to_owned());
                                        local_tag.set(tag.clone());
                                        save.run(());
                                    }>
                                        {label}
                                        {active.then_some(" · in use")}
                                    </button>
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                })}
                <div class="page-actions">
                    <button type="button" on:click=download>
                        {move || if pulling.get() { "Downloading…" } else { "Download" }}
                    </button>
                    {move || (!progress.get().is_empty()).then(|| view! {
                        <span class="assistant-progress">{progress.get()}</span>
                    })}
                </div>
                <label>
                    <span>"CLOUD API"</span>
                    <select
                        prop:value=move || preset.get()
                        on:change=move |event| {
                            let next = event_target_value(&event);
                            preset.set(next.clone());
                            if let Some(url) = preset_url(&next) {
                                base_url.set(url.to_owned());
                            }
                        }
                    >
                        <option value="openai">"OpenAI"</option>
                        <option value="anthropic">"Anthropic"</option>
                        <option value="grok">"Grok"</option>
                        <option value="compatible">"OpenAI-compatible"</option>
                    </select>
                </label>
                <label>
                    <span>"ENDPOINT"</span>
                    <input
                        type="url"
                        prop:value=move || base_url.get()
                        on:input=move |event| base_url.set(event_target_value(&event))
                    />
                </label>
                <label>
                    <span>"MODEL"</span>
                    <input
                        type="text"
                        prop:value=move || model.get()
                        on:input=move |event| model.set(event_target_value(&event))
                    />
                </label>
                <label>
                    <span>"API KEY"</span>
                    <input
                        type="password"
                        autocomplete="off"
                        placeholder=move || {
                            if assistant.0.get().is_some_and(|status| status.credential == "set") {
                                "Saved — leave blank to keep it"
                            } else {
                                ""
                            }
                        }
                        prop:value=move || api_key.get()
                        on:input=move |event| api_key.set(event_target_value(&event))
                    />
                </label>
                {move || assistant.0.get().is_some_and(|status| status.credential == "set").then(|| view! {
                    <label class="assistant-check">
                        <input
                            type="checkbox"
                            prop:checked=move || clear_key.get()
                            on:change=move |event| clear_key.set(event_target_checked(&event))
                        />
                        <span>"Remove the saved key"</span>
                    </label>
                })}
                {move || (mode.get() == "cloud").then(|| view! {
                    <p class="why">
                        "A cloud answer leaves the house, including device names and what you ask."
                    </p>
                })}
                <div class="page-actions">
                    <button type="submit">
                        {move || if saving.get() { "Saving…" } else { "Save" }}
                    </button>
                </div>
            </form>
        </section>
    }
}

fn apply(
    status: &AssistantStatus,
    mode: RwSignal<String>,
    local_tag: RwSignal<String>,
    preset: RwSignal<String>,
    base_url: RwSignal<String>,
    model: RwSignal<String>,
) {
    mode.set(status.mode.clone());
    local_tag.set(status.local_tag.clone());
    preset.set(status.preset.clone());
    base_url.set(status.base_url.clone());
    model.set(status.cloud_model.clone());
}

fn preset_url(preset: &str) -> Option<&'static str> {
    match preset {
        "openai" => Some("https://api.openai.com/v1"),
        "anthropic" => Some("https://api.anthropic.com"),
        "grok" => Some("https://api.x.ai/v1"),
        _ => None,
    }
}

/// Scrolls to the assistant card when the address says `#assistant`.
pub fn watch_hash() {
    let location = use_location();
    Effect::new(move |_| {
        let hash = location.hash.get();
        if hash == "#assistant" || hash == "assistant" {
            let Some(section) = document().get_element_by_id("assistant") else {
                return;
            };
            section.scroll_into_view();
            let again = section.get_attribute("data-flash").as_deref() == Some("a");
            let _ = section.set_attribute("data-flash", if again { "b" } else { "a" });
        }
    });
}
