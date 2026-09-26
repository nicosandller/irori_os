//! What a piece of Irori has said lately, in a window of its own.
//!
//! Two sources, one window. An extension's own output is where a failure a person can see is
//! usually written down (`docs/specs/extensions.md` §8); Irori's own log is where the words
//! Irori writes itself end up. The same window for both, so a person chasing a problem reads one
//! thing the same way whichever of the two said it.

use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;

/// How often an open log window asks for more. The same rhythm as the Devices page's own polling
/// — a crash loop writes a fresh round of output every few seconds, and a log that stopped
/// updating while you watched it would read as having gone quiet.
const REFRESH: Duration = Duration::from_secs(2);

/// Whose log a window shows.
#[derive(Clone, PartialEq)]
pub enum Source {
    /// One extension's own output: the tail of the process it runs as.
    Extension(String),
    /// Irori's own log: the tail of what this instance has written since it started.
    System,
}

impl Source {
    async fn lines(&self) -> Result<Vec<String>, String> {
        match self {
            Source::Extension(id) => crate::api::fetch_extension_log(id).await,
            Source::System => crate::api::fetch_system_log().await,
        }
    }

    /// What the window calls itself, what it says about what it is showing, and what it says when
    /// there is nothing to show. All three differ between the sources because the truth does: an
    /// extension's lines are its own process's output, while these are Irori's — which also
    /// carries each extension's, tagged with the extension it came from.
    fn say(&self) -> (String, &'static str, &'static str) {
        match self {
            Source::Extension(id) => (
                format!("{id} log"),
                "What this extension has written to its own output, oldest first. Irori keeps \
                 the most recent lines only, and starts again each time the extension is \
                 reinstalled.",
                "Nothing yet. A built-in extension has no output of its own, and one that \
                 hasn't started has nothing to say.",
            ),
            Source::System => (
                "Irori log".to_owned(),
                "What Irori has said since it started, oldest first — which includes the lines \
                 an extension's own output arrived as, tagged with the extension they came from. \
                 Irori keeps the most recent lines only, and this log starts again whenever \
                 Irori does. The whole log also goes wherever Irori was started from: the \
                 terminal, or the service log of a container or a systemd unit.",
                "Nothing yet. Irori shows what it has logged, and how much there is depends on \
                 the level set by --log-level or [server] log_level in irori.toml.",
            ),
        }
    }
}

#[component]
pub fn LogWindow(source: Source, #[prop(into)] on_close: Callback<()>) -> impl IntoView {
    let lines = RwSignal::new(Vec::<String>::new());
    let trouble = RwSignal::new(None::<String>);
    // Distinguishes "nothing to show" from "haven't looked yet": an empty log is a real answer
    // and shouldn't flash an explanation before the first response arrives.
    let asked = RwSignal::new(false);
    let (title, note, quiet) = source.say();

    let load = move || {
        let source = source.clone();
        spawn_local(async move {
            match source.lines().await {
                Ok(said) => {
                    lines.set(said);
                    trouble.set(None);
                }
                Err(why) => trouble.set(Some(why)),
            }
            asked.set(true);
        });
    };
    load();
    let handle = set_interval_with_handle(load, REFRESH).ok();
    on_cleanup(move || {
        if let Some(handle) = handle {
            handle.clear();
        }
    });

    view! {
        <crate::modal::Modal title=title on_close=on_close>
            <p class="muted small">
                {note}
            </p>
            {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}
            {move || {
                let said = lines.get();
                if said.is_empty() {
                    (asked.get())
                        .then(|| view! { <p class="muted">{quiet}</p> })
                        .into_any()
                } else {
                    view! {
                        <pre class="ext-log">
                            {said.join("\n")}
                        </pre>
                    }
                        .into_any()
                }
            }}
        </crate::modal::Modal>
    }
}
