//! The flow playing out on the canvas as it runs, while it's being edited: a trigger flashes,
//! the wire it takes lights up from one end to the other and stays lit until the run is over, a
//! condition gets a ✓ or ✗ in its corner, an action lights up with what it sent — one step after
//! another, a beat apart, so it can be followed. A near-miss makes its trigger shiver, and a
//! trigger counting down a `for` fills a bar.
//!
//! Most runs are over in milliseconds, long before anything could watch them happen, so this
//! plays each finished run back from its record, as soon as it's heard of.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use irori_flow_types::api::Holding;
use irori_flow_types::trace::{RunRecord, Step, TestKind};
use irori_flow_types::{NodeId, Port, Wire};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::editor::{Editing, View};

/// A beat: from one step to the next.
const BEAT: Duration = Duration::from_millis(520);
/// How long a step takes to travel along its wire; `.wire.glowing` and `.wire.spark` in
/// index.html take the same time.
const TRAVEL: Duration = Duration::from_millis(450);
/// How long a run's lights stay on once it's played.
const LINGER: Duration = Duration::from_millis(2600);
/// Most runs waiting to be played; more, and the oldest are skipped to keep up.
const MOST_WAITING: usize = 3;

/// How a node came out, as the canvas marks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Fired,
    Yes,
    No,
    Done,
    Failed,
    Waiting,
    Missed,
}

impl Mark {
    pub fn class(self) -> &'static str {
        match self {
            Self::Fired => "lit lit-fired",
            Self::Yes => "lit lit-yes",
            Self::No => "lit lit-no",
            Self::Done => "lit lit-done",
            Self::Failed => "lit lit-failed",
            Self::Waiting => "lit lit-waiting",
            Self::Missed => "lit lit-missed",
        }
    }

    /// The badge in the node's corner.
    pub fn badge(self) -> &'static str {
        match self {
            Self::Fired => "⚡",
            Self::Yes | Self::Done => "✓",
            Self::No => "✗",
            Self::Failed => "!",
            Self::Waiting => "…",
            Self::Missed => "·",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Lit {
    pub mark: Mark,
    /// A few words on what happened there: "idle → paused", "brightness 58%".
    pub note: Option<String>,
    /// Which playing lit it, so its lights go out together.
    pub show: u64,
}

/// What the canvas is showing of the flow running.
#[derive(Debug, Clone, Copy)]
pub struct Show {
    pub lit: RwSignal<BTreeMap<NodeId, Lit>>,
    /// Wires the run passed along, glowing until playing `n` fades.
    pub flowing: RwSignal<BTreeMap<Wire, u64>>,
    pub holding: RwSignal<Vec<Holding>>,
    /// The latest thing that happened, in words, for the strip at the bottom.
    pub latest: RwSignal<Option<(u64, String)>>,
}

impl Show {
    pub fn new() -> Self {
        Self {
            lit: RwSignal::new(BTreeMap::new()),
            flowing: RwSignal::new(BTreeMap::new()),
            holding: RwSignal::new(Vec::new()),
            latest: RwSignal::new(None),
        }
    }
}

/// Watches the flow while the editor is open, and plays what it does.
pub fn watch(ed: Editing, show: Show) {
    let alive = StoredValue::new(true);
    let queue = StoredValue::new(VecDeque::<RunRecord>::new());
    let playing = StoredValue::new(false);
    let counter = StoredValue::new(0u64);
    on_cleanup(move || alive.set_value(false));

    spawn_local(async move {
        let mut after = None;
        loop {
            if !alive.try_get_value().unwrap_or(false) {
                return;
            }
            let id = ed.id();
            if !id.is_empty() && !ed.is_new.get_untracked() {
                if let Ok(live) = api::live(&id, after).await {
                    after = Some(live.now);
                    let _ = show.holding.try_set(live.holding);
                    for miss in live.near_misses {
                        let n = next(counter);
                        let _ = show.lit.try_update(|lit| {
                            lit.insert(
                                miss.node.clone(),
                                Lit {
                                    mark: Mark::Missed,
                                    note: Some(miss.message.clone()),
                                    show: n,
                                },
                            );
                        });
                        let _ = show
                            .latest
                            .try_set(Some((n, format!("Almost: {}", miss.message))));
                        spawn_local(fade(show, n, LINGER));
                    }
                    let runs: Vec<RunRecord> = live
                        .runs
                        .into_iter()
                        // A dry run is played in the Test tab; it didn't happen in the home.
                        .filter(|run| run.test != Some(TestKind::Dry))
                        .collect();
                    if !runs.is_empty() {
                        queue.update_value(|q| {
                            q.extend(runs);
                            while q.len() > MOST_WAITING {
                                q.pop_front();
                            }
                        });
                        if !playing.get_value() {
                            playing.set_value(true);
                            spawn_local(play(ed, show, queue, playing, counter, alive));
                        }
                    }
                }
            } else {
                after = None;
            }
            gloo_timers::future::sleep(Duration::from_secs(1)).await;
        }
    });
}

fn next(counter: StoredValue<u64>) -> u64 {
    counter.update_value(|n| *n += 1);
    counter.get_value()
}

/// Plays the queue, one run after another.
async fn play(
    ed: Editing,
    show: Show,
    queue: StoredValue<VecDeque<RunRecord>>,
    playing: StoredValue<bool>,
    counter: StoredValue<u64>,
    alive: StoredValue<bool>,
) {
    loop {
        if !alive.try_get_value().unwrap_or(false) {
            return;
        }
        let Some(run) = queue.try_update_value(VecDeque::pop_front).flatten() else {
            let _ = playing.try_set_value(false);
            return;
        };
        // Only while editing: a trace or a comparison has the canvas to itself.
        if !matches!(ed.view.try_get_untracked(), Some(View::Edit)) {
            continue;
        }
        let n = next(counter);
        let _ = show.latest.try_set(Some((n, run_words(&run))));
        for step in &run.steps {
            if !alive.try_get_value().unwrap_or(false) {
                return;
            }
            // Only what the draft still has: nodes and wires may have been changed since.
            let known = ed
                .draft
                .try_with_untracked(|d| {
                    d.as_ref().is_some_and(|f| f.nodes.contains_key(&step.node))
                })
                .unwrap_or(false);
            if !known {
                continue;
            }
            if let Some(via) = &step.via {
                let _ = show.flowing.try_update(|f| {
                    f.insert(via.clone(), n);
                });
                gloo_timers::future::sleep(TRAVEL).await;
            }
            let (mark, note) = mark_of(step, &run);
            let _ = show.lit.try_update(|lit| {
                lit.insert(
                    step.node.clone(),
                    Lit {
                        mark,
                        note,
                        show: n,
                    },
                );
            });
            gloo_timers::future::sleep(BEAT / 2).await;
        }
        spawn_local(fade(show, n, LINGER));
    }
}

/// Puts out playing `n`'s lights after a while, unless something newer lit them since.
async fn fade(show: Show, n: u64, after: Duration) {
    gloo_timers::future::sleep(after).await;
    let _ = show.lit.try_update(|lit| lit.retain(|_, l| l.show != n));
    let _ = show.flowing.try_update(|f| f.retain(|_, s| *s != n));
    let _ = show.latest.try_update(|latest| {
        if latest.as_ref().is_some_and(|(shown, _)| *shown == n) {
            *latest = None;
        }
    });
}

/// How a step came out, and a few words on it.
fn mark_of(step: &Step, run: &RunRecord) -> (Mark, Option<String>) {
    if step.node == run.trigger && step.via.is_none() {
        return (Mark::Fired, step.note.as_deref().map(trigger_words));
    }
    if let Some(call) = &step.call {
        return match &call.result {
            Some(Err(error)) => (Mark::Failed, Some(error.clone())),
            _ => {
                let level = call
                    .data
                    .as_ref()
                    .and_then(|d| d.get("brightness_pct"))
                    .and_then(serde_json::Value::as_f64)
                    .map(|pct| format!("at {pct}%"));
                (Mark::Done, level)
            }
        };
    }
    match step.port {
        Some(Port::Yes) => (Mark::Yes, None),
        Some(Port::No) => (Mark::No, None),
        Some(Port::Case(n)) => (Mark::Yes, Some(format!("case {n}"))),
        Some(Port::Else) => (Mark::No, Some("none".into())),
        Some(Port::Matched) => (Mark::Yes, Some("matched".into())),
        Some(Port::Timeout) => (Mark::No, Some("gave up".into())),
        Some(Port::Error) => (Mark::Failed, None),
        Some(Port::Out) => (Mark::Done, step.note.clone().filter(|n| n.contains(" = "))),
        None => (Mark::Waiting, None),
    }
}

/// A trigger's note without its entity id: "idle → paused", "held “paused” for 2s".
fn trigger_words(note: &str) -> String {
    let rest = note
        .split_once(' ')
        .filter(|(first, _)| first.contains('.'))
        .map_or(note, |(_, rest)| rest);
    // Straight quotes become curly ones, opening and closing in turn.
    let mut open = false;
    rest.trim_start_matches("went ")
        .chars()
        .map(|c| match c {
            '"' => {
                open = !open;
                if open { '“' } else { '”' }
            }
            other => other,
        })
        .collect()
}

/// A run in a line, for the strip under the canvas.
fn run_words(run: &RunRecord) -> String {
    let summary = run.summary();
    let how = if summary.summary.is_empty() {
        summary.outcome
    } else {
        summary.summary
    };
    format!("{} fired · {how}", run.trigger)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trigger_note_reads_without_its_entity_id() {
        assert_eq!(
            trigger_words("sensor.tv went \"idle\" → \"paused\""),
            "“idle” → “paused”"
        );
        assert_eq!(
            trigger_words("sensor.tv held \"off\" for 2s"),
            "held “off” for 2s"
        );
        assert_eq!(trigger_words("fired by hand"), "fired by hand");
    }
}
