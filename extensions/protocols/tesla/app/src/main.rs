//! The Tesla usage page: what Tesla's Fleet API has cost this month, where the month is heading,
//! where the money went, and how often Irori reads. Runs inside Irori's shell, in a sandboxed
//! frame, and asks the Tesla extension through the bridge (irori-ui-kit).
//!
//! The extension does the sums and the wording (`src/page.rs` there); this page lays them out.

mod api;
mod calls;
mod chart;
mod controls;
mod month;
mod time;

use std::cell::OnceCell;
use std::time::Duration;

use irori_ui_kit::page::{Bridge, Event, apply_theme};
use leptos::prelude::*;
use leptos::task::spawn_local;

thread_local! {
    static BRIDGE: OnceCell<Bridge> = const { OnceCell::new() };
}

/// The page's line to the shell.
pub fn bridge() -> Bridge {
    BRIDGE.with(|cell| {
        cell.get_or_init(|| {
            Bridge::connect(|event| {
                if let Event::Theme(theme) = event {
                    apply_theme(&theme);
                }
            })
        })
        .clone()
    })
}

/// How often the page asks again while it's open. Reading is minutes apart, so a minute is
/// plenty, and the asking itself costs nothing at Tesla.
const REFRESH: Duration = Duration::from_secs(60);

/// What the page knows, shared by every part of it.
#[derive(Debug, Clone, Copy)]
pub struct Usage {
    pub summary: RwSignal<Option<api::Summary>>,
    pub days: RwSignal<Option<api::Days>>,
    pub calls: RwSignal<Vec<api::Call>>,
    /// Why the last ask failed, if it did.
    pub trouble: RwSignal<Option<String>>,
}

impl Usage {
    /// Asks for everything again.
    pub fn reload(self) {
        spawn_local(async move {
            match api::summary().await {
                Ok(summary) => {
                    self.summary.set(Some(summary));
                    self.trouble.set(None);
                }
                Err(why) => {
                    self.trouble.set(Some(why));
                    return;
                }
            }
            if let Ok(days) = api::days().await {
                self.days.set(Some(days));
            }
            if let Ok(calls) = api::calls().await {
                self.calls.set(calls);
            }
        });
    }

    /// A change was saved: show the month as the extension now sees it, and the rest after.
    pub fn changed(self, summary: api::Summary) {
        self.summary.set(Some(summary));
        self.reload();
    }
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        console_error_panic_hook::hook(info);
        // Tell the shell too: the frame's own console is out of sight.
        if let Ok(Some(parent)) = window().parent() {
            let message =
                serde_json::json!({ "irori": 1, "id": 0, "op": "log", "args": info.to_string() });
            let _ = parent.post_message(&message.to_string().into(), "*");
        }
        if let Some(body) = document().body()
            && let Ok(note) = document().create_element("div")
        {
            let _ = note.set_attribute("class", "panic");
            note.set_text_content(Some(&format!(
                "Something went wrong on this page; reload it to carry on. ({info})"
            )));
            let _ = body.append_child(&note);
        }
    }));
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    let usage = Usage {
        summary: RwSignal::new(None),
        days: RwSignal::new(None),
        calls: RwSignal::new(Vec::new()),
        trouble: RwSignal::new(None),
    };
    provide_context(usage);

    spawn_local(async move {
        if let Ok(hello) = bridge().hello().await {
            apply_theme(&hello.theme);
        }
        usage.reload();
        loop {
            gloo_timers::future::sleep(REFRESH).await;
            usage.reload();
        }
    });

    view! {
        <main class="page">
            <header class="top">
                <h1>"Tesla usage"</h1>
                <p class="muted sub">{move || usage.summary.with(|summary| {
                    summary.as_ref().map(|summary| format!(
                        "{} · Tesla bills by calendar month, in UTC.",
                        time::month_name(&summary.month)
                    ))
                })}</p>
            </header>
            {move || usage.trouble.get().map(|why| view! {
                <p class="note error" role="alert">
                    "The Tesla extension didn't answer: " {why}
                </p>
            })}
            {move || {
                if usage.summary.with(Option::is_none) {
                    if usage.trouble.with(Option::is_none) {
                        return view! { <p class="muted loading">"Asking the Tesla extension…"</p> }
                            .into_any();
                    }
                    return ().into_any();
                }
                view! {
                    <month::Month/>
                    <chart::Daily/>
                    <div class="pair">
                        <calls::Targets/>
                        <controls::Reading/>
                    </div>
                    <controls::Dashboard/>
                    <calls::Calls/>
                }
                .into_any()
            }}
        </main>
    }
}
