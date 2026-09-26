//! What a piece of Irori has said lately, in a window of its own.
//!
//! Two sources, one window. An extension's own output is where a failure a person can see is
//! usually written down (`docs/specs/extensions.md` §8); Irori's own log is where the words
//! Irori writes itself end up. The same window for both, so a person chasing a problem reads one
//! thing the same way whichever of the two said it.

use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen_futures::JsFuture;

/// How often an open log window asks for more. The same rhythm as the Devices page's own polling
/// — a crash loop writes a fresh round of output every few seconds, and a log that stopped
/// updating while you watched it would read as having gone quiet.
const REFRESH: Duration = Duration::from_secs(2);

/// How long the copy button says what happened before it goes back to offering to copy again.
/// Long enough to notice, short enough that it isn't still claiming to have copied something by
/// the time you press it again.
const COPIED_FOR: Duration = Duration::from_millis(1500);

/// Whose log a window shows.
#[derive(Clone, Debug, PartialEq)]
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

/// How seriously to take a line, which is what decides its colour.
///
/// Read off what a line says, not off where it came from: an extension's own output is whatever
/// it printed, and only its author knows which lines were trouble. Most of a log is [`Plain`],
/// and saying so is not the same as having nothing to show — every line is still shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Weight {
    /// Something failed, or said it did.
    Error,
    /// Something is off that hasn't failed yet.
    Warn,
    /// Everything else.
    Plain,
}

impl Weight {
    /// The class that gives a line of this weight its colour. The page's stylesheet owns what
    /// each one looks like; this only says which one a line is.
    fn class(self) -> &'static str {
        match self {
            Weight::Error => "log-error",
            Weight::Warn => "log-warn",
            Weight::Plain => "log-plain",
        }
    }
}

/// How seriously to read `line`, by what it says.
///
/// In priority order, so the most serious reading of a line is the one it gets: `ERROR`, then
/// `WARNING` or `WARN`, then everything else. A line that says both reads as the error it is —
/// `couldn't open /dev/ttyUSB0: ERROR, not a warning` is an error because of the word in it, and
/// a person scanning for red should not have to read the rest of it to know.
///
/// The word has to be a word: `ERRORS` and `NOERROR` are not this, and a path or a device name
/// that happens to contain one is not either. The check is on an uppercased copy, so it doesn't
/// matter how the line was cased.
fn weight(line: &str) -> Weight {
    let upper = line.to_uppercase();
    if has_word(&upper, "ERROR") {
        Weight::Error
    } else if has_word(&upper, "WARNING") || has_word(&upper, "WARN") {
        Weight::Warn
    } else {
        Weight::Plain
    }
}

/// Whether `text` holds `word` as a whole word — not glued to a letter on either side, so it
/// matches `ERROR:` and the end of a line, and not `ERRORS` or `/dev/NOERROR`.
fn has_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let joined_before = text[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_alphabetic);
        let joined_after = text[at + word.len()..]
            .chars()
            .next()
            .is_some_and(char::is_alphabetic);
        !joined_before && !joined_after
    })
}

/// The timestamp `line` opens with, and the message that follows it.
///
/// `tracing` writes `2026-09-26T10:00:00.123456Z  INFO irori is ready`, and the timestamp is the
/// same shape on every line — which is the point of taking it out and colouring it on its own: a
/// column of times reads as a column, and the times stop competing with the message for
/// attention.
///
/// A line without one is all message, with an empty timestamp: an extension's output is whatever
/// it printed, so most of these lines have no timestamp and none of them are dropped for it.
/// The space after the timestamp is `tracing`'s column padding rather than part of the message,
/// so it goes with the timestamp.
fn split_timestamp(line: &str) -> (&str, &str) {
    let Some((first, rest)) = line.split_once(|c: char| c.is_whitespace()) else {
        return ("", line);
    };
    if looks_like_timestamp(first) {
        (first, rest.trim_start())
    } else {
        ("", line)
    }
}

/// Whether `word` is a date and a clock rather than the first word of a message.
///
/// The shape `tracing` writes: `2026-09-26T10:00:00.123456Z`. Each part is checked as a shape
/// rather than as a whole date being valid — a log window has no business rejecting a timestamp
/// because the day is the 31st of February, and every line here is shown whatever it turns out
/// to be. What the check is for is not validity but telling a timestamp apart from a message:
/// `couldn't-open-the-door: no such thing` has a colon and dashes in it too.
fn looks_like_timestamp(word: &str) -> bool {
    // The `T` joining the date to the clock is what makes this a timestamp at all, and a
    // timestamp with no clock is not one.
    let Some((date, clock)) = word.split_once(['T', 't']) else {
        return false;
    };
    // `2026-09-26`: four digits, a dash, two, a dash, two.
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| !part.bytes().all(|b| b.is_ascii_digit()))
        || parts[0].len() != 4
        || parts[1].len() != 2
        || parts[2].len() != 2
    {
        return false;
    }
    // A UTC clock ends in `Z`; an offset one carries `+02:00` or `-05:00` after it. Which zone a
    // line was written in isn't this's call, so both are checked as shapes and the offset is
    // simply the tail either way.
    let clock = clock.trim_end_matches(['Z', 'z']);
    let (clock, zone) = match clock.rfind(['+', '-']) {
        // Only an offset if it's where an offset goes: straight after `HH:MM:SS`. A `-` anywhere
        // else is part of the message this word was taken from, not a zone.
        Some(at) if at >= 8 && clock.len() - at == 6 => clock.split_at(at),
        Some(_) => return false,
        None => (clock, ""),
    };
    // The fraction of a second, which `tracing` writes between the clock and the zone.
    let clock = clock.split_once('.').map_or(clock, |(clock, _)| clock);
    let clock: Vec<&str> = clock.split(':').collect();
    clock.len() == 3
        && clock
            .iter()
            .all(|part| part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_digit()))
        // The offset itself: a sign and `HH:MM`, or nothing for UTC.
        && (zone.is_empty() || zone[1..].split(':').count() == 2
            && zone[1..].bytes().all(|byte| byte.is_ascii_digit() || byte == b':')
            && zone.len() == 6)
}

/// Whether the browser's clipboard took `text`.
///
/// The async clipboard API: the synchronous way to copy (a hidden textarea and `execCommand`) is
/// deprecated, and only works while the page has focus. The returned promise is what says whether
/// the write was actually allowed — it rejects without a user gesture, without permission, or
/// outside a secure context — so it is awaited rather than fired and forgotten. Reporting
/// "Copied" for a copy that didn't happen is the one thing a copy button must not do.
async fn write_to_clipboard(text: &str) -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    JsFuture::from(window.navigator().clipboard().write_text(text))
        .await
        .is_ok()
}

#[component]
pub fn LogWindow(source: Source, #[prop(into)] on_close: Callback<()>) -> impl IntoView {
    let lines = RwSignal::new(Vec::<String>::new());
    let trouble = RwSignal::new(None::<String>);
    // Distinguishes "nothing to show" from "haven't looked yet": an empty log is a real answer
    // and shouldn't flash an explanation before the first response arrives.
    let asked = RwSignal::new(false);
    let (title, note, quiet) = source.say();

    // What the copy button last did, or `None` when it isn't saying anything. A signal holding a
    // word for a person rather than a type of its own: there is nothing else to know about it.
    let copied: RwSignal<Option<&'static str>> = RwSignal::new(None);

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

    // The whole log at once, which is what someone reading a window of it wants to paste into an
    // issue: a single line out of a stack trace is rarely the useful part. Formatting stays in
    // the view, so what is copied is the text as it was written and not as it is coloured.
    let copy = move || {
        let text = lines.read().join("\n");
        spawn_local(async move {
            let said = if write_to_clipboard(&text).await {
                "Copied"
            } else {
                "Copy failed"
            };
            copied.set(Some(said));
            set_timeout(move || copied.set(None), COPIED_FOR);
        });
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
                    // One element per line, coloured by what it says, with its timestamp set
                    // apart from the message. The line is a real element holding the real text:
                    // nothing here is built as HTML for the browser to parse, so a line carrying
                    // someone else's `<script>` is shown as the words `<script>`, which is what
                    // it is. Every line is shown, including the ones that match nothing — a log
                    // window that quietly dropped the lines it didn't recognise would be worse
                    // than no window at all.
                    let rendered = said
                        .iter()
                        .map(|line| {
                            let (stamp, message) = split_timestamp(line);
                            let class = weight(line).class();
                            view! {
                                <span class=class>
                                    <span class="log-stamp">{stamp}</span>
                                    {message}
                                </span>
                            }
                            .into_any()
                        })
                        .collect::<Vec<_>>();
                    view! {
                        <div class="log-window">
                            <button
                                type="button"
                                class="log-copy"
                                on:click=move |_| copy()
                                title="Copy the whole log"
                            >
                                {move || copied.get().unwrap_or("Copy")}
                            </button>
                            <pre class="ext-log">{rendered}</pre>
                        </div>
                    }
                        .into_any()
                }
            }}
        </crate::modal::Modal>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_reads_as_an_error() {
        assert_eq!(weight("couldn't open it: ERROR"), Weight::Error);
    }

    #[test]
    fn the_most_serious_reading_of_a_line_is_the_one_it_gets() {
        // Both words, and the error is the one a person scanning for trouble needs to see.
        assert_eq!(weight("ERROR, not a WARNING"), Weight::Error);
    }

    #[test]
    fn a_warning_reads_as_a_warning_under_either_spelling() {
        assert_eq!(
            weight("listening beyond this machine: WARNING"),
            Weight::Warn
        );
        assert_eq!(weight("WARN deprecated option"), Weight::Warn);
    }

    #[test]
    fn the_word_has_to_be_a_word() {
        // A device or path that happens to contain one of these is not a line that said it.
        assert_eq!(weight("/dev/NOERROR/tty"), Weight::Plain);
        assert_eq!(weight("2 ERRORS were dropped"), Weight::Plain);
        assert_eq!(weight("some WARNINGLY worded message"), Weight::Plain);
    }

    #[test]
    fn the_case_a_line_was_written_in_does_not_matter() {
        assert_eq!(weight("error: it broke"), Weight::Error);
        assert_eq!(weight("Warning: it is odd"), Weight::Warn);
    }

    #[test]
    fn most_lines_are_only_plain() {
        assert_eq!(
            weight("2026-09-26T10:00:00Z  INFO irori is ready"),
            Weight::Plain
        );
        assert_eq!(weight(""), Weight::Plain);
    }

    #[test]
    fn each_weight_has_its_own_class_for_the_stylesheet() {
        assert_eq!(Weight::Error.class(), "log-error");
        assert_eq!(Weight::Warn.class(), "log-warn");
        assert_eq!(Weight::Plain.class(), "log-plain");
    }

    #[test]
    fn a_timestamp_is_taken_off_the_front_of_the_message() {
        // What `tracing` writes, with the column padding after the timestamp.
        let (stamp, message) = split_timestamp("2026-09-26T10:00:00.123456Z  INFO irori is ready");
        assert_eq!(stamp, "2026-09-26T10:00:00.123456Z");
        assert_eq!(message, "INFO irori is ready");
    }

    #[test]
    fn a_line_without_a_timestamp_is_all_message() {
        // An extension's own output is whatever it printed, so most lines look like this.
        let (stamp, message) = split_timestamp("ser: opening /dev/ttyUSB0");
        assert_eq!(stamp, "");
        assert_eq!(message, "ser: opening /dev/ttyUSB0");
    }

    #[test]
    fn a_first_word_is_not_mistaken_for_a_timestamp() {
        // Long, and it has both a colon and a dash — but it is a message, and this is a log
        // window rather than a guess at a format.
        let (stamp, message) = split_timestamp("couldn't-open-the-door: no such thing");
        assert_eq!(stamp, "");
        assert_eq!(message, "couldn't-open-the-door: no such thing");
    }

    #[test]
    fn the_shapes_tracing_actually_writes_are_taken_off_the_front() {
        // The three forms it writes, so none of them is left to be read as part of a message.
        for line in [
            "2026-09-26T10:00:00Z  INFO irori is ready",
            "2026-09-26T10:00:00.123456Z  INFO irori is ready",
            "2026-09-26T10:00:00+02:00  INFO irori is ready",
        ] {
            let (stamp, message) = split_timestamp(line);
            assert!(stamp.starts_with("2026-09-26T10:00:00"), "{stamp}");
            assert_eq!(message, "INFO irori is ready");
        }
    }

    #[test]
    fn a_line_of_one_word_keeps_itself() {
        let (stamp, message) = split_timestamp("ready");
        assert_eq!(stamp, "");
        assert_eq!(message, "ready");
    }
}
