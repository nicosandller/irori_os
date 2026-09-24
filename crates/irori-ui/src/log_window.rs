//! What an extension has said for itself lately, in a window of its own.
//!
//! The guideline this exists to serve (`docs/specs/extensions.md` §8): a failure a person can see
//! should be one they can act on. A card's reason line carries the extension's last words, which
//! is usually enough; when it isn't, the rest of what it said is one press away rather than in a
//! terminal they may not have open.

use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;

/// How often an open log window asks for more. The same rhythm as the Devices page's own polling
/// — a crash loop writes a fresh round of output every few seconds, and a log that stopped
/// updating while you watched it would read as the extension having gone quiet.
const REFRESH: Duration = Duration::from_secs(2);

#[component]
pub fn LogWindow(id: String, #[prop(into)] on_close: Callback<()>) -> impl IntoView {
    let lines = RwSignal::new(Vec::<String>::new());
    let trouble = RwSignal::new(None::<String>);
    // Distinguishes "nothing to show" from "haven't looked yet": an empty log is a real answer
    // and shouldn't flash an explanation before the first response arrives.
    let asked = RwSignal::new(false);

    let load = {
        let id = id.clone();
        move || {
            let id = id.clone();
            spawn_local(async move {
                match crate::api::fetch_extension_log(&id).await {
                    Ok(said) => {
                        lines.set(said);
                        trouble.set(None);
                    }
                    Err(why) => trouble.set(Some(why)),
                }
                asked.set(true);
            });
        }
    };
    load();
    let handle = set_interval_with_handle(load, REFRESH).ok();
    on_cleanup(move || {
        if let Some(handle) = handle {
            handle.clear();
        }
    });

    view! {
        <crate::modal::Modal title=format!("{id} log") on_close=on_close>
            <p class="muted small">
                "What this extension has written to its own output, oldest first. Irori keeps the "
                "most recent lines only, and starts again each time the extension is reinstalled."
            </p>
            {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}
            {move || {
                let said = lines.get();
                if said.is_empty() {
                    (asked.get())
                        .then(|| view! {
                            <p class="muted">
                                "Nothing yet. A built-in extension has no output of its own, and "
                                "one that hasn't started has nothing to say."
                            </p>
                        })
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
