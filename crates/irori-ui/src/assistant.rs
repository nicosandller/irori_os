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
    /// How much of the model's context the last answer took.
    used: RwSignal<Option<api::AssistantContext>>,
    /// The Floorplan page's working copy, while that page is open.
    desk: StoredValue<Option<Desk>>,
    /// The picture or PDF that will go with the next question.
    attached: RwSignal<Option<api::Attachment>>,
    /// Floors and rooms the assistant asked to have removed, waiting on a yes or a no.
    asked: RwSignal<Vec<api::Removal>>,
}

/// The plan being edited on the Floorplan page, as the assistant reaches it: `read` is the
/// working copy while somebody is editing (and nothing otherwise), and `take` hands the page a
/// plan the model drew, to keep as one step it can undo.
///
/// The page lays this down while it is open and lifts it when it goes, so a conversation never
/// holds on to an editor that is no longer there.
#[derive(Debug, Clone, Copy)]
pub struct Desk {
    pub read: Callback<(), Option<irori_types::Floorplan>>,
    /// The conversation it was drawn in, and the plan: the page takes it only for the floor it
    /// is showing.
    pub take: Callback<(String, irori_types::Floorplan)>,
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
    desk: StoredValue<Option<Desk>>,
}

impl Chats {
    /// Call from the shell, whose lifetime the conversations take.
    pub fn new() -> Self {
        Self {
            owner: Owner::current().unwrap_or_default(),
            threads: StoredValue::new(HashMap::new()),
            desk: StoredValue::new(None),
        }
    }

    /// The Floorplan page saying where its working copy is, or — with `None` — that it has
    /// gone.
    pub fn lay(&self, desk: Option<Desk>) {
        self.desk.set_value(desk);
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
            used: RwSignal::new(None),
            desk: self.desk,
            attached: RwSignal::new(None),
            asked: RwSignal::new(Vec::new()),
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
    fn hear(self, scope: &str) -> impl FnMut(Streamed) + use<> {
        let scope = scope.to_owned();
        move |event| match event {
            Streamed::Delta(delta) => {
                self.writing.update(|answer| answer.push_str(&delta));
                self.phase.set(Phase::Writing);
            }
            Streamed::Step(tool) => self.phase.set(Phase::Looking(tool)),
            // Only the Floorplan page can take a plan, and only while it is there to.
            Streamed::Plan(plan) => {
                if let Some(desk) = self.desk.get_value() {
                    desk.take.run((scope.clone(), *plan));
                }
            }
            // Each is asked once, however many times the answer is read from its start.
            Streamed::Confirm(removals) => self.asked.update(|asked| {
                for removal in removals {
                    if !asked.contains(&removal) {
                        asked.push(removal);
                    }
                }
            }),
            Streamed::Failed(_) | Streamed::Done => {}
        }
    }

    /// The answer to `question` has ended, well or badly. What Irori kept is what is shown:
    /// a reply that failed or was stopped isn't kept, and neither is its question.
    async fn settle(self, scope: &str, question: String, result: Result<(), String>) {
        let kept = api::assistant_transcript(scope).await;
        // Only what Irori says it kept is shown as kept: an answer that ended may have been
        // stopped, and the words written so far are then nobody's reply.
        match kept {
            Ok(kept) => {
                self.messages.set(kept.turns);
                self.used.set(kept.context);
            }
            Err(error) if result.is_ok() => self.trouble.set(Some(error)),
            Err(_) => {}
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
            if self.used.get_untracked() != kept.context {
                self.used.set(kept.context);
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
            let result = api::assistant_follow(&scope, self.hear(&scope)).await;
            self.settle(&scope, pending.question, result).await;
        });
    }

    /// Asks what is in the box.
    fn ask(self, scope: String) {
        if self.asking.get_untracked() {
            return;
        }
        let attachment = self.attached.get_untracked();
        let text = self.draft.get_untracked().trim().to_owned();
        // A picture with nothing said about it is a request to draw it.
        let text = match (&attachment, text.is_empty()) {
            (Some(_), true) => "Draw this floorplan.".to_owned(),
            (None, true) => return,
            _ => text,
        };
        self.draft.set(String::new());
        self.attached.set(None);
        self.trouble.set(None);
        self.unanswered.set(None);
        self.wait(0);
        self.messages.update(|turns| {
            turns.push(api::AssistantMessage {
                role: "user".to_owned(),
                body: text.clone(),
            })
        });
        // The plan being edited goes with a question asked on the Floorplan page, so the model
        // sees — and may draw on — what is on the screen and not what was last saved.
        let plan = scope
            .starts_with("floorplan:")
            .then(|| self.desk.get_value())
            .flatten()
            .and_then(|desk| desk.read.run(()));
        spawn_local(async move {
            let result = api::assistant_ask(
                &scope,
                &text,
                plan.as_ref(),
                attachment.as_ref(),
                self.hear(&scope),
            )
            .await;
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
        // How much had been said before this question.
        let before = self.messages.with_untracked(Vec::len).saturating_sub(1);
        spawn_local(async move {
            match api::assistant_stop(&scope).await {
                Ok(()) => {
                    // An answer that finished just as it was stopped was kept after all, and
                    // its question then isn't one to send again.
                    let kept = api::assistant_transcript(&scope).await;
                    if let Some(asked) = asked
                        && kept.is_ok_and(|kept| kept.turns.len() <= before)
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

/// A paperclip: a file to go with the question.
const CLIP: &str = r#"<path d="M20 11.5l-8 8a5 5 0 0 1-7-7l8.5-8.5a3.3 3.3 0 0 1 4.7 4.7L9.8 17a1.7 1.7 0 0 1-2.4-2.4L15 7" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>"#;

/// The most a file to attach may be: what Irori will take (`Attachment::MOST` on the server).
const MOST_ATTACHED: f64 = 8.0 * 1024.0 * 1024.0;

/// Reads a chosen file into what goes with a question, or says why it can't go.
async fn read_file(file: web_sys::File) -> Result<api::Attachment, String> {
    let media_type = file.type_();
    let known = matches!(
        media_type.as_str(),
        "image/png" | "image/jpeg" | "image/webp" | "image/gif" | "application/pdf"
    );
    if !known {
        return Err(
            "That isn't a picture or a PDF. A PNG, JPEG, WebP, GIF or PDF can be read.".into(),
        );
    }
    if file.size() > MOST_ATTACHED {
        return Err("That file is too big. Up to 8 MB can be read.".into());
    }
    let buffer = wasm_bindgen_futures::JsFuture::from(file.array_buffer())
        .await
        .map_err(|_| "That file couldn't be read.".to_owned())?;
    let bytes = web_sys::js_sys::Uint8Array::new(&buffer).to_vec();
    Ok(api::Attachment {
        name: file.name(),
        media_type,
        data: api::base64(&bytes),
    })
}

/// How wide the chat window is drawn, in pixels: `.ask-pop`'s 26rem.
const POP_WIDTH: f64 = 416.0;

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
        // Its right edge under the button's, unless the button is so far left that the chat
        // would hang off that side of the window: then as far left as it fits.
        right: (width - right)
            .min(width - POP_WIDTH.min(width - 24.0) - 12.0)
            .max(12.0),
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
                "read_settings" => "Reading Irori's settings",
                "list_automations" => "Looking at the automations",
                "get_automation" => "Reading an automation",
                "read_floorplan" => "Looking at the plan",
                "edit_floorplan" => "Drawing on the plan",
                "edit_home" => "Arranging floors and rooms",
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
        used,
        attached,
        asked,
        ..
    } = thread;
    let live = expect_context::<crate::Live>();
    // Removes what the person said yes to, as the person: the same request Settings makes,
    // held to the same rules about who may.
    let remove = move |removal: api::Removal| {
        spawn_local(async move {
            let done = match removal.kind.as_str() {
                "floor" => match removal.id.parse() {
                    Ok(id) => api::remove_floor(&id).await,
                    Err(_) => Err(format!("there's no floor `{}`", removal.id)),
                },
                _ => match removal.id.parse() {
                    Ok(id) => api::remove_area(&id).await,
                    Err(_) => Err(format!("there's no room `{}`", removal.id)),
                },
            };
            match done {
                Ok(()) => {
                    trouble.set(None);
                    crate::refresh(live);
                }
                Err(why) => trouble.set(Some(why)),
            }
            asked.update(|asked| asked.retain(|waiting| *waiting != removal));
        });
    };
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
                    used.set(None);
                    trouble.set(None);
                    unanswered.set(None);
                }
                Err(error) => trouble.set(Some(error)),
            }
        });
    };

    let empty = move || messages.with(Vec::is_empty) && !asking.get();
    let about = title.clone();
    // The one conversation that can change something: the drawing on the Floorplan page.
    let on_plan = scope.with_value(|scope| scope.starts_with("floorplan:"));
    let local = move || {
        assistant
            .0
            .get()
            .is_some_and(|status| status.mode == "local")
    };

    // Takes a file to go with the next question, however it arrived: chosen, or dropped.
    let attach = move |file: web_sys::File| {
        spawn_local(async move {
            match read_file(file).await {
                Ok(file) => {
                    trouble.set(None);
                    attached.set(Some(file));
                }
                Err(why) => trouble.set(Some(why)),
            }
        });
    };
    // How many of the chat's parts a dragged file is over. A drag leaves one part of the chat
    // as it enters the next, so "over the chat" is counted, not switched on and off.
    let over = RwSignal::new(0i32);
    let held = move || over.get() > 0;
    // Only a file, and only where a file is taken: dragging a line of text across the chat
    // is not an offer of anything.
    let offered = move |event: &ev::DragEvent| {
        on_plan
            && event.data_transfer().is_some_and(|carried| {
                carried
                    .types()
                    .iter()
                    .any(|kind| kind.as_string().as_deref() == Some("Files"))
            })
    };

    view! {
        <section
            class="chat"
            class:dropping=held
            on:dragenter=move |event: ev::DragEvent| {
                if offered(&event) {
                    event.prevent_default();
                    over.update(|over| *over += 1);
                }
            }
            on:dragover=move |event: ev::DragEvent| {
                // Without this the browser doesn't let the drop happen here at all, and
                // opens the file in place of the page.
                if offered(&event) {
                    event.prevent_default();
                }
            }
            on:dragleave=move |event: ev::DragEvent| {
                if offered(&event) {
                    over.update(|over| *over = (*over - 1).max(0));
                }
            }
            on:drop=move |event: ev::DragEvent| {
                if !offered(&event) {
                    return;
                }
                event.prevent_default();
                over.set(0);
                let dropped = event
                    .data_transfer()
                    .and_then(|carried| carried.files())
                    .and_then(|files| files.get(0));
                if let Some(file) = dropped {
                    attach(file);
                }
            }
        >
            // What letting go here will do, said over the chat while a file is held over it.
            {move || held().then(|| view! {
                <div class="chat-drop" aria-hidden="true">
                    <svg viewBox="0 0 24 24" inner_html=CLIP></svg>
                    <span>"Drop a picture or PDF of a floorplan"</span>
                </div>
            })}
            <header class="chat-head">
                <Spark />
                <span class="chat-title">{title}</span>
                {move || used.get().map(|used| {
                    let (words, part) = context_words(used);
                    view! {
                        <span
                            class="chat-context"
                            class:high=part.is_some_and(|part| part >= 80.0)
                            title=context_title(used)
                        >
                            {part.map(|part| view! {
                                <span class="bar">
                                    <span style=format!("width: {part:.0}%")></span>
                                </span>
                            })}
                            {words}
                        </span>
                    }
                })}
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
                {move || empty().then(|| if on_plan {
                    view! {
                        <p class="chat-empty">
                            "Ask about " {about.clone()} ": its rooms, what's in them, and the \
                             automations that act on them. "
                            {if local() {
                                "Drawing on the plan needs a cloud model; the one on this \
                                 machine can only answer."
                            } else {
                                "It can draw too — walls, doors, windows and rooms, from what you \
                                 tell it or from a picture or PDF of a plan you attach or drop \
                                 here — as one step you can undo. Nothing is saved until you press Save."
                            }}
                        </p>
                    }.into_any()
                } else {
                    view! {
                        <p class="chat-empty">
                            "Ask about " {about.clone()} ". Answers come from what Irori can see \
                             right now; nothing is changed."
                        </p>
                    }.into_any()
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
            // What the assistant asked to have removed. It can't remove anything itself, so
            // each is put here as a question, and the answer is the person's.
            {move || asked.get().into_iter().map(|removal| {
                let (yes, no) = (removal.clone(), removal.clone());
                let what = if removal.kind == "floor" { "floor" } else { "room" };
                view! {
                    <div class="chat-confirm" role="alertdialog" aria-label="Remove?">
                        <p>
                            "Remove the " {what} " " <b>{removal.name.clone()}</b> "? "
                            <span class="muted">
                                {if removal.kind == "floor" {
                                    "Its rooms stay, on no floor. This is done at once, not on Save."
                                } else {
                                    "Its devices stay, in no room. This is done at once, not on Save."
                                }}
                            </span>
                        </p>
                        <span class="chat-confirm-actions">
                            <button type="button" class="press"
                                on:click=move |_| {
                                    let no = no.clone();
                                    asked.update(|asked| asked.retain(|waiting| *waiting != no));
                                }>
                                "Keep it"
                            </button>
                            <button type="button" class="press danger"
                                on:click=move |_| remove(yes.clone())>
                                "Remove"
                            </button>
                        </span>
                    </div>
                }
            }).collect_view()}
            // The file that will go with the next question, and the way to take it back.
            {move || attached.get().map(|file| view! {
                <p class="attached">
                    <svg viewBox="0 0 24 24" aria-hidden="true" inner_html=CLIP></svg>
                    <span class="attached-name">{file.name}</span>
                    <button
                        type="button"
                        aria-label="Don't send this file"
                        title="Don't send this file"
                        on:click=move |_| attached.set(None)
                    >
                        "×"
                    </button>
                </p>
            })}
            <form
                class="composer"
                on:submit=move |event: ev::SubmitEvent| {
                    event.prevent_default();
                    send.run(());
                }
            >
                // Only where there is something to draw from a picture: the Floorplan's chat.
                {on_plan.then(|| view! {
                    <label class="attach" title="Attach a picture or PDF of a floorplan">
                        <span class="visually-hidden">"Attach a picture or PDF of a floorplan"</span>
                        <svg viewBox="0 0 24 24" aria-hidden="true" inner_html=CLIP></svg>
                        <input
                            type="file"
                            accept="image/png,image/jpeg,image/webp,image/gif,application/pdf"
                            on:change=move |event| {
                                let input = event_target::<web_sys::HtmlInputElement>(&event);
                                let file = input.files().and_then(|files| files.get(0));
                                // The same file can be chosen again after it's taken back.
                                input.set_value("");
                                if let Some(file) = file {
                                    attach(file);
                                }
                            }
                        />
                    </label>
                })}
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
                            disabled=move || {
                                draft.with(|text| text.trim().is_empty())
                                    && attached.with(Option::is_none)
                            }
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
    // As typed: a number of tokens, checked when it is applied.
    let context = RwSignal::new(String::new());
    let instructions = RwSignal::new(String::new());
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
            context.set(status.local_context.to_string());
            instructions.set(status.instructions.clone());
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
    // What is typed and isn't yet what Irori has: only then is there something to save.
    let context_changed = move || {
        status().is_some_and(|status| context.get().trim() != status.local_context.to_string())
    };
    let instructions_changed =
        move || status().is_some_and(|status| instructions.get().trim() != status.instructions);
    // These two are saved on their own, and only their own field is read back: a model or
    // a key half typed in the form beside them stays as it was typed.
    let apply_context = move || match context.get_untracked().trim().parse::<u64>() {
        Ok(tokens) => change(
            Box::pin(async move {
                let status = api::save_assistant(&serde_json::json!({ "context": tokens })).await?;
                context.set(status.local_context.to_string());
                Ok(status)
            }),
            false,
        ),
        Err(_) => trouble.set(Some(
            "A context is a number of tokens, such as 8192.".into(),
        )),
    };
    let save_instructions = move || {
        let text = instructions.get_untracked();
        change(
            Box::pin(async move {
                let status =
                    api::save_assistant(&serde_json::json!({ "instructions": text })).await?;
                instructions.set(status.instructions.clone());
                Ok(status)
            }),
            false,
        );
    };
    let has_key = move || status().is_some_and(|status| status.credential == "set");
    // The cloud model as it was saved is shown, not offered for typing: the fields are still,
    // the key is a row of dots, and Save has gone, so there is no wondering whether it took.
    // `changing` is somebody having asked to type in them again.
    let changing = RwSignal::new(false);
    let settled = move || {
        has_key() && status().is_some_and(|status| status.mode == "cloud") && !changing.get()
    };
    // Whether the model that was saved answers: being asked, and what came of it.
    let checking = RwSignal::new(false);
    let answered = RwSignal::new(None::<Result<(), String>>);
    let check = move || {
        if checking.get_untracked() {
            return;
        }
        checking.set(true);
        answered.set(None);
        spawn_local(async move {
            let result = api::assistant_check().await;
            // A model that doesn't answer is one to go back and put right.
            changing.set(result.is_err());
            answered.set(Some(result));
            checking.set(false);
        });
    };
    // Saving the cloud model is two things: Irori keeping it, and the model being asked one
    // small thing to find out whether the key, the name and the address are right. Only when
    // it answers do the fields go still.
    let save_cloud = move |body: serde_json::Value| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        trouble.set(None);
        answered.set(None);
        spawn_local(async move {
            match api::save_assistant(&body).await {
                Ok(status) => {
                    apply(&status);
                    api_key.set(String::new());
                    let ready = status.ready && status.mode == "cloud";
                    assistant.0.set(Some(status));
                    changing.set(true);
                    if ready {
                        check();
                    }
                }
                Err(error) => trouble.set(Some(error)),
            }
            busy.set(false);
        });
    };
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
                        <div class="field">
                            <span>"CONTEXT"</span>
                            <div class="assistant-context">
                                <input
                                    type="number"
                                    aria-label="Context, in tokens"
                                    inputmode="numeric"
                                    step="1024"
                                    min=move || status().map(|status| status.context_min.to_string())
                                    max=move || status().map(|status| status.context_max.to_string())
                                    prop:value=move || context.get()
                                    on:input=move |event| context.set(event_target_value(&event))
                                    on:keydown=move |event: ev::KeyboardEvent| {
                                        if event.key() == "Enter" {
                                            event.prevent_default();
                                            apply_context();
                                        }
                                    }
                                />
                                <span class="muted small">"tokens"</span>
                                <button type="button" class="quiet-button"
                                    disabled=move || busy.get() || !context_changed()
                                    on:click=move |_| apply_context()>
                                    {move || if busy.get() && context_changed() { "Applying…" } else { "Apply" }}
                                </button>
                            </div>
                        </div>
                        <p class="muted small">
                            "How much the model holds at once: the picture of your home, the \
                             conversation so far, and its answer. More lets it be told more and \
                             remember further back, and takes more memory; the model in use is \
                             loaded again to change it. Each chat shows how much of it the last \
                             answer took."
                            {move || status().and_then(|status| {
                                let most = status
                                    .pulled
                                    .iter()
                                    .find(|model| model.active)?
                                    .context_most?;
                                Some(format!(
                                    " {} was made for up to {} tokens.",
                                    status.local_tag,
                                    thousands(most)
                                ))
                            })}
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
                        save_cloud(body);
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
                                move || busy.get() || checking.get() || settled(),
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
                                disabled=move || settled() || checking.get()
                                prop:value=move || base_url.get()
                                on:input=move |event| base_url.set(event_target_value(&event))
                            />
                        </label>
                        <label>
                            <span>"MODEL"</span>
                            <input
                                type="text"
                                disabled=move || settled() || checking.get()
                                prop:value=move || model.get()
                                on:input=move |event| model.set(event_target_value(&event))
                            />
                        </label>
                        <label>
                            <span>"API KEY"</span>
                            <input
                                type="password"
                                autocomplete="off"
                                disabled=move || settled() || checking.get()
                                placeholder=move || {
                                    if settled() {
                                        "•••••••••••••••• saved, and hidden"
                                    } else if has_key() {
                                        "Saved. Leave blank to keep it."
                                    } else {
                                        ""
                                    }
                                }
                                prop:value=move || api_key.get()
                                on:input=move |event| api_key.set(event_target_value(&event))
                            />
                        </label>
                        <p class="muted small">
                            "A cloud answer leaves the house: what you ask goes to the provider, \
                             with the names and states of your devices, your automations, \
                             the floorplan, Irori's settings and lines of its log. The key stays on this \
                             machine and is never shown again."
                        </p>
                        {move || status().filter(|status| status.mode == "cloud").map(|status| {
                            view! { <p class="assistant-detail" class:ok=status.ready>{status.detail}</p> }
                        })}
                        // What came of asking the model: on its way, there, or why not.
                        {move || if checking.get() {
                            view! {
                                <p class="model-check asking" aria-live="polite">
                                    <span class="dots"><i></i><i></i><i></i></span>
                                    "Saved. Asking " {model.get()} " whether it answers"
                                </p>
                            }.into_any()
                        } else {
                            match answered.get() {
                                Some(Ok(())) => view! {
                                    <p class="model-check there" aria-live="polite">
                                        <svg viewBox="0 0 24 24" aria-hidden="true">
                                            <circle cx="12" cy="12" r="9.5" pathLength="1" />
                                            <path d="M7.5 12.5l3 3 6-6.5" pathLength="1" />
                                        </svg>
                                        <span>
                                            <b>{model.get()}</b>
                                            " answered. The key is right, and it's kept on this \
                                             machine."
                                        </span>
                                    </p>
                                }.into_any(),
                                Some(Err(why)) => view! {
                                    <p class="model-check not-there" role="alert">
                                        <span>
                                            "Saved, but " <b>{model.get()}</b> " didn't answer: "
                                            {why}
                                        </span>
                                    </p>
                                }.into_any(),
                                None => ().into_any(),
                            }
                        }}
                        <div class="assistant-actions end">
                            {move || has_key().then(|| view! {
                                <button type="button" class="quiet-button danger"
                                    disabled=move || busy.get() || checking.get()
                                    on:click=move |_| {
                                        // The key going takes the cloud model with it. A local
                                        // model that is in use stays in use.
                                        let mut body = serde_json::json!({ "api_key": "" });
                                        if status().is_some_and(|status| status.mode == "cloud") {
                                            body["mode"] = "off".into();
                                        }
                                        answered.set(None);
                                        changing.set(false);
                                        save(body);
                                    }>
                                    "Remove credentials"
                                </button>
                            })}
                            {move || if settled() {
                                view! {
                                    <button type="button" class="quiet-button"
                                        disabled=move || checking.get()
                                        on:click=move |_| check()>
                                        "Check again"
                                    </button>
                                    <button type="button" class="quiet-button"
                                        disabled=move || checking.get()
                                        on:click=move |_| {
                                            answered.set(None);
                                            changing.set(true);
                                        }>
                                        "Change"
                                    </button>
                                }.into_any()
                            } else {
                                view! {
                                    // Backing out of a change puts the fields back as Irori
                                    // has them.
                                    {move || (changing.get() && has_key()
                                        && status().is_some_and(|status| status.mode == "cloud"))
                                        .then(|| view! {
                                            <button type="button" class="quiet-button"
                                                disabled=move || busy.get() || checking.get()
                                                on:click=move |_| {
                                                    if let Some(status) = status() {
                                                        apply(&status);
                                                    }
                                                    api_key.set(String::new());
                                                    answered.set(None);
                                                    changing.set(false);
                                                }>
                                                "Cancel"
                                            </button>
                                        })}
                                    <button type="submit" class="primary"
                                        disabled=move || busy.get() || checking.get()>
                                        {move || if busy.get() {
                                            "Saving…"
                                        } else if checking.get() {
                                            "Checking…"
                                        } else {
                                            "Save"
                                        }}
                                    </button>
                                }.into_any()
                            }}
                        </div>
                    </form>
                }
                .into_any(),
            }}
            // Whichever model answers, and before one does: what it is told to keep to.
            <form class="assistant-instructions about-form" on:submit=move |event: ev::SubmitEvent| {
                event.prevent_default();
                save_instructions();
            }>
                <label>
                    <span>"INSTRUCTIONS"</span>
                    <textarea
                        rows="4"
                        placeholder="Answer in Spanish. Call the living room “the lounge”. One sentence unless I ask for more."
                        maxlength=move || status().map(|status| status.instructions_max.to_string())
                        prop:value=move || instructions.get()
                        on:input=move |event| instructions.set(event_target_value(&event))
                    ></textarea>
                </label>
                <p class="muted small">
                    "Read by the assistant before every question, in every chat: how to answer, \
                     what to call things, what you care about. It still only reads the home; \
                     nothing here lets it switch anything. For a model on this machine these \
                     words come out of its context."
                </p>
                <div class="assistant-actions end">
                    <span class="muted small">
                        {move || {
                            let most = status().map_or(0, |status| status.instructions_max);
                            format!("{} / {most}", instructions.with(|text| text.trim().chars().count()))
                        }}
                    </span>
                    <button type="submit" class="primary"
                        disabled=move || busy.get() || !instructions_changed()>
                        "Save instructions"
                    </button>
                </div>
            </form>
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

/// How much of its context a conversation has taken, as the chat's heading shows it: the
/// words, and the share of the whole where the whole is known.
fn context_words(used: api::AssistantContext) -> (String, Option<f64>) {
    match used.size.filter(|size| *size > 0) {
        Some(size) => {
            #[allow(clippy::cast_precision_loss)]
            let part = (used.used as f64 / size as f64 * 100.0).min(100.0);
            (format!("{part:.0}%"), Some(part))
        }
        None => (format!("{} tokens", thousands(used.used)), None),
    }
}

/// The same in a sentence, for the pointer resting on it.
fn context_title(used: api::AssistantContext) -> String {
    match used.size.filter(|size| *size > 0) {
        Some(size) => format!(
            "The last answer took {} of the model's {} tokens of context. Nearer the end, \
             older messages are left out; Clear starts again.",
            thousands(used.used),
            thousands(size)
        ),
        None => format!(
            "The last answer took {} tokens. How much a cloud model can hold is its \
             provider's to say, so there is no share to show.",
            thousands(used.used)
        ),
    }
}

/// A count of tokens, short: `812`, `1.5k`, `12k`.
fn thousands(n: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let k = n as f64 / 1000.0;
    if n < 1000 {
        n.to_string()
    } else if k < 10.0 {
        format!("{k:.1}k")
    } else {
        format!("{k:.0}k")
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
    fn a_chat_says_what_share_of_the_context_it_took_where_the_whole_is_known() {
        let local = api::AssistantContext {
            used: 1536,
            size: Some(4096),
        };
        assert_eq!(context_words(local), ("38%".to_owned(), Some(37.5)));
        assert!(context_title(local).contains("1.5k of the model's 4.1k tokens"));
        // More than the whole is still only all of it.
        let over = api::AssistantContext {
            used: 5000,
            size: Some(4096),
        };
        assert_eq!(context_words(over).0, "100%");
        let cloud = api::AssistantContext {
            used: 12_400,
            size: None,
        };
        assert_eq!(context_words(cloud), ("12k tokens".to_owned(), None));
        assert_eq!(thousands(812), "812");
    }

    #[test]
    fn the_card_opens_on_the_model_in_use_and_on_this_machine_when_none_is() {
        assert_eq!(first_side("cloud"), Side::Cloud);
        assert_eq!(first_side("local"), Side::Local);
        assert_eq!(first_side("off"), Side::Local);
    }
}
