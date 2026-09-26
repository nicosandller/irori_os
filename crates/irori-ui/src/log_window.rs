//! What a piece of Irori has said lately, in a window of its own.
//!
//! Two sources, one window. An extension's own output is where a failure a person can see is
//! usually written down (`docs/specs/extensions.md` §8); Irori's own log is where the words
//! Irori writes itself end up. The same window for both, so a person chasing a problem reads one
//! thing the same way whichever of the two said it.

use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use crate::lib::{CopyFeedback, CopyFeedback};

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

    // Helper to detect and format log line elements using simple string patterns
    fn format_log_line(line: String) -> Option<String> {
        let upper = line.to_uppercase();
        
        // Priority 1: ERROR messages (word boundary check)
        if find_word_boundary(&upper, "ERROR").is_some() {
            return Some(format!(
                "<span class=\"log-error\">{}</span>",
                escape_html(&line)
            ));
        }

        // Priority 2: WARNING/WARN messages (word boundary check)  
        if find_word_boundary(&upper, "WARNING").is_some() {
            return Some(format!(
                "<span class=\"log-warn\">{}</span>",
                escape_html(&line)
            ));
        }

        // Priority 3: Timestamps at start of line (HH:MM:SS.ddd or HH:MM:SS)
        if let Some(ts_pos) = find_timestamp_start(&line) {
            return Some(format!(
                "<span class=\"log-time\">[{}]</span><span class=\"log-info\">{}</span>",
                escape_html(&line[ts_pos..]),
                escape_html(&line[(ts_pos + 12)..])
            ));
        }

        // Priority 4: Windows paths (C:\ or C:/ at start of line, not just bare drive letter)
        if let Some(pos) = find_path_start(&line) {
            if pos > 0 || line[pos] != ':' {
                let rest = &line[pos..];
                
                return Some(format!(
                    "<span class=\"log-path\">{}</span>{rest}",
                    escape_html(rest.trim_start_matches(['\\', '/', ' ', '\t']))
                ));
            }
        }

        None
    }

    // Find position where a word pattern starts (word boundary check)
    fn find_word_boundary(text: &str, pattern: &str) -> Option<usize> {
        text.find(pattern).and_then(|pos| {
            if pos == 0 || text[pos - 1].is_whitespace() {
                if pos + pattern.len() >= text.len() 
                    || text[pos + pattern.len()] == '\n' 
                    || text[pos + pattern.len()] == ' ' 
                    || !text[pos + pattern.len()].is_alphabetic() {
                    Some(pos)
                } else {
                    None
                }
            } else {
                None
            }
        })
    }

    // Find timestamp start (HH:MM:SS.ddd or HH:MM:SS at start of line)
    fn find_timestamp_start(line: &str) -> Option<usize> {
        if line.len() < 12 { return None; }
        
        // Check first 13 chars for HH:MM:SS pattern
        let first_chars: Vec<char> = line.chars().take(13).collect();
        
        // Need at least "HH:MM:" (6 chars) plus either SS or SS.something (2+ chars)
        if first_chars.len() < 8 { return None; }
        
        // Check for colons at positions 2 and 5
        if first_chars[2] != ':' || first_chars[5] != ':' { return None; }
        
        // Check HH are digits
        let hh = first_chars[0..2].collect::<String>();
        if !hh.chars().all(|c| c.is_ascii_digit()) { return None; }
        
        // Check MM are digits
        let mm = first_chars[3..5].collect::<String>();
        if !mm.chars().all(|c| c.is_ascii_digit()) { return None; }
        
        // Check SS are digits
        let ss = first_chars[6..8].collect::<String>();
        if !ss.chars().all(|c| c.is_ascii_digit()) { return None; }
        
        // Find where the timestamp ends (either at position 8, or with optional .ddd)
        let mut ts_end = 8;
        
        if ts_end < first_chars.len() && first_chars[ts_end] == '.' {
            // Check for milliseconds (.ddd format)
            ts_end += 1; // skip the dot
            if ts_end + 2 < first_chars.len() {
                let ms = &first_chars[ts_end..ts_end+3].collect::<String>();
                if ms.chars().all(|c| c.is_ascii_digit()) && ms.len() == 3 {
                    ts_end += 3; // include milliseconds
                }
            }
        }
        
        Some(0) // Return 0 since timestamp starts at beginning of line
    }

    // Find Windows path start (first \ or / character)
    fn find_path_start(line: &str) -> Option<usize> {
        line.chars().enumerate().find(|(_, c)| matches!(c, '\\' | '/')).map(|(pos, _)| pos)
    }

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

    // Copy button feedback state
    let copy_feedback: CopyFeedback = RwSignal::new(CopyFeedback::none());
    let copy_btn = move |_| {
        let lines = lines.read();
        let text = lines.join("\n");
        
        leptos_use_navigator::navigator()
            .map(|nav| {
                let _ = navigator::clipboard::write(&text);
                copy_feedback.set(Some("Copied!".to_string()));
                set_timeout(Duration::from_millis(1500), || {
                    copy_feedback.set(None);
                });
            })
            .unwrap_or_else(|_| {
                spawn_local(async move {
                    let _ = navigator::clipboard::write(&text);
                    copy_feedback.set(Some("Copied!".to_string()));
                    set_timeout(Duration::from_millis(1500), || {
                        copy_feedback.set(None);
                    });
                })
            });

        view! {}
    };

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
                        .then(|| view! {
                            <p class="muted">
                                {quiet}
                            </p>
                        })
                        .into_any()
                } else {
                    view! {
                        <pre class="ext-log">
                            <button class="copy-btn" onclick=move |_| copy_btn() title="Copy all text">📋</button>
                            <span class="copy-feedback">{ move || copy_feedback.get().as_deref().unwrap_or_default() }</span>
                            {said.iter().map(format_log_line).collect::<Vec<_>>().into_view()}
                        </pre>
                    }
                        .into_any()
                }
            }}
        </crate::modal::Modal>
    }
}

/// Escape HTML special characters to prevent XSS when displaying log content
fn escape_html(s: &str) -> String {
    s.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace("\"", "&quot;")
        .replace("'", "&#x27;")
}
