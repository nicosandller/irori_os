//! A word's last 24 hours, as a strip.
//!
//! For what has a state rather than a number — a player that is playing or paused, a door that
//! is open or closed. Time runs left to right; each block is one state, as wide as it lasted.
//! There is no value axis: the colour says which state, the width says for how long.
//!
//! A state keeps its colour whatever else happened that day: it comes from the state's place
//! among everything the entity can say, not from the order things turned up in. Colour is never
//! the only way to tell: the legend names each one, pointing at a block says what it was and
//! when, and the table is a click away (the device page's Timeline/Table switch).

use leptos::ev;
use leptos::prelude::*;
use web_sys::wasm_bindgen::JsCast;

/// One change: when it arrived, and what the state became. No label is a gap — the thing wasn't
/// there, or hadn't said — and the strip is left empty for it rather than filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub at_ms: f64,
    /// For the labels, in the same words the table uses.
    pub at: String,
    pub label: Option<String>,
}

/// How many states get a colour of their own. The palette has eight that stay apart from each
/// other; a ninth would have to be made up, and would look like one of them.
pub const COLOURS: usize = 8;

/// What a state is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// One of the palette's colours, from 0.
    Colour(usize),
    /// The state that means nothing is going on (off, closed): quiet, so the rest stands out.
    Off,
    /// More states than colours: the rest share one, and are still named when pointed at.
    Other,
}

impl Tone {
    fn class(self) -> String {
        match self {
            Self::Colour(slot) => format!("tone-{}", slot + 1),
            Self::Off => "tone-off".to_owned(),
            Self::Other => "tone-other".to_owned(),
        }
    }
}

/// One block of the strip: a stretch of time in one state.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// Where it starts and how wide it is, as percentages of the strip.
    pub left: f64,
    pub width: f64,
    pub label: Option<String>,
    pub from: String,
    /// When it ended; nothing if it's still going.
    pub to: Option<String>,
    pub lasted_ms: f64,
}

/// The day as blocks: each state from when it started to when the next one did, the last one
/// running on to now. The same state reported twice in a row is one block. The day starts at
/// the first thing that was said: gaps before it are dropped.
pub fn blocks(spans: &[Span], now_ms: f64) -> Vec<Block> {
    let spans = match spans.iter().position(|span| span.label.is_some()) {
        Some(first) => &spans[first..],
        None => return Vec::new(),
    };
    let start = spans[0].at_ms;
    let whole = (now_ms - start).max(1.0);
    let mut blocks: Vec<Block> = Vec::new();
    for (i, span) in spans.iter().enumerate() {
        let (until_ms, until) = match spans.get(i + 1) {
            Some(next) => (next.at_ms, Some(next.at.clone())),
            None => (now_ms.max(span.at_ms), None),
        };
        let lasted_ms = (until_ms - span.at_ms).max(0.0);
        match blocks.last_mut() {
            Some(last) if last.label == span.label => {
                last.width += lasted_ms / whole * 100.0;
                last.lasted_ms += lasted_ms;
                last.to = until;
            }
            _ => blocks.push(Block {
                left: ((span.at_ms - start) / whole * 100.0).clamp(0.0, 100.0),
                width: lasted_ms / whole * 100.0,
                label: span.label.clone(),
                from: span.at.clone(),
                to: until,
                lasted_ms,
            }),
        }
    }
    blocks
}

/// Which colour each state gets. A state the entity lists (`known`) is coloured by its place in
/// that list, so it's the same colour every day; anything else follows, in alphabetical order.
/// `off` is the one that means nothing is going on.
pub fn tones(known: &[String], off: Option<&str>, seen: &[String]) -> Vec<(String, Tone)> {
    // Each one once, where it first comes: a list that names a state twice still gives it one
    // colour and one line of the legend.
    let mut labels: Vec<String> = Vec::new();
    for label in known {
        if !labels.contains(label) {
            labels.push(label.clone());
        }
    }
    let mut extra: Vec<String> = seen
        .iter()
        .filter(|label| !known.contains(label))
        .cloned()
        .collect();
    extra.sort();
    extra.dedup();
    labels.extend(extra);
    let mut slot = 0;
    labels
        .into_iter()
        .map(|label| {
            let tone = if off == Some(label.as_str()) {
                Tone::Off
            } else if slot < COLOURS {
                slot += 1;
                Tone::Colour(slot - 1)
            } else {
                Tone::Other
            };
            (label, tone)
        })
        .collect()
}

/// How long something lasted: to the second under a minute ("40 s"), to the minute after that
/// ("12 min", "3 h 05 min").
pub fn lasted(ms: f64) -> String {
    let minutes = (ms / 60_000.0).floor() as u64;
    match (minutes / 60, minutes % 60) {
        (0, 0) => format!("{} s", (ms / 1000.0).floor().max(0.0) as u64),
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes:02} min"),
    }
}

/// Which block is under a point `percent` of the way across.
pub fn block_at(blocks: &[Block], percent: f64) -> Option<usize> {
    blocks.iter().rposition(|block| block.left <= percent)
}

const NO_READING: &str = "No reading";

#[component]
pub fn StateTimeline(
    /// The day so far, oldest first.
    spans: Vec<Span>,
    /// The state as it is now, as each one arrives: the strip runs on with it.
    live: Signal<Option<Span>>,
    /// Every state the entity can be in, when it says: what the colours are counted from.
    known: Vec<String>,
    /// The state that means nothing is going on, if there is one.
    off: Option<String>,
    /// What the strip is of, for a screen reader.
    name: String,
) -> impl IntoView {
    let series = RwSignal::new(spans);
    // A state joins the day if it's newer than the last one and says something different.
    Effect::new(move |_| {
        let Some(span) = live.get() else { return };
        let joins = series.with_untracked(|series| {
            series
                .last()
                .is_none_or(|last| span.at_ms > last.at_ms && span.label != last.label)
        });
        if joins {
            series.update(|series| series.push(span));
        }
    });

    // The last block runs on to now, so the strip is drawn again as time passes, not only when
    // something changes.
    let passing = RwSignal::new(0_u32);
    let figure = NodeRef::<leptos::html::Figure>::new();
    let ticking = set_interval_with_handle(
        move || {
            // Only while it can be seen: rolled up, or behind the table, it waits.
            let seen = figure.get_untracked().is_some_and(|figure| {
                figure.offset_parent().is_some()
                    && figure
                        .closest(".drawer:not(.open)")
                        .ok()
                        .flatten()
                        .is_none()
            });
            if seen {
                passing.update(|passing| *passing = passing.wrapping_add(1));
            }
        },
        std::time::Duration::from_secs(20),
    )
    .ok();
    on_cleanup(move || {
        if let Some(ticking) = ticking {
            ticking.clear();
        }
    });

    // The day as it's drawn: its blocks, and the colour of each state in it.
    let drawn = Memo::new(move |_| {
        passing.track();
        let blocks = series.with(|series| blocks(series, web_sys::js_sys::Date::now()));
        let seen: Vec<String> = blocks
            .iter()
            .filter_map(|block| block.label.clone())
            .collect();
        let tones = tones(&known, off.as_deref(), &seen);
        (blocks, tones)
    });
    let tone_of = |tones: &[(String, Tone)], label: &Option<String>| -> Option<Tone> {
        let label = label.as_ref()?;
        tones
            .iter()
            .find(|(known, _)| known == label)
            .map(|(_, tone)| *tone)
    };

    let strip = move || {
        let (blocks, tones) = drawn.get();
        blocks
            .iter()
            .filter_map(|block| {
                let tone = tone_of(&tones, &block.label)?;
                Some(view! {
                    <span
                        class=format!("timeline-block {}", tone.class())
                        style=format!("left:{:.3}%;width:{:.3}%", block.left, block.width)
                    ></span>
                })
            })
            .collect_view()
    };

    // What each state added up to over the day, in the order the colours come in.
    let legend = move || {
        let (blocks, tones) = drawn.get();
        let whole: f64 = blocks.iter().map(|block| block.lasted_ms).sum();
        tones
            .iter()
            .filter_map(|(label, tone)| {
                let total: f64 = blocks
                    .iter()
                    .filter(|block| block.label.as_ref() == Some(label))
                    .map(|block| block.lasted_ms)
                    .sum();
                let times = blocks
                    .iter()
                    .filter(|block| block.label.as_ref() == Some(label))
                    .count();
                (times > 0).then(|| {
                    let share = if whole > 0.0 {
                        total / whole * 100.0
                    } else {
                        0.0
                    };
                    view! {
                        <li>
                            <span class=format!("timeline-swatch {}", tone.class())></span>
                            <span class="timeline-state">{label.clone()}</span>
                            <span class="timeline-total">
                                {format!("{} · {share:.0}%", lasted(total))}
                            </span>
                        </li>
                    }
                })
            })
            .collect_view()
    };

    let summary = move || {
        let (blocks, _) = drawn.get();
        let now = blocks
            .last()
            .map(|block| block.label.clone().unwrap_or_else(|| NO_READING.to_owned()))
            .unwrap_or_default();
        format!(
            "{name}, last 24 hours: {} changes, now {now}. Arrow keys step through them.",
            blocks.len().saturating_sub(1),
        )
    };
    let since = move || {
        series.with(|series| {
            series
                .iter()
                .find(|span| span.label.is_some())
                .map(|span| span.at.clone())
        })
    };

    // What the pointer or the arrow keys are on: the block, and where across the rule is.
    let pointed = RwSignal::new(None::<(usize, f64)>);
    let point_at = move |ev: ev::PointerEvent| {
        let Some(plot) = ev.current_target() else {
            return;
        };
        let rect = plot
            .unchecked_into::<web_sys::Element>()
            .get_bounding_client_rect();
        let percent =
            ((f64::from(ev.client_x()) - rect.left()) / rect.width()).clamp(0.0, 1.0) * 100.0;
        pointed.set(
            drawn.with_untracked(|(blocks, _)| block_at(blocks, percent).map(|i| (i, percent))),
        );
    };
    // The keys land in the middle of a block, where its tip sits squarely over it.
    let middle = |block: &Block| block.left + block.width / 2.0;
    let step = move |ev: ev::KeyboardEvent| {
        drawn.with_untracked(|(blocks, _)| {
            let last = blocks.len().saturating_sub(1);
            let now = pointed.get_untracked().map_or(last, |(index, _)| index);
            let next = match ev.key().as_str() {
                "ArrowLeft" => now.saturating_sub(1),
                "ArrowRight" => (now + 1).min(last),
                "Home" => 0,
                "End" => last,
                _ => return,
            };
            ev.prevent_default();
            if let Some(block) = blocks.get(next) {
                pointed.set(Some((next, middle(block))));
            }
        });
    };
    let on_focus = move |_| {
        if pointed.get_untracked().is_none() {
            drawn.with_untracked(|(blocks, _)| {
                if let Some(block) = blocks.last() {
                    pointed.set(Some((blocks.len() - 1, middle(block))));
                }
            });
        }
    };
    // The block pointed at, in words: what it was, from when to when, and for how long.
    let told = move || {
        let (index, percent) = pointed.get()?;
        let (blocks, tones) = drawn.get();
        let block = blocks.get(index)?;
        let label = block.label.clone().unwrap_or_else(|| NO_READING.to_owned());
        let when = format!(
            "{} – {} · {}",
            block.from,
            block.to.as_deref().unwrap_or("now"),
            lasted(block.lasted_ms),
        );
        Some((percent, label, when, tone_of(&tones, &block.label)))
    };
    let spoken = told;
    let tip = move || {
        told().map(|(percent, label, when, tone)| {
            // Kept inside the strip at either end, rather than hanging off it.
            let anchor = if percent > 75.0 {
                "end"
            } else if percent < 25.0 {
                "start"
            } else {
                "mid"
            };
            view! {
                <span class="chart-rule" style=format!("left:{percent:.2}%")></span>
                <span class=format!("chart-tip {anchor}") style=format!("left:{percent:.2}%")>
                    <strong class="timeline-tip-state">
                        {tone.map(|tone| view! {
                            <span class=format!("timeline-swatch {}", tone.class())></span>
                        })}
                        {label}
                    </strong>
                    <span>{when}</span>
                </span>
            }
        })
    };

    view! {
        <figure class="chart timeline" node_ref=figure>
            <div
                class="timeline-plot"
                tabindex="0"
                role="group"
                aria-label=summary
                on:pointermove=point_at
                on:pointerleave=move |_| pointed.set(None)
                on:keydown=step
                on:focus=on_focus
                on:blur=move |_| pointed.set(None)
            >
                <div class="timeline-track">{strip}</div>
                {tip}
            </div>
            <figcaption class="chart-axis timeline-axis">
                <span>{since}</span>
                <span>"now"</span>
            </figcaption>
            <ul class="timeline-legend">{legend}</ul>
            // What the keys are on, said out loud as it changes.
            <p class="visually-hidden" aria-live="polite">
                {move || spoken().map(|(_, label, when, _)| format!("{label}, {when}"))}
            </p>
        </figure>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(at_ms: f64, label: Option<&str>) -> Span {
        Span {
            at_ms,
            at: format!("t{at_ms}"),
            label: label.map(str::to_owned),
        }
    }

    /// Each state runs until the next one starts, and the last runs on to now: the widths are
    /// how long each lasted, and together they fill the strip.
    #[test]
    fn a_block_is_as_wide_as_its_state_lasted() {
        let day = [
            span(0.0, Some("idle")),
            span(250.0, Some("playing")),
            span(750.0, Some("paused")),
        ];
        let blocks = blocks(&day, 1000.0);
        let shape: Vec<(f64, f64)> = blocks.iter().map(|b| (b.left, b.width)).collect();
        assert_eq!(shape, [(0.0, 25.0), (25.0, 50.0), (75.0, 25.0)]);
        assert_eq!(blocks[1].from, "t250");
        assert_eq!(blocks[1].to.as_deref(), Some("t750"));
        assert_eq!(blocks[2].to, None);
        assert_eq!(blocks[1].lasted_ms, 500.0);
    }

    /// The same state said twice is one stretch of it; a gap is a block of its own, with no
    /// label; and the day starts when something was first said.
    #[test]
    fn repeats_join_and_gaps_stay_empty() {
        let day = [
            span(0.0, None),
            span(100.0, Some("on")),
            span(300.0, Some("on")),
            span(500.0, None),
            span(700.0, Some("off")),
        ];
        let blocks = blocks(&day, 1100.0);
        let labels: Vec<Option<&str>> = blocks.iter().map(|b| b.label.as_deref()).collect();
        assert_eq!(labels, [Some("on"), None, Some("off")]);
        assert_eq!(blocks[0].left, 0.0);
        assert_eq!(blocks[0].width, 40.0);
        assert_eq!(blocks[0].to.as_deref(), Some("t500"));
        assert!(super::blocks(&[span(0.0, None)], 10.0).is_empty());
    }

    /// A state's colour is its place among what the entity can say, whichever of them happened
    /// today; the quiet one takes no colour; and past eight, the rest share one.
    #[test]
    fn a_state_keeps_its_colour() {
        let known: Vec<String> = ["Off", "Idle", "Playing", "Paused"]
            .map(str::to_owned)
            .to_vec();
        let today = tones(&known, Some("Off"), &["Paused".to_owned()]);
        let other_day = tones(&known, Some("Off"), &["Idle".to_owned(), "Zzz".to_owned()]);
        let paused = |tones: &[(String, Tone)]| {
            tones
                .iter()
                .find(|(label, _)| label == "Paused")
                .map(|(_, tone)| *tone)
        };
        assert_eq!(paused(&today), Some(Tone::Colour(2)));
        assert_eq!(paused(&today), paused(&other_day));
        assert_eq!(today[0], ("Off".to_owned(), Tone::Off));
        assert_eq!(other_day.last(), Some(&("Zzz".to_owned(), Tone::Colour(3))));

        let twice = ["a", "b", "a", "c"].map(str::to_owned);
        let tones_twice = tones(&twice, None, &[]);
        assert_eq!(tones_twice.len(), 3);
        assert_eq!(tones_twice[2], ("c".to_owned(), Tone::Colour(2)));

        let many: Vec<String> = (0..10).map(|n| format!("s{n}")).collect();
        let tones = tones(&[], None, &many);
        assert_eq!(tones[7].1, Tone::Colour(7));
        assert_eq!(tones[8].1, Tone::Other);
    }

    #[test]
    fn durations_read_to_the_minute() {
        assert_eq!(lasted(20_900.0), "20 s");
        assert_eq!(lasted(12.0 * 60_000.0), "12 min");
        assert_eq!(lasted(180.0 * 60_000.0), "3 h");
        assert_eq!(lasted(185.0 * 60_000.0), "3 h 05 min");
    }

    /// Pointing anywhere in a block is pointing at that block.
    #[test]
    fn a_point_belongs_to_the_block_it_is_over() {
        let day = [span(0.0, Some("a")), span(400.0, Some("b"))];
        let blocks = blocks(&day, 1000.0);
        assert_eq!(block_at(&blocks, 0.0), Some(0));
        assert_eq!(block_at(&blocks, 39.9), Some(0));
        assert_eq!(block_at(&blocks, 40.0), Some(1));
        assert_eq!(block_at(&blocks, 100.0), Some(1));
    }
}
