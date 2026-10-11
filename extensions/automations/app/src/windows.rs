//! Whether a window of time holds right now: "after 14:00", "from sunset until sunrise".
//!
//! The engine is asked (`clock.holds`), as it is for when a trigger next fires. The page does
//! no time zone arithmetic of its own, so a dot on the canvas and a run can't disagree about
//! what time it is at home. Each window is asked about when it's first drawn, and all of them
//! again every half minute, which is as fine as a window is written.

use std::collections::BTreeMap;
use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;
use serde_json::{Value, json};

/// How often every window on show is asked about again.
const EVERY: Duration = Duration::from_secs(30);

/// More windows than a flow ever has on show: past this, what was asked is forgotten and
/// asked again as it's drawn. Every edit of a time is a window that won't be seen again.
const MOST: usize = 96;

/// What the engine says of one window.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Told {
    /// Whether it holds now, when that can be told.
    #[serde(default)]
    pub holds: Option<bool>,
    /// Why it can't be: no time zone, no location, no sunset at home today.
    #[serde(default)]
    pub why: Option<String>,
}

/// What the engine says of the home's clock, with every answer.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct AtHome {
    #[serde(default)]
    pub time_zone: Option<String>,
    #[serde(default)]
    pub location: bool,
    /// The time on the wall at home, "14:05".
    #[serde(default)]
    pub now: Option<String>,
}

#[derive(Deserialize)]
struct Answer {
    #[serde(flatten)]
    at_home: AtHome,
    #[serde(default)]
    holds: Vec<Told>,
}

/// Every window the page has drawn, and what the engine last said of each.
#[derive(Debug, Clone, Copy)]
pub struct Windows {
    /// By the window's own JSON. `None` while the first answer is on its way.
    told: RwSignal<BTreeMap<String, Option<Told>>>,
    pub at_home: RwSignal<Option<AtHome>>,
}

impl Windows {
    /// Starts asking: once for each window as it's drawn, and all of them every half minute.
    pub fn start() -> Self {
        let windows = Self {
            told: RwSignal::new(BTreeMap::new()),
            at_home: RwSignal::new(None),
        };
        spawn_local(async move {
            loop {
                gloo_timers::future::sleep(EVERY).await;
                let keys: Vec<String> = windows
                    .told
                    .with_untracked(|told| told.keys().cloned().collect());
                if keys.len() > MOST {
                    // Whatever is still on show asks again as it's drawn again.
                    windows.told.set(BTreeMap::new());
                } else if !keys.is_empty() {
                    windows.ask(keys);
                }
            }
        });
        windows
    }

    /// What's known of `window` (a `time` or `sun` condition). Reads a signal, so a view using
    /// it follows the answers as they arrive.
    pub fn told(&self, window: &Value) -> Option<Told> {
        let key = window.to_string();
        if let Some(told) = self.told.with(|told| told.get(&key).cloned()) {
            return told;
        }
        // Noted quietly: whoever is reading is told when the answer comes, not now.
        self.told.update_untracked(|told| {
            told.insert(key.clone(), None);
        });
        self.ask(vec![key]);
        None
    }

    fn ask(&self, keys: Vec<String>) {
        let windows = *self;
        let conditions: Vec<Value> = keys
            .iter()
            .map(|key| serde_json::from_str(key).unwrap_or(Value::Null))
            .collect();
        spawn_local(async move {
            let answer: Result<Answer, String> = crate::bridge()
                .rpc("clock.holds", json!({ "conditions": conditions }))
                .await;
            let Ok(answer) = answer else {
                return;
            };
            if windows.at_home.get_untracked().as_ref() != Some(&answer.at_home) {
                windows.at_home.set(Some(answer.at_home));
            }
            let fresh: Vec<(String, Told)> = keys.into_iter().zip(answer.holds).collect();
            let changed = windows.told.with_untracked(|told| {
                fresh
                    .iter()
                    .any(|(key, now)| told.get(key).and_then(Option::as_ref) != Some(now))
            });
            if changed {
                windows.told.update(|told| {
                    for (key, now) in fresh {
                        told.insert(key, Some(now));
                    }
                });
            }
        });
    }
}
