//! The assistant: a Settings card, a quiet Ask button, and one chat per view.
//!
//! Ask is a real button before a model is ready. It opens Settings at this card. Once a model
//! is ready it opens a chat window under itself, about the thing the page is showing, and the
//! sidebar grows an entry for the chat about the whole home.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::NavigateOptions;
use leptos_router::hooks::{use_location, use_navigate};
use web_sys::wasm_bindgen::JsCast as _;

use crate::Assistant;
use crate::api::{self, AssistantStatus, Progress, Streamed};

/// The chat window an Ask button opened: what it is about, and where the button was.
#[derive(Debug, Clone, PartialEq)]
pub struct AskAt {
    /// `device:<id>` or `automation:<id>`.
    pub scope: String,
    pub title: String,
    /// From the top of the window to the top of the chat.
    top: f64,
    /// From the right of the window to the right of the chat.
    right: f64,
}

/// Whether the local model's log window is open. Kept by the shell, so a chat and the
/// Settings card open the same one and it is drawn over both.
#[derive(Debug, Clone, Copy)]
pub struct ModelLog(pub RwSignal<bool>);

/// Which chat window is open, if one is. One at a time, shared by every Ask on the page.
#[derive(Debug, Clone, Copy)]
pub struct Asking(pub RwSignal<Option<AskAt>>);

/// What an Ask button does: Settings while no model is ready, and otherwise a chat window
/// under the button, whose bottom edge is `bottom` and right edge `right`. Asking the same
/// thing again closes it.
pub fn ask(
    assistant: Assistant,
    asking: Asking,
    navigate: &impl Fn(&str, NavigateOptions),
    scope: Option<String>,
    title: String,
    bottom: f64,
    right: f64,
) {
    let ready = assistant
        .0
        .get_untracked()
        .is_some_and(|status| status.ready);
    let Some(scope) = scope.filter(|_| ready) else {
        navigate("/settings#assistant", Default::default());
        return;
    };
    if asking
        .0
        .with_untracked(|at| at.as_ref().is_some_and(|at| at.scope == scope))
    {
        asking.0.set(None);
        return;
    }
    let side = |read: Result<web_sys::wasm_bindgen::JsValue, _>, fallback: f64| {
        read.ok()
            .and_then(|value| value.as_f64())
            .unwrap_or(fallback)
    };
    let width = side(window().inner_width(), 1024.0);
    let height = side(window().inner_height(), 768.0);
    asking.0.set(Some(AskAt {
        scope,
        title,
        // Under the button, but never so low that the chat has no room to be a chat.
        top: (bottom + 8.0).min(height - 360.0).max(8.0),
        right: (width - right).max(12.0),
    }));
}

/// Opens the chat about `scope` under itself when a model is ready, and Settings otherwise.
#[component]
pub fn Ask(scope: String, title: String) -> impl IntoView {
    let assistant = expect_context::<Assistant>();
    let asking = expect_context::<Asking>();
    let navigate = use_navigate();
    let quiet = move || !assistant.0.get().is_some_and(|status| status.ready);
    let open = {
        let scope = scope.clone();
        move || {
            asking
                .0
                .with(|at| at.as_ref().is_some_and(|at| at.scope == scope))
        }
    };
    view! {
        <button
            type="button"
            class="ask"
            class:ask-quiet=quiet
            class:on=open.clone()
            aria-expanded=move || open().to_string()
            title="Ask"
            on:click=move |event: ev::MouseEvent| {
                let Some(button) = event
                    .current_target()
                    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                else {
                    return;
                };
                let rect = button.get_bounding_client_rect();
                ask(
                    assistant,
                    asking,
                    &navigate,
                    Some(scope.clone()),
                    title.clone(),
                    rect.bottom(),
                    rect.right(),
                );
            }
        >
            <Spark />
            "Ask"
        </button>
    }
}

#[component]
fn Spark() -> impl IntoView {
    view! {
        <svg class="spark" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M12 3l1.9 5.6L19.5 10l-5.6 1.9L12 17.5l-1.9-5.6L4.5 10l5.6-1.4z"
                fill="currentColor" />
        </svg>
    }
}

/// The chat window an Ask opened. It sits in the shell, over the page, so the same window
/// serves a button on a page and one inside an extension's frame.
#[component]
pub fn Popover() -> impl IntoView {
    let asking = expect_context::<Asking>();
    let location = use_location();
    // Going to another page leaves the thing the chat was about.
    Effect::new(move |before: Option<String>| {
        let path = location.pathname.get();
        if before.is_some_and(|before| before != path) {
            asking.0.set(None);
        }
        path
    });
    move || {
        asking.0.get().map(|at| {
            let close = Callback::new(move |_: ()| asking.0.set(None));
            let label = format!("Ask about {}", at.title);
            view! {
                <div
                    class="ask-pop"
                    role="dialog"
                    aria-label=label
                    style=format!("--top: {}px; --right: {}px", at.top, at.right)
                    on:keydown=move |event: ev::KeyboardEvent| {
                        if event.key() == "Escape" {
                            asking.0.set(None);
                        }
                    }
                >
                    <Chat scope=at.scope title=at.title on_close=close />
                </div>
            }
        })
    }
}

/// `/assistant`: the chat about the whole home.
#[component]
pub fn Page() -> impl IntoView {
    let assistant = expect_context::<Assistant>();
    view! {
        <div class="page-head">
            <h1>"Assistant"</h1>
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
        <div class="chat-page">
            <Chat scope="general".to_owned() title="Your home".to_owned() />
        </div>
    }
}

/// What the model is doing between the question and the end of the answer.
#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Thinking,
    /// Running a tool. Its name.
    Looking(String),
    Writing,
}

impl Phase {
    fn words(&self) -> &'static str {
        match self {
            Self::Thinking => "Thinking",
            Self::Looking(tool) => match tool.as_str() {
                "queued" => "Waiting for the model to finish another answer",
                "list_devices" => "Looking around the home",
                "get_device" => "Checking a device",
                "recent_states" => "Reading recent values",
                _ => "Looking something up",
            },
            Self::Writing => "Writing",
        }
    }
}

/// One conversation: what was said, the answer as it is written, and the box to ask in.
#[component]
pub fn Chat(
    scope: String,
    title: String,
    #[prop(optional)] on_close: Option<Callback<()>>,
) -> impl IntoView {
    let messages = RwSignal::new(Vec::<api::AssistantMessage>::new());
    let draft = RwSignal::new(String::new());
    let asking = RwSignal::new(false);
    let phase = RwSignal::new(Phase::Thinking);
    // The answer so far, while it is being written.
    let writing = RwSignal::new(String::new());
    let trouble = RwSignal::new(None::<String>);
    // The question a failed answer was for, so it can be asked again without typing it.
    let unanswered = RwSignal::new(None::<String>);
    // Seconds since the question was sent.
    let waited = RwSignal::new(0u32);
    let log = NodeRef::<leptos::html::Div>::new();
    let scope = StoredValue::new(scope);
    let assistant = expect_context::<Assistant>();
    let model_log = expect_context::<ModelLog>();
    // Ollama's own log, when the model is on this machine and Irori runs its Ollama.
    let has_log = move || {
        assistant
            .0
            .get()
            .is_some_and(|status| status.mode == "local" && status.managed)
    };

    spawn_local(async move {
        match api::assistant_transcript(&scope.get_value()).await {
            Ok(turns) => {
                messages.try_set(turns);
            }
            Err(error) => {
                trouble.try_set(Some(error));
            }
        }
    });

    // The newest line stays in view, while reading back and while an answer grows.
    Effect::new(move |_| {
        messages.track();
        writing.track();
        asking.track();
        request_animation_frame(move || {
            if let Some(log) = log.get_untracked() {
                log.set_scroll_top(log.scroll_height());
            }
        });
    });

    // The chat can be closed while an answer is on its way, so everything the answer touches
    // is asked for with `try_`: by then it may be gone.
    let send = Callback::new(move |_: ()| {
        if asking.get_untracked() {
            return;
        }
        let text = draft.get_untracked().trim().to_owned();
        if text.is_empty() {
            return;
        }
        draft.set(String::new());
        asking.set(true);
        trouble.set(None);
        unanswered.set(None);
        writing.set(String::new());
        phase.set(Phase::Thinking);
        waited.set(0);
        // A model on a small machine can take minutes. The count says it is still going.
        spawn_local(async move {
            while asking.try_get_untracked() == Some(true) {
                gloo_timers::future::sleep(std::time::Duration::from_secs(1)).await;
                waited.try_update(|seconds| *seconds += 1);
            }
        });
        messages.update(|turns| {
            turns.push(api::AssistantMessage {
                role: "user".to_owned(),
                body: text.clone(),
            })
        });
        spawn_local(async move {
            let scope = scope.try_get_value().unwrap_or_default();
            let result = api::assistant_ask(&scope, &text, |event| match event {
                Streamed::Delta(delta) => {
                    writing.try_update(|answer| answer.push_str(&delta));
                    phase.try_set(Phase::Writing);
                }
                Streamed::Step(tool) => {
                    phase.try_set(Phase::Looking(tool));
                }
                Streamed::Failed(_) | Streamed::Done => {}
            })
            .await;
            match result {
                Ok(()) => {
                    let body = writing.try_get_untracked().unwrap_or_default();
                    messages.try_update(|turns| {
                        turns.push(api::AssistantMessage {
                            role: "assistant".to_owned(),
                            body: body.trim_end().to_owned(),
                        })
                    });
                }
                Err(error) => {
                    trouble.try_set(Some(error));
                    unanswered.try_set(Some(text.clone()));
                    // A reply that failed isn't kept, and neither is its question.
                    if let Ok(turns) = api::assistant_transcript(&scope).await {
                        messages.try_set(turns);
                    }
                }
            }
            writing.try_set(String::new());
            asking.try_set(false);
        });
    });

    let clear = move |_| {
        spawn_local(async move {
            let scope = scope.try_get_value().unwrap_or_default();
            match api::assistant_clear(&scope).await {
                Ok(()) => {
                    messages.try_set(Vec::new());
                }
                Err(error) => {
                    trouble.try_set(Some(error));
                }
            }
        });
    };

    let empty = move || messages.with(Vec::is_empty) && !asking.get();
    let about = title.clone();

    view! {
        <section class="chat">
            <header class="chat-head">
                <Spark />
                <span class="chat-title">{title}</span>
                {move || has_log().then(|| view! {
                    <button
                        type="button"
                        class="chat-clear"
                        title="What the model on this machine has said"
                        on:click=move |_| model_log.0.set(true)
                    >
                        "Log"
                    </button>
                })}
                <button
                    type="button"
                    class="chat-clear"
                    disabled=move || empty() || asking.get()
                    on:click=clear
                >
                    "Clear"
                </button>
                {on_close.map(|close| view! {
                    <button
                        type="button"
                        class="chat-close"
                        aria-label="Close"
                        on:click=move |_| close.run(())
                    >
                        "×"
                    </button>
                })}
            </header>
            <div class="chat-log" node_ref=log>
                {move || empty().then(|| view! {
                    <p class="chat-empty">
                        "Ask about " {about.clone()} ". Answers come from what Irori can see right \
                         now; nothing is changed."
                    </p>
                })}
                {move || {
                    messages
                        .get()
                        .into_iter()
                        .map(|message| {
                            if message.role == "user" {
                                view! { <div class="turn mine"><p>{message.body}</p></div> }
                                    .into_any()
                            } else {
                                view! {
                                    <div class="turn theirs rich">
                                        {crate::rich::render(&message.body)}
                                    </div>
                                }
                                .into_any()
                            }
                        })
                        .collect_view()
                }}
                {move || asking.get().then(|| view! {
                    <div class="turn theirs rich writing">
                        {move || crate::rich::render(&writing.get())}
                        <p class="chat-phase" aria-live="polite">
                            <span class="dots"><i></i><i></i><i></i></span>
                            {move || phase.get().words()}
                            {move || {
                                let seconds = waited.get();
                                (seconds >= 5).then(|| view! {
                                    <span class="chat-waited">{clock(seconds)}</span>
                                })
                            }}
                        </p>
                        {move || {
                            (waited.get() >= 20 && phase.get() == Phase::Thinking).then(|| view! {
                                <p class="chat-slow">
                                    "A model on this machine reads the whole question before it \
                                     says a word, which can take a few minutes on a small one."
                                </p>
                            })
                        }}
                    </div>
                })}
                {move || trouble.get().map(|why| view! {
                    <div class="chat-trouble">
                        <p>{why}</p>
                        <div class="chat-trouble-actions">
                            {move || unanswered.get().map(|question| view! {
                                <button type="button" class="quiet-button" on:click=move |_| {
                                    draft.set(question.clone());
                                    send.run(());
                                }>
                                    "Ask again"
                                </button>
                            })}
                            {move || has_log().then(|| view! {
                                <button type="button" class="quiet-button"
                                    on:click=move |_| model_log.0.set(true)>
                                    "See the model log"
                                </button>
                            })}
                        </div>
                    </div>
                })}
            </div>
            <form
                class="composer"
                on:submit=move |event: ev::SubmitEvent| {
                    event.prevent_default();
                    send.run(());
                }
            >
                <textarea
                    rows="1"
                    aria-label="Message"
                    placeholder="Ask a question"
                    prop:value=move || draft.get()
                    on:input=move |event| draft.set(event_target_value(&event))
                    on:keydown=move |event: ev::KeyboardEvent| {
                        // Enter sends; Shift+Enter is a new line. Not while a word is still
                        // being composed, where Enter belongs to the keyboard.
                        if event.key() == "Enter" && !event.shift_key() && !event.is_composing() {
                            event.prevent_default();
                            send.run(());
                        }
                    }
                ></textarea>
                <button
                    type="submit"
                    class="send"
                    aria-label="Send"
                    class:busy=move || asking.get()
                    disabled=move || asking.get() || draft.with(|text| text.trim().is_empty())
                >
                    <svg viewBox="0 0 24 24" aria-hidden="true">
                        <path d="M12 19V5M5.5 11.5L12 5l6.5 6.5" fill="none" stroke="currentColor"
                            stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" />
                    </svg>
                </button>
            </form>
        </section>
    }
}

/// The two ways a model can answer, as the Settings card shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Local,
    Cloud,
}

/// The Settings card. Always there; a model is optional. Folded until it is wanted.
#[component]
pub fn Section() -> impl IntoView {
    let assistant = expect_context::<Assistant>();
    let side = RwSignal::new(Side::Local);
    let local_tag = RwSignal::new("qwen3:1.7b".to_owned());
    let preset = RwSignal::new("openai".to_owned());
    let base_url = RwSignal::new(String::new());
    let model = RwSignal::new(String::new());
    let api_key = RwSignal::new(String::new());
    let loaded = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let progress = RwSignal::new(None::<Progress>);
    let removing = RwSignal::new(false);
    let log_open = expect_context::<ModelLog>().0;
    let trouble = RwSignal::new(None::<String>);

    let apply = move |status: &AssistantStatus| {
        side.set(if status.mode == "cloud" {
            Side::Cloud
        } else {
            Side::Local
        });
        local_tag.set(status.local_tag.clone());
        preset.set(status.preset.clone());
        base_url.set(status.base_url.clone());
        model.set(status.cloud_model.clone());
    };

    // The fields follow the first status to arrive, and after that only what this card saved:
    // the slow poll must not write over something half typed.
    Effect::new(move |_| {
        if loaded.get_untracked() {
            return;
        }
        if let Some(status) = assistant.0.get() {
            apply(&status);
            loaded.set(true);
        }
    });

    spawn_local(async move {
        match api::fetch_assistant().await {
            Ok(status) => assistant.0.set(Some(status)),
            Err(error) => trouble.set(Some(error)),
        }
    });

    // Every change goes through here: one at a time, and the card shows what came back.
    let change =
        move |work: std::pin::Pin<Box<dyn Future<Output = Result<AssistantStatus, String>>>>| {
            if busy.get_untracked() {
                return;
            }
            busy.set(true);
            trouble.set(None);
            spawn_local(async move {
                match work.await {
                    Ok(status) => {
                        apply(&status);
                        api_key.set(String::new());
                        assistant.0.set(Some(status));
                    }
                    Err(error) => trouble.set(Some(error)),
                }
                removing.set(false);
                busy.set(false);
            });
        };
    let save = move |body: serde_json::Value| {
        change(Box::pin(async move { api::save_assistant(&body).await }));
    };

    let download = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        trouble.set(None);
        progress.set(Some(Progress::default()));
        let tag = local_tag.get_untracked();
        spawn_local(async move {
            let pulled = api::assistant_pull(&tag, |step| progress.set(Some(step))).await;
            if let Err(error) = pulled {
                trouble.set(Some(error));
            }
            if let Ok(status) = api::fetch_assistant().await {
                apply(&status);
                assistant.0.set(Some(status));
            }
            progress.set(None);
            busy.set(false);
        });
    };

    let status = move || assistant.0.get();
    let has_key = move || status().is_some_and(|status| status.credential == "set");
    let ollama_up = move || status().is_some_and(|status| status.ollama == "up");

    view! {
        <details class="card settings-section assistant-card" id="assistant" style="--i: 2">
            <summary>
                "Assistant"
                {move || status().map(|status| {
                    let (words, set) = match (status.mode.as_str(), status.ready, status.model) {
                        ("off", _, _) => ("not set".to_owned(), false),
                        (_, true, Some(model)) => (model, true),
                        _ => ("not ready".to_owned(), false),
                    };
                    view! { <span class="assistant-state" class:set=set>{words}</span> }
                })}
            </summary>
            <p class="muted small">
                "Answers questions about your home, from a model on this machine or a cloud API. \
                 It reads; it never switches anything. Until one is ready, Ask opens this card."
            </p>
            {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}
            <div
                class="modes"
                role="radiogroup"
                aria-label="Where the model runs"
                style=move || format!("--at: {}", if side.get() == Side::Cloud { 1 } else { 0 })
            >
                <span class="modes-thumb"></span>
                {[(Side::Local, "local", "On this machine"), (Side::Cloud, "cloud", "Cloud model")]
                    .into_iter()
                    .map(|(which, mode, words)| view! {
                        <button
                            type="button"
                            role="radio"
                            class:on=move || side.get() == which
                            aria-checked=move || (side.get() == which).to_string()
                            on:click=move |_| side.set(which)
                        >
                            {words}
                            {move || status().is_some_and(|status| status.mode == mode).then(|| {
                                view! { <span class="in-use" title="In use"></span> }
                            })}
                        </button>
                    })
                    .collect_view()}
            </div>
            {move || match side.get() {
                Side::Local => view! {
                    <div class="mode-body about-form">
                        <label>
                            <span>"MODEL"</span>
                            <input
                                type="text"
                                placeholder="qwen3:1.7b or https://ollama.com/library/…"
                                prop:value=move || local_tag.get()
                                on:input=move |event| local_tag.set(event_target_value(&event))
                            />
                        </label>
                        <p class="muted small">
                            "An Ollama tag, or the address of a model's page: paste a link from "
                            <a href="https://ollama.com/library" target="_blank" rel="noreferrer">
                                "ollama.com"
                            </a>
                            " or "
                            <a href="https://huggingface.co/models?library=gguf" target="_blank"
                                rel="noreferrer">"huggingface.co"</a>
                            " and it works the same. The default, qwen3:1.7b, is about 1.4 GB."
                        </p>
                        <div class="assistant-actions">
                            <button type="button" class="primary" disabled=move || busy.get()
                                on:click=download>
                                {move || match (progress.get().is_some(), ollama_up()) {
                                    (true, _) => "Working…",
                                    (false, true) => "Download",
                                    (false, false) => "Install and download",
                                }}
                            </button>
                            {move || (!ollama_up()).then(|| view! {
                                <span class="muted small">
                                    "Ollama isn't on this machine yet. This puts it in Irori's \
                                     own folder, then downloads the model."
                                </span>
                            })}
                        </div>
                        {move || progress.get().map(|step| {
                            let part = (step.total > 0)
                                .then(|| step.completed as f64 / step.total as f64);
                            view! {
                                <div class="assistant-progress" class:unknown=part.is_none()>
                                    <div class="bar">
                                        <span style=format!(
                                            "width: {:.1}%", part.unwrap_or(1.0) * 100.0
                                        )></span>
                                    </div>
                                    <span>
                                        {progress_words(&step.status)}
                                        {part.map(|part| format!(" · {:.0}%", part * 100.0))}
                                    </span>
                                </div>
                            }
                        })}
                        {move || status().filter(|status| !status.pulled.is_empty()).map(|status| {
                            view! {
                                <ul class="assistant-models">
                                    {status.pulled.into_iter().map(|pulled| {
                                        let live = pulled.active && status.mode == "local";
                                        let using = pulled.name.clone();
                                        let deleting = pulled.name.clone();
                                        let holding = pulled.name.clone();
                                        let loaded = pulled.loaded;
                                        // Loaded, it fits by being there. Otherwise it needs
                                        // what it takes, and some left for the machine.
                                        let room = loaded
                                            || pulled.needs + 200 * 1024 * 1024 <= status.memory_free;
                                        view! {
                                            <li class:live=live>
                                                <span class="name">
                                                    {pulled.name.clone()}
                                                    {loaded.then(|| view! {
                                                        <span class="loaded" title="In memory now">
                                                            "loaded"
                                                        </span>
                                                    })}
                                                </span>
                                                <span class="muted needs" class:tight=!room
                                                    title="Memory it takes once loaded">
                                                    {format!("needs {}", size(pulled.needs))}
                                                </span>
                                                <button type="button" class="quiet-button"
                                                    disabled=move || busy.get() || (!loaded && !room)
                                                    on:click=move |_| {
                                                        let tag = holding.clone();
                                                        change(Box::pin(async move {
                                                            api::assistant_hold(&tag, !loaded).await
                                                        }));
                                                    }>
                                                    {if loaded { "Unload" } else { "Load" }}
                                                </button>
                                                {if live {
                                                    view! { <span class="in-use-word">"in use"</span> }
                                                        .into_any()
                                                } else {
                                                    view! {
                                                        <button type="button" class="quiet-button"
                                                            disabled=move || busy.get()
                                                            on:click=move |_| save(serde_json::json!({
                                                                "mode": "local",
                                                                "local_tag": using.clone(),
                                                            }))>
                                                            "Use"
                                                        </button>
                                                    }
                                                    .into_any()
                                                }}
                                                <button type="button" class="quiet-button danger"
                                                    disabled=move || busy.get()
                                                    on:click=move |_| {
                                                        let tag = deleting.clone();
                                                        change(Box::pin(async move {
                                                            api::assistant_forget(&tag).await
                                                        }));
                                                    }>
                                                    "Delete"
                                                </button>
                                            </li>
                                        }
                                    }).collect_view()}
                                </ul>
                                {(status.memory_total > 0).then(|| {
                                    let used = status.memory_total.saturating_sub(status.memory_free);
                                    let part = used as f64 / status.memory_total as f64 * 100.0;
                                    view! {
                                        <div class="assistant-memory">
                                            <div class="bar"><span style=format!("width: {part:.1}%")></span></div>
                                            <span>
                                                {format!(
                                                    "{} of memory free, of {}",
                                                    size(status.memory_free),
                                                    size(status.memory_total)
                                                )}
                                            </span>
                                        </div>
                                    }
                                })}
                            }
                        })}
                        {move || status().filter(|status| status.mode == "local").map(|status| {
                            view! { <p class="assistant-detail" class:ok=status.ready>{status.detail}</p> }
                        })}
                        {move || status().is_some_and(|status| status.managed).then(|| view! {
                            <div class="assistant-actions end">
                                {move || if removing.get() {
                                    view! {
                                        <span class="small">
                                            "Remove Ollama and every model it downloaded?"
                                        </span>
                                        <button type="button" class="danger-button"
                                            disabled=move || busy.get()
                                            on:click=move |_| change(Box::pin(api::assistant_uninstall()))>
                                            "Uninstall"
                                        </button>
                                        <button type="button" class="quiet-button"
                                            on:click=move |_| removing.set(false)>
                                            "Keep it"
                                        </button>
                                    }
                                    .into_any()
                                } else {
                                    view! {
                                        <button type="button" class="quiet-button"
                                            on:click=move |_| log_open.set(true)>
                                            "Model log"
                                        </button>
                                        <button type="button" class="quiet-button danger"
                                            disabled=move || busy.get()
                                            on:click=move |_| removing.set(true)>
                                            "Uninstall Ollama"
                                        </button>
                                    }
                                    .into_any()
                                }}
                            </div>
                        })}
                    </div>
                }
                .into_any(),
                Side::Cloud => view! {
                    <form class="mode-body about-form" on:submit=move |event: ev::SubmitEvent| {
                        event.prevent_default();
                        let mut body = serde_json::json!({
                            "mode": "cloud",
                            "preset": preset.get_untracked(),
                            "base_url": base_url.get_untracked(),
                            "model": model.get_untracked(),
                        });
                        if !api_key.get_untracked().is_empty() {
                            body["api_key"] = serde_json::Value::String(api_key.get_untracked());
                        }
                        save(body);
                    }>
                        <div class="field">
                            <span>"PROVIDER"</span>
                            {crate::choices::choices(
                                "Provider",
                                [
                                    ("openai", "OpenAI"),
                                    ("anthropic", "Anthropic"),
                                    ("grok", "Grok"),
                                    ("compatible", "OpenAI-compatible"),
                                ]
                                .into_iter()
                                .map(|(value, words)| (value.to_owned(), words.to_owned()))
                                .collect(),
                                move || Some(preset.get()),
                                move || busy.get(),
                                move |next| {
                                    if let Some(url) = preset_url(&next) {
                                        base_url.set(url.to_owned());
                                    }
                                    preset.set(next);
                                },
                            )}
                        </div>
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
                                    if has_key() { "Saved. Leave blank to keep it." } else { "" }
                                }
                                prop:value=move || api_key.get()
                                on:input=move |event| api_key.set(event_target_value(&event))
                            />
                        </label>
                        <p class="muted small">
                            "A cloud answer leaves the house: what you ask, and the names and \
                             states of your devices, go to the provider. The key stays on this \
                             machine and is never shown again."
                        </p>
                        {move || status().filter(|status| status.mode == "cloud").map(|status| {
                            view! { <p class="assistant-detail" class:ok=status.ready>{status.detail}</p> }
                        })}
                        <div class="assistant-actions end">
                            {move || has_key().then(|| view! {
                                <button type="button" class="quiet-button danger"
                                    disabled=move || busy.get()
                                    on:click=move |_| {
                                        // The key going takes the cloud model with it. A local
                                        // model that is in use stays in use.
                                        let mut body = serde_json::json!({ "api_key": "" });
                                        if status().is_some_and(|status| status.mode == "cloud") {
                                            body["mode"] = "off".into();
                                        }
                                        save(body);
                                    }>
                                    "Remove credentials"
                                </button>
                            })}
                            <button type="submit" class="primary" disabled=move || busy.get()>
                                {move || if busy.get() { "Saving…" } else { "Save" }}
                            </button>
                        </div>
                    </form>
                }
                .into_any(),
            }}
            {move || status().is_some_and(|status| status.mode != "off").then(|| view! {
                <p class="assistant-off">
                    <button type="button" class="quiet-button" disabled=move || busy.get()
                        on:click=move |_| save(serde_json::json!({ "mode": "off" }))>
                        "Turn the assistant off"
                    </button>
                </p>
            })}
        </details>
    }
}

/// Seconds as a short clock: `8 s`, `1:05`.
fn clock(seconds: u32) -> String {
    if seconds < 60 {
        format!("{seconds} s")
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

/// Ollama's progress line, in the words the card uses.
fn progress_words(status: &str) -> String {
    if status.is_empty() {
        "Starting".to_owned()
    } else if status.starts_with("pulling manifest") {
        "Finding the model".to_owned()
    } else if status.starts_with("pulling") {
        "Downloading the model".to_owned()
    } else if status.starts_with("verifying") {
        "Checking the download".to_owned()
    } else if status == "success" {
        "Done".to_owned()
    } else {
        let mut words = status.to_owned();
        if let Some(first) = words.get_mut(..1) {
            first.make_ascii_uppercase();
        }
        words
    }
}

fn size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let bytes = bytes as f64;
    if bytes >= 1024.0 * MB {
        format!("{:.1} GB", bytes / (1024.0 * MB))
    } else {
        format!("{:.0} MB", bytes / MB)
    }
}

fn preset_url(preset: &str) -> Option<&'static str> {
    match preset {
        "openai" => Some("https://api.openai.com/v1"),
        "anthropic" => Some("https://api.anthropic.com"),
        "grok" => Some("https://api.x.ai/v1"),
        _ => None,
    }
}

/// Opens the assistant card and scrolls to it when the address says `#assistant`.
pub fn watch_hash() {
    let location = use_location();
    Effect::new(move |_| {
        let hash = location.hash.get();
        if hash == "#assistant" || hash == "assistant" {
            let Some(section) = document().get_element_by_id("assistant") else {
                return;
            };
            // It is folded until wanted, and this is it being wanted.
            let _ = section.set_attribute("open", "");
            section.scroll_into_view();
            let again = section.get_attribute("data-flash").as_deref() == Some("a");
            let _ = section.set_attribute("data-flash", if again { "b" } else { "a" });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_said_in_the_cards_own_words() {
        assert_eq!(
            progress_words("pulling 6f96e01a3f55"),
            "Downloading the model"
        );
        assert_eq!(progress_words("installing Ollama"), "Installing Ollama");
        assert_eq!(progress_words(""), "Starting");
        assert_eq!(size(1_400_000_000), "1.3 GB");
        assert_eq!(size(300 * 1024 * 1024), "300 MB");
    }
}
