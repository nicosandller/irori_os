//! The assistant: a Settings card, a quiet Ask button, and one chat per view.
//!
//! Ask is a real button before a model is ready. It opens Settings at this card. Once a model
//! is ready it opens a chat window under itself, about the thing the page is showing, and the
//! sidebar grows an entry for the chat about the whole home.

use std::collections::HashMap;

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::NavigateOptions;
use leptos_router::hooks::{use_location, use_navigate};
use web_sys::wasm_bindgen::JsCast as _;

use crate::Assistant;
use crate::api::{self, AssistantStatus, Progress, Streamed};

/// One conversation as the page holds it: what was said, what is being typed, and the answer
/// on its way.
#[derive(Debug, Clone, Copy)]
struct Thread {
    messages: RwSignal<Vec<api::AssistantMessage>>,
    draft: RwSignal<String>,
    asking: RwSignal<bool>,
    phase: RwSignal<Phase>,
    /// The answer so far, while it is being written.
    writing: RwSignal<String>,
    trouble: RwSignal<Option<String>>,
    /// The question a failed answer was for, so it can be asked again without typing it.
    unanswered: RwSignal<Option<String>>,
    /// Seconds since the question was sent.
    waited: RwSignal<u32>,
    /// Which wait the clock is counting: a new question starts a new one.
    round: RwSignal<u32>,
}

/// Every conversation this page has opened, by what it is about. Kept by the shell and not by
/// the chat drawn on a page, so a question asked on one page is still there, and still being
/// answered, after a look at another.
#[derive(Debug, Clone)]
pub struct Chats {
    /// The shell's own owner. A conversation's signals belong to it, so they outlive the page
    /// that first opened the conversation.
    owner: Owner,
    threads: StoredValue<HashMap<String, Thread>>,
}

impl Chats {
    /// Call from the shell, whose lifetime the conversations take.
    pub fn new() -> Self {
        Self {
            owner: Owner::current().unwrap_or_default(),
            threads: StoredValue::new(HashMap::new()),
        }
    }

    fn thread(&self, scope: &str) -> Thread {
        if let Some(thread) = self
            .threads
            .with_value(|threads| threads.get(scope).copied())
        {
            return thread;
        }
        let thread = self.owner.with(|| Thread {
            messages: RwSignal::new(Vec::new()),
            draft: RwSignal::new(String::new()),
            asking: RwSignal::new(false),
            phase: RwSignal::new(Phase::Thinking),
            writing: RwSignal::new(String::new()),
            trouble: RwSignal::new(None),
            unanswered: RwSignal::new(None),
            waited: RwSignal::new(0),
            round: RwSignal::new(0),
        });
        self.threads.update_value(|threads| {
            threads.insert(scope.to_owned(), thread);
        });
        thread
    }
}

impl Thread {
    /// Shows that an answer is on its way, and counts the seconds until it is here. A model
    /// on a small machine can take minutes; the count says it is still going.
    fn wait(self, already: u32) {
        self.writing.set(String::new());
        self.phase.set(Phase::Thinking);
        self.waited.set(already);
        self.asking.set(true);
        let round = self.round.get_untracked() + 1;
        self.round.set(round);
        spawn_local(async move {
            loop {
                gloo_timers::future::sleep(std::time::Duration::from_secs(1)).await;
                if !self.asking.get_untracked() || self.round.get_untracked() != round {
                    return;
                }
                self.waited.update(|seconds| *seconds += 1);
            }
        });
    }

    /// What each piece of an answer does to the page.
    fn hear(self) -> impl FnMut(Streamed) {
        move |event| match event {
            Streamed::Delta(delta) => {
                self.writing.update(|answer| answer.push_str(&delta));
                self.phase.set(Phase::Writing);
            }
            Streamed::Step(tool) => self.phase.set(Phase::Looking(tool)),
            Streamed::Failed(_) | Streamed::Done => {}
        }
    }

    /// The answer to `question` has ended, well or badly. What Irori kept is what is shown:
    /// a reply that failed or was stopped isn't kept, and neither is its question.
    async fn settle(self, scope: &str, question: String, result: Result<(), String>) {
        let kept = api::assistant_transcript(scope).await;
        match (kept, &result) {
            (Ok(kept), _) => self.messages.set(kept.turns),
            (Err(_), Ok(())) => {
                let body = self.writing.get_untracked();
                self.messages.update(|turns| {
                    turns.push(api::AssistantMessage {
                        role: "assistant".to_owned(),
                        body: body.trim_end().to_owned(),
                    })
                });
            }
            (Err(_), Err(_)) => {}
        }
        if let Err(error) = result {
            self.trouble.set(Some(error));
            self.unanswered.set(Some(question));
        }
        self.writing.set(String::new());
        self.asking.set(false);
    }

    /// Reads the conversation as Irori has it. A question Irori is still answering, asked
    /// before this page was loaded or from somewhere else, is joined where it has got to.
    fn load(self, scope: String) {
        // An answer this page is already reading is ahead of anything it could fetch.
        if self.asking.get_untracked() {
            return;
        }
        spawn_local(async move {
            let kept = match api::assistant_transcript(&scope).await {
                Ok(kept) => kept,
                Err(error) => {
                    self.trouble.set(Some(error));
                    return;
                }
            };
            if self.asking.get_untracked() {
                return;
            }
            // Coming back to a conversation nothing was added to draws nothing again.
            if self.messages.with_untracked(|turns| *turns != kept.turns) {
                self.messages.set(kept.turns);
            }
            let Some(pending) = kept.pending else {
                return;
            };
            if let Some(why) = pending.failed {
                self.trouble.set(Some(why));
                self.unanswered.set(Some(pending.question));
                return;
            }
            self.trouble.set(None);
            self.unanswered.set(None);
            self.messages.update(|turns| {
                turns.push(api::AssistantMessage {
                    role: "user".to_owned(),
                    body: pending.question.clone(),
                })
            });
            self.wait(pending.seconds);
            let result = api::assistant_follow(&scope, self.hear()).await;
            self.settle(&scope, pending.question, result).await;
        });
    }

    /// Asks what is in the box.
    fn ask(self, scope: String) {
        if self.asking.get_untracked() {
            return;
        }
        let text = self.draft.get_untracked().trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.draft.set(String::new());
        self.trouble.set(None);
        self.unanswered.set(None);
        self.wait(0);
        self.messages.update(|turns| {
            turns.push(api::AssistantMessage {
                role: "user".to_owned(),
                body: text.clone(),
            })
        });
        spawn_local(async move {
            let result = api::assistant_ask(&scope, &text, self.hear()).await;
            self.settle(&scope, text, result).await;
        });
    }

    /// Stops the answer on its way. Its question goes back in the box, to change or send again.
    fn stop(self, scope: String) {
        let asked = self.messages.with_untracked(|turns| {
            turns
                .last()
                .filter(|turn| turn.role == "user")
                .map(|turn| turn.body.clone())
        });
        spawn_local(async move {
            match api::assistant_stop(&scope).await {
                Ok(()) => {
                    if let Some(asked) = asked
                        && self.draft.with_untracked(String::is_empty)
                    {
                        self.draft.set(asked);
                    }
                }
                Err(error) => self.trouble.set(Some(error)),
            }
        });
    }
}

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
                "read_logs" => "Reading the log",
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
    let thread = expect_context::<Chats>().thread(&scope);
    let Thread {
        messages,
        draft,
        asking,
        phase,
        writing,
        trouble,
        unanswered,
        waited,
        ..
    } = thread;
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

    thread.load(scope.get_value());

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

    let send = Callback::new(move |_: ()| thread.ask(scope.get_value()));

    let clear = move |_| {
        let scope = scope.get_value();
        spawn_local(async move {
            match api::assistant_clear(&scope).await {
                Ok(()) => {
                    messages.set(Vec::new());
                    trouble.set(None);
                    unanswered.set(None);
                }
                Err(error) => trouble.set(Some(error)),
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
                // Irori answers whether or not this chat is open, so while it does, the button
                // is the way to call the answer off.
                {move || if asking.get() {
                    view! {
                        <button
                            type="button"
                            class="send stop"
                            aria-label="Stop"
                            title="Stop answering"
                            on:click=move |_| thread.stop(scope.get_value())
                        >
                            <svg viewBox="0 0 24 24" aria-hidden="true">
                                <rect x="7" y="7" width="10" height="10" rx="1.6"
                                    fill="currentColor" />
                            </svg>
                        </button>
                    }
                    .into_any()
                } else {
                    view! {
                        <button
                            type="submit"
                            class="send"
                            aria-label="Send"
                            disabled=move || draft.with(|text| text.trim().is_empty())
                        >
                            <svg viewBox="0 0 24 24" aria-hidden="true">
                                <path d="M12 19V5M5.5 11.5L12 5l6.5 6.5" fill="none"
                                    stroke="currentColor" stroke-width="2.2"
                                    stroke-linecap="round" stroke-linejoin="round" />
                            </svg>
                        </button>
                    }
                    .into_any()
                }}
            </form>
        </section>
    }
}

/// The two ways a model can answer, as the Settings card shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Local,
    Cloud,
}

/// Which side of the Settings card is showing. `None` until the first status says which model
/// is in use; after that only a click on the slider changes it. Kept by the shell, because
/// the card is drawn again each time its row opens, and would otherwise choose again.
#[derive(Debug, Clone, Copy)]
pub struct ModelSide(pub RwSignal<Option<Side>>);

/// The side the card opens on: the one whose model is in use, and this machine when none is.
fn first_side(mode: &str) -> Side {
    if mode == "cloud" {
        Side::Cloud
    } else {
        Side::Local
    }
}

/// What the Assistant row of Settings opens to. Always there; a model is optional.
#[component]
pub fn Section() -> impl IntoView {
    let assistant = expect_context::<Assistant>();
    let shown = expect_context::<ModelSide>().0;
    let side = Memo::new(move |_| shown.get().unwrap_or(Side::Local));
    let local_tag = RwSignal::new("qwen3:1.7b".to_owned());
    let preset = RwSignal::new("openai".to_owned());
    let base_url = RwSignal::new(String::new());
    let model = RwSignal::new(String::new());
    let api_key = RwSignal::new(String::new());
    let loaded = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let progress = RwSignal::new(None::<Progress>);
    let removing = RwSignal::new(false);
    // The model being loaded or unloaded right now, which on a small machine takes a while.
    let holding = RwSignal::new(None::<String>);
    let log_open = expect_context::<ModelLog>().0;
    let trouble = RwSignal::new(None::<String>);

    // The fields, as Irori has them. The slider is not one of them: nothing Irori answers
    // moves it.
    let apply = move |status: &AssistantStatus| {
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
            if shown.get_untracked().is_none() {
                shown.set(Some(first_side(&status.mode)));
            }
        }
    });

    spawn_local(async move {
        match api::fetch_assistant().await {
            Ok(status) => assistant.0.set(Some(status)),
            Err(error) => trouble.set(Some(error)),
        }
    });

    // Every change goes through here: one at a time, and the card shows what came back.
    // `saved` is whether the fields themselves were what was sent: only then are they read
    // back, so loading or deleting a model leaves a half-typed field alone.
    let change =
        move |work: std::pin::Pin<Box<dyn Future<Output = Result<AssistantStatus, String>>>>,
              saved: bool| {
            if busy.get_untracked() {
                return;
            }
            busy.set(true);
            trouble.set(None);
            spawn_local(async move {
                match work.await {
                    Ok(status) => {
                        if saved {
                            apply(&status);
                            api_key.set(String::new());
                        }
                        assistant.0.set(Some(status));
                    }
                    Err(error) => trouble.set(Some(error)),
                }
                removing.set(false);
                holding.set(None);
                busy.set(false);
                // Memory a model let go of is counted a moment later. Look once more, so the
                // bar and what fits are not judged on the reading from before.
                gloo_timers::future::sleep(std::time::Duration::from_secs(2)).await;
                if let Ok(status) = api::fetch_assistant().await {
                    assistant.0.try_set(Some(status));
                }
            });
        };
    let save = move |body: serde_json::Value| {
        change(
            Box::pin(async move { api::save_assistant(&body).await }),
            true,
        );
    };

    // Installing Ollama alone (`with_model` false), or downloading a model, which installs
    // Ollama first when it is missing.
    let fetch = move |with_model: bool| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        trouble.set(None);
        progress.set(Some(Progress::default()));
        let tag = local_tag.get_untracked();
        spawn_local(async move {
            let pulled = if with_model {
                api::assistant_pull(&tag, |step| progress.set(Some(step))).await
            } else {
                api::assistant_install(|step| progress.set(Some(step))).await
            };
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
        <div class="assistant-card">
            <p class="muted small">
                "Answers questions about your home, from a model on this machine or a cloud API. \
                 It reads; it never switches anything. Until one is ready, Ask opens this row."
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
                            on:click=move |_| shown.set(Some(which))
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
                        // Ollama first: it is what runs a model, so it comes before there is
                        // a model to download or load.
                        <div class="assistant-ollama">
                            <div class="field">
                                <span>"OLLAMA"</span>
                                <p class="assistant-ollama-state">
                                    {move || {
                                        let (up, managed) = status()
                                            .map_or((false, false), |status| {
                                                (status.ollama == "up", status.managed)
                                            });
                                        match (up, managed) {
                                            (true, true) => "Installed by Irori, and running.",
                                            (true, false) => {
                                                "Running. It was installed outside Irori, so it is \
                                                 yours to remove."
                                            }
                                            (false, true) => "Installed by Irori, but not running.",
                                            (false, false) => {
                                                "Not installed. Irori can put it in its own folder; \
                                                 nothing else on the machine is touched."
                                            }
                                        }
                                    }}
                                </p>
                            </div>
                            <div class="assistant-actions">
                                {move || {
                                    let (up, managed) = status().map_or((false, false), |status| {
                                        (status.ollama == "up", status.managed)
                                    });
                                    (!up).then(|| view! {
                                        <button type="button" class="primary"
                                            disabled=move || busy.get()
                                            on:click=move |_| fetch(false)>
                                            {move || match (progress.get().is_some(), managed) {
                                                (true, _) => "Working…",
                                                (false, true) => "Start Ollama",
                                                (false, false) => "Install Ollama",
                                            }}
                                        </button>
                                    })
                                }}
                                {move || status().is_some_and(|status| status.managed).then_some({
                                    move || if removing.get() {
                                        view! {
                                            <span class="small">
                                                "Remove Ollama and every model it downloaded?"
                                            </span>
                                            <button type="button" class="danger-button"
                                                disabled=move || busy.get()
                                                on:click=move |_| change(Box::pin(api::assistant_uninstall()), false)>
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
                                    }
                                })}
                            </div>
                        </div>
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
                            <button type="button" class="primary"
                                disabled=move || busy.get() || !ollama_up()
                                on:click=move |_| fetch(true)>
                                {move || if progress.get().is_some() { "Working…" } else { "Download" }}
                            </button>
                            {move || (!ollama_up()).then(|| view! {
                                <span class="muted small">"Install Ollama above first."</span>
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
                                    {status.pulled.clone().into_iter().map(|pulled| {
                                        let held_by_others: u64 = status
                                            .pulled
                                            .iter()
                                            .filter(|other| other.loaded && other.name != pulled.name)
                                            .map(|other| other.needs)
                                            .sum();
                                        let live = pulled.active && status.mode == "local";
                                        let deleting = pulled.name.clone();
                                        let held = pulled.name.clone();
                                        let label = pulled.name.clone();
                                        let loaded = pulled.loaded;
                                        // Loaded, it fits by being there. Otherwise it needs
                                        // what it takes, and some left for the machine.
                                        // Loading it lets go of any other, so their
                                        // memory counts as room too.
                                        let room = loaded
                                            || pulled.needs + 200 * 1024 * 1024
                                                <= status.memory_free + held_by_others;
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
                                                    on:click={
                                                        let tag = held.clone();
                                                        move |_| {
                                                            let tag = tag.clone();
                                                            holding.set(Some(tag.clone()));
                                                            change(Box::pin(async move {
                                                                api::assistant_hold(&tag, !loaded).await
                                                            }), false);
                                                        }
                                                    }>
                                                    {move || {
                                                        let now = holding.get().as_deref() == Some(label.as_str());
                                                        match (now, loaded) {
                                                            (true, true) => "Unloading…",
                                                            (true, false) => "Loading…",
                                                            (false, true) => "Unload",
                                                            (false, false) => "Load",
                                                        }
                                                    }}
                                                </button>
                                                {live.then(|| view! {
                                                    <span class="in-use-word">"in use"</span>
                                                })}
                                                <button type="button" class="quiet-button danger"
                                                    disabled=move || busy.get()
                                                    on:click=move |_| {
                                                        let tag = deleting.clone();
                                                        change(Box::pin(async move {
                                                            api::assistant_forget(&tag).await
                                                        }), false);
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
                            "A cloud answer leaves the house: what you ask goes to the provider, \
                             with the names and states of your devices, and, when you ask from \
                             Settings, Irori's settings and lines of its log. The key stays on \
                             this machine and is never shown again."
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
        </div>
    }
}

/// How the assistant stands, for the heading of its row in Settings: the model that answers,
/// or that none does yet.
pub fn state() -> AnyView {
    let assistant = expect_context::<Assistant>();
    (move || {
        assistant.0.get().map(|status| {
            let (words, set) = match (status.mode.as_str(), status.ready, status.model) {
                ("off", _, _) => ("not set".to_owned(), false),
                (_, true, Some(model)) => (model, true),
                _ => ("not ready".to_owned(), false),
            };
            view! { <span class:set=set>{words}</span> }
        })
    })
    .into_any()
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
    } else if status.starts_with("loading") {
        "Loading the model into memory".to_owned()
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

    #[test]
    fn the_card_opens_on_the_model_in_use_and_on_this_machine_when_none_is() {
        assert_eq!(first_side("cloud"), Side::Cloud);
        assert_eq!(first_side("local"), Side::Local);
        assert_eq!(first_side("off"), Side::Local);
    }
}
