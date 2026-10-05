//! What a piece of Irori has said lately, in a window of its own.
//!
//! Two sources, one window. An extension's own output is where a failure a person can see is
//! usually written down (`docs/specs/extensions.md` §8); Irori's own log is where the words
//! Irori writes itself end up. The same window for both, so a person chasing a problem reads one
//! thing the same way whichever of the two said it.
//!
//! Each line is read into its parts — when, how serious, who said it, what, and the details
//! after it — and shown as a row of its own, so a log reads down the page rather than across it.
//! Nothing is dropped for not fitting that shape: a line that's only words is a row that's only a
//! message.

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
    /// The local model's: what the Ollama Irori installed has written since it last started.
    Model,
}

impl Source {
    async fn lines(&self) -> Result<Vec<String>, String> {
        match self {
            Source::Extension(id) => crate::api::fetch_extension_log(id).await,
            Source::System => crate::api::fetch_system_log().await,
            Source::Model => crate::api::fetch_model_log().await,
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
            Source::Model => (
                "Model log".to_owned(),
                "What the Ollama on this machine has said since it last started, oldest first: \
                 a model loading, how much memory it asked for, and why it stopped if it did. \
                 The whole of it is ollama.log in Irori's data directory.",
                "Nothing yet. Ollama writes here once it has started and been asked for a model.",
            ),
        }
    }
}

/// The level a line says it was written at, where it says one — `tracing`'s five words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    fn read(word: &str) -> Option<Level> {
        Some(match word {
            "ERROR" => Level::Error,
            "WARN" => Level::Warn,
            "INFO" => Level::Info,
            "DEBUG" => Level::Debug,
            "TRACE" => Level::Trace,
            _ => return None,
        })
    }

    fn word(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        }
    }
}

/// One line of a log, in its parts.
#[derive(Clone, Debug, PartialEq)]
struct Line {
    /// The whole line as it was written, for the copy button and the search box.
    raw: String,
    /// When, as written: `2026-09-26T10:00:00.123456Z`. Empty for a line that doesn't say.
    stamp: String,
    /// The level it was written at, if it says — or, if it doesn't, what the words in it amount
    /// to: a line that says ERROR is an error whoever printed it.
    level: Option<Level>,
    /// The extension a line in Irori's own log came from (`extension=esphome`).
    source: Option<String>,
    /// What it said.
    message: String,
    /// The `key=value` details `tracing` writes after the message, in order.
    fields: Vec<(String, String)>,
}

impl Line {
    /// How seriously to take it, for the colour and the filters.
    fn weight(&self) -> Weight {
        match self.level {
            Some(Level::Error) => Weight::Error,
            Some(Level::Warn) => Weight::Warn,
            _ => Weight::Plain,
        }
    }

    /// `HH:MM:SS` out of the stamp: the part that tells one line from the next.
    fn clock(&self) -> &str {
        self.stamp.get(11..19).unwrap_or(&self.stamp)
    }
}

/// Reads a line into its parts. Never fails: what can't be read as a part stays in the message.
///
/// Irori's own lines are `tracing`'s: `2026-…Z  WARN connected device=… extension=esphome`. An
/// extension's own output often is too, and an older Irori passed an extension's line on whole,
/// as an INFO line wrapped round the extension's own timestamp and level — so a second stamp and
/// level straight after the first are read as well, and the more serious of the two levels wins:
/// an extension's ERROR is an error, whatever the line round it said.
fn parse(raw: &str) -> Line {
    let clean = without_escaped_colour(raw);
    let (stamp, rest) = split_timestamp(&clean);
    let (mut level, mut rest) = take_level(rest);
    // The wrapped line's own stamp and level, when there are any.
    let (inner_stamp, inner) = split_timestamp(rest);
    let (inner_level, inner_rest) = take_level(inner);
    if inner_level.is_some() || !inner_stamp.is_empty() {
        level = level.max(inner_level);
        rest = inner_rest;
    }
    let (message, mut fields) = split_fields(rest);
    let source = fields
        .iter()
        .position(|(key, _)| key == "extension")
        .map(|at| fields.remove(at).1);
    // What the words say counts too: an extension that prints `error: failed to bind` without
    // a level of its own arrives in Irori's log as INFO, and it's still an error.
    let said = match weight(message) {
        Weight::Error => Some(Level::Error),
        Weight::Warn => Some(Level::Warn),
        Weight::Plain => None,
    };
    let level = match (level, said) {
        (Some(level), Some(said)) => Some(level.max(said)),
        (level, said) => level.or(said),
    };
    Line {
        raw: raw.to_owned(),
        stamp: stamp.to_owned(),
        level,
        source,
        message: message.to_owned(),
        fields,
    }
}

/// A level word off the front of `text`, and what follows it.
fn take_level(text: &str) -> (Option<Level>, &str) {
    let text = text.trim_start();
    match text.split_once(char::is_whitespace) {
        Some((word, rest)) => match Level::read(word) {
            Some(level) => (Some(level), rest.trim_start()),
            None => (None, text),
        },
        None => match Level::read(text) {
            Some(level) => (Some(level), ""),
            None => (None, text),
        },
    }
}

/// The message, and the `key=value` details after it.
///
/// `tracing` writes its fields last, each a word, `=`, and a value that runs until the next
/// field — a value may have spaces in it (`name=Sensor fusion radar`). So the message is what
/// comes before the first word shaped like `key=`, and each field runs to the next. A message
/// that happens to contain a `key=` word is split there too; everything is still shown, just
/// with the rest set back as details.
fn split_fields(text: &str) -> (&str, Vec<(String, String)>) {
    let mut starts = Vec::new();
    let mut at = 0;
    for word in text.split(' ') {
        if is_field(word) {
            starts.push(at);
        }
        at += word.len() + 1;
    }
    let Some(&first) = starts.first() else {
        return (text.trim_end(), Vec::new());
    };
    let message = text[..first].trim_end();
    let mut fields = Vec::new();
    for (i, &start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).map_or(text.len(), |&next| next);
        let field = text[start..end].trim_end();
        if let Some((key, value)) = field.split_once('=') {
            fields.push((key.to_owned(), value.to_owned()));
        }
    }
    (message, fields)
}

/// Whether `word` starts a field: an identifier, then `=`.
fn is_field(word: &str) -> bool {
    let Some((key, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.'))
}

/// A line without the colour codes an older Irori let through as text: `\x1b[2m`, written out
/// as the four characters it is, where the terminal escape used to be. Only that shape is taken
/// out — a backslash that's part of what a line said stays.
fn without_escaped_colour(line: &str) -> String {
    const ESCAPE: &str = "\\x1b[";
    if !line.contains(ESCAPE) {
        return line.to_owned();
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find(ESCAPE) {
        out.push_str(&rest[..at]);
        let after = &rest[at + ESCAPE.len()..];
        let params = after
            .find(|c: char| !matches!(c, '0'..='9' | ';'))
            .unwrap_or(after.len());
        rest = match after[params..].chars().next() {
            Some(end) if end.is_ascii_alphabetic() => &after[params + 1..],
            _ => {
                out.push_str(ESCAPE);
                after
            }
        };
    }
    out.push_str(rest);
    out
}

/// How many lines fell off the front of `old` to make `new`: the smallest cut that leaves the
/// rest of `old` as the start of `new`. The server keeps only the newest lines, so once the log
/// is full every new line pushes one off the front — and a row keeps its place on the page only
/// if it keeps its key, which counts lines from the first one this window ever saw. All of `old`
/// when nothing lines up: Irori restarted, or more arrived than it keeps.
fn fell_off(old: &[String], new: &[String]) -> usize {
    (0..old.len())
        .find(|&cut| {
            let rest = &old[cut..];
            rest.len() <= new.len() && new[..rest.len()] == *rest
        })
        .unwrap_or(old.len())
}

/// Which lines a filter shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Showing {
    All,
    Warnings,
    Errors,
}

impl Showing {
    fn keeps(self, weight: Weight) -> bool {
        match self {
            Showing::All => true,
            Showing::Warnings => weight != Weight::Plain,
            Showing::Errors => weight == Weight::Error,
        }
    }
}

/// Whether `line` has `needle` in it, ignoring case. An empty needle is in every line.
fn mentions(line: &Line, needle: &str) -> bool {
    needle.is_empty() || line.raw.to_lowercase().contains(needle)
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

/// The copy button's picture: a clipboard, and the check it turns into once the copy worked.
fn copy_icon() -> impl IntoView {
    view! {
        <svg class="copy-icon" viewBox="0 0 24 24" aria-hidden="true" fill="none"
             stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
            <g class="copy-clip">
                <rect x="8" y="8" width="12" height="12" rx="2" />
                <path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2" />
            </g>
            <path class="copy-check" d="M5 12.5l4.5 4.5L19 7.5" pathLength="1" />
        </svg>
    }
}

#[component]
pub fn LogWindow(source: Source, #[prop(into)] on_close: Callback<()>) -> impl IntoView {
    // Every line so far, each with the key it keeps for as long as the window is open.
    let rows = RwSignal::new(Vec::<(usize, Line)>::new());
    // The key the next new line gets.
    let next = StoredValue::new(0usize);
    // Keys below this arrived with the first answer; the ones after it arrived while the window
    // was open, and those are the ones that fade in.
    let first_batch = StoredValue::new(None::<usize>);
    let trouble = RwSignal::new(None::<String>);
    // Distinguishes "nothing to show" from "haven't looked yet": an empty log is a real answer
    // and shouldn't flash an explanation before the first response arrives.
    let asked = RwSignal::new(false);
    let (title, note, quiet) = source.say();

    // The filters. Outside what redraws as lines arrive, so a search being typed survives a
    // refresh (ROADMAP D33).
    let showing = RwSignal::new(Showing::All);
    let needle = RwSignal::new(String::new());

    // What the copy button last did, or `None` when it isn't saying anything.
    let copied: RwSignal<Option<bool>> = RwSignal::new(None);

    // The scrolling log itself, to keep the newest line in view while you're reading it.
    let scroller = NodeRef::<leptos::html::Div>::new();
    // Whether the log is scrolled to its end: a new line then scrolls into view. Scrolled up to
    // read something, it stays where you are.
    let pinned = StoredValue::new(true);

    let load = move || {
        let source = source.clone();
        spawn_local(async move {
            match source.lines().await {
                Ok(said) => {
                    let old: Vec<String> = rows.with_untracked(|rows| {
                        rows.iter().map(|(_, line)| line.raw.clone()).collect()
                    });
                    // Nothing new, nothing to redraw.
                    if old != said {
                        let gone = fell_off(&old, &said);
                        let kept = old.len() - gone;
                        let start = next.get_value() - kept;
                        rows.update(|rows| {
                            rows.drain(..gone);
                            for (i, raw) in said.iter().enumerate().skip(kept) {
                                rows.push((start + i, parse(raw)));
                            }
                        });
                        next.set_value(start + said.len());
                        if first_batch.get_value().is_none() {
                            first_batch.set_value(Some(next.get_value()));
                        }
                        if pinned.get_value() {
                            // After the new rows are drawn.
                            request_animation_frame(move || {
                                if let Some(log) = scroller.get_untracked() {
                                    log.set_scroll_top(log.scroll_height());
                                }
                            });
                        }
                    }
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

    // The lines the filters let through.
    let shown = Memo::new(move |_| {
        let needle = needle.get().to_lowercase();
        let showing = showing.get();
        rows.with(|rows| {
            rows.iter()
                .filter(|(_, line)| showing.keeps(line.weight()) && mentions(line, &needle))
                .cloned()
                .collect::<Vec<_>>()
        })
    });
    let count = move |weight: Weight| {
        rows.with(|rows| {
            rows.iter()
                .filter(|(_, line)| line.weight() == weight)
                .count()
        })
    };

    // What's showing, as it was written — so a filtered log copies as the lines you picked out,
    // and a colour never ends up in an issue someone pastes this into.
    let copy = move || {
        let text = shown.with(|shown| {
            shown
                .iter()
                .map(|(_, line)| line.raw.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        });
        spawn_local(async move {
            copied.set(Some(write_to_clipboard(&text).await));
            set_timeout(move || copied.set(None), COPIED_FOR);
        });
    };

    let filter = move |to: Showing, label: &'static str, weight: Option<Weight>| {
        view! {
            <button
                type="button"
                class:chosen=move || showing.get() == to
                aria-pressed=move || (showing.get() == to).to_string()
                on:click=move |_| showing.set(to)
            >
                {label}
                {weight.map(|weight| view! {
                    <span class="log-count" class:none=move || count(weight) == 0>
                        {move || count(weight)}
                    </span>
                })}
            </button>
        }
    };

    view! {
        <crate::modal::Modal title=title on_close=on_close wide=true>
            <p class="muted small">{note}</p>
            {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}
            <div class="log-bar">
                <div class="switcher log-filter" role="group" aria-label="Which lines">
                    {filter(Showing::All, "All", None)}
                    {filter(Showing::Warnings, "Warnings", Some(Weight::Warn))}
                    {filter(Showing::Errors, "Errors", Some(Weight::Error))}
                </div>
                <input
                    class="log-search"
                    type="search"
                    placeholder="Search the log"
                    aria-label="Search the log"
                    prop:value=needle
                    on:input:target=move |ev| needle.set(ev.target().value())
                />
                <button
                    type="button"
                    class="log-copy"
                    class:done=move || copied.get() == Some(true)
                    class:failed=move || copied.get() == Some(false)
                    on:click=move |_| copy()
                    aria-label="Copy what's showing"
                    title=move || match copied.get() {
                        Some(true) => "Copied",
                        Some(false) => "Copy failed — the browser didn't allow it",
                        None => "Copy what's showing",
                    }
                >
                    {copy_icon()}
                </button>
                <span class="visually-hidden" aria-live="polite">
                    {move || match copied.get() {
                        Some(true) => "Copied",
                        Some(false) => "Copy failed",
                        None => "",
                    }}
                </span>
            </div>
            <div
                class="log-window"
                node_ref=scroller
                on:scroll=move |_| {
                    if let Some(log) = scroller.get_untracked() {
                        pinned.set_value(
                            log.scroll_top() + log.client_height() >= log.scroll_height() - 8,
                        );
                    }
                }
            >
                {move || {
                    let empty = rows.with(Vec::is_empty);
                    (empty && asked.get()).then(|| view! { <p class="muted log-quiet">{quiet}</p> })
                }}
                {move || {
                    let nothing = !rows.with(Vec::is_empty) && shown.with(Vec::is_empty);
                    nothing.then(|| view! { <p class="muted log-quiet">"No lines match."</p> })
                }}
                // One row per line, keyed so a row stays put as newer lines arrive and older ones
                // fall off the top. Every part is a real element holding the real text: nothing
                // here is built as HTML for the browser to parse, so a line carrying someone
                // else's `<script>` is shown as the words `<script>`, which is what it is.
                <ol class="log-lines">
                    <For
                        each=move || shown.get()
                        key=|(key, _)| *key
                        children=move |(key, line)| {
                            let fresh = first_batch.get_value().is_some_and(|first| key >= first);
                            let level = line.level.map(Level::word);
                            view! {
                                <li
                                    class=format!("log-row {}", line.weight().class())
                                    class:fresh=fresh
                                >
                                    <time class="log-stamp" title=line.stamp.clone()>
                                        {line.clock().to_owned()}
                                    </time>
                                    <span class=format!("log-level {}", level.unwrap_or("none"))>
                                        {level.unwrap_or("")}
                                    </span>
                                    <span class="log-text">
                                        {line.source.clone().map(|source| view! {
                                            <span class="log-source">{source}</span>
                                        })}
                                        <span class="log-message">{line.message.clone()}</span>
                                        {line
                                            .fields
                                            .iter()
                                            .map(|(key, value)| view! {
                                                <span class="log-field">
                                                    <span class="log-key">{format!("{key}=")}</span>
                                                    {value.clone()}
                                                </span>
                                            })
                                            .collect_view()}
                                    </span>
                                </li>
                            }
                        }
                    />
                </ol>
            </div>
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

    #[test]
    fn a_line_of_irori_s_own_reads_into_its_parts() {
        let line = parse(
            "2026-09-28T13:38:35.195523Z  WARN connected without authentication \
             device=30:83:98:CA:6A:08 name=Sensor fusion radar extension=esphome",
        );
        assert_eq!(line.stamp, "2026-09-28T13:38:35.195523Z");
        assert_eq!(line.clock(), "13:38:35");
        assert_eq!(line.level, Some(Level::Warn));
        assert_eq!(line.source.as_deref(), Some("esphome"));
        assert_eq!(line.message, "connected without authentication");
        assert_eq!(
            line.fields,
            [
                ("device".to_owned(), "30:83:98:CA:6A:08".to_owned()),
                ("name".to_owned(), "Sensor fusion radar".to_owned()),
            ]
        );
    }

    #[test]
    fn a_wrapped_extension_line_reads_as_its_own_level() {
        // How an older Irori passed an extension's line on: INFO round the extension's ERROR,
        // with the colour codes written out as text.
        let line = parse(
            "2026-09-28T13:38:35.124795Z  INFO \\x1b[2m2026-09-28T13:38:35.124696Z\\x1b[0m \
             \\x1b[31mERROR\\x1b[0m couldn't connect extension=esphome",
        );
        assert_eq!(line.level, Some(Level::Error));
        assert_eq!(line.message, "couldn't connect");
        assert_eq!(line.source.as_deref(), Some("esphome"));
        assert_eq!(line.weight(), Weight::Error);
    }

    #[test]
    fn an_error_in_the_words_of_an_info_line_is_an_error() {
        let line = parse("2026-09-28T13:38:35Z  INFO error: failed to bind port extension=zigbee");
        assert_eq!(line.level, Some(Level::Error));
        // A field that mentions it isn't the message saying it.
        let quiet = parse("2026-09-28T13:38:35Z  INFO connected errors=0");
        assert_eq!(quiet.level, Some(Level::Info));
    }

    #[test]
    fn a_line_that_is_only_words_is_all_message() {
        let line = parse("ser: opening /dev/ttyUSB0");
        assert_eq!(line.stamp, "");
        assert_eq!(line.level, None);
        assert_eq!(line.message, "ser: opening /dev/ttyUSB0");
        assert!(line.fields.is_empty());
        // And one that says it's trouble is trouble, whatever else it looks like.
        assert_eq!(parse("couldn't open it: ERROR").level, Some(Level::Error));
        assert_eq!(parse("").message, "");
    }

    #[test]
    fn an_equals_sign_in_a_message_is_not_a_field_unless_it_is_shaped_like_one() {
        let line = parse("2026-09-26T10:00:00Z  INFO 2 + 2 = 4 answer=four");
        assert_eq!(line.message, "2 + 2 = 4");
        assert_eq!(line.fields, [("answer".to_owned(), "four".to_owned())]);
        assert!(parse("https://example.com/?a=b").fields.is_empty());
    }

    #[test]
    fn a_backslash_that_is_not_a_colour_code_stays() {
        assert_eq!(
            without_escaped_colour(r"C:\x1b and \x1b[2mdim\x1b[0m"),
            r"C:\x1b and dim"
        );
    }

    #[test]
    fn lines_that_fell_off_the_front_are_counted() {
        let lines = |all: &[&str]| all.iter().map(|&s| s.to_owned()).collect::<Vec<_>>();
        assert_eq!(fell_off(&lines(&["a", "b"]), &lines(&["a", "b", "c"])), 0);
        assert_eq!(
            fell_off(&lines(&["a", "b", "c"]), &lines(&["b", "c", "d"])),
            1
        );
        assert_eq!(fell_off(&lines(&[]), &lines(&["a"])), 0);
        // Nothing lines up: a restart, or more than the server keeps.
        assert_eq!(fell_off(&lines(&["a", "b"]), &lines(&["x"])), 2);
        // A repeated line doesn't fool it.
        assert_eq!(
            fell_off(&lines(&["a", "a", "b"]), &lines(&["a", "b", "a"])),
            1
        );
    }

    #[test]
    fn the_filters_keep_what_they_say() {
        assert!(Showing::All.keeps(Weight::Plain));
        assert!(Showing::Warnings.keeps(Weight::Error));
        assert!(!Showing::Warnings.keeps(Weight::Plain));
        assert!(!Showing::Errors.keeps(Weight::Warn));
    }
}
