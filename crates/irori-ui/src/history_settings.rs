//! How long entity history is kept. Detailed history is every change, for a number of days.
//! Hourly summaries of a sensor that measures or counts are kept after that, forever or for a
//! number of days. Both are written to `[recorder]` in `irori.toml` and applied immediately.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{self, RecorderSettings};

/// What the folded History row says: "10 days · hourly kept", or "10 days · hourly 365 days".
pub fn summary(settings: Option<RecorderSettings>) -> String {
    let Some(settings) = settings else {
        return String::new();
    };
    match settings.summary_days {
        None => format!("{} days · hourly kept", settings.retain_days),
        Some(days) => format!("{} days · hourly {days} days", settings.retain_days),
    }
}

#[component]
pub fn Section(
    /// Whether this person may change how the home is set up. Anyone can read the row.
    owner: Signal<bool>,
    /// The settings last fetched or saved, so the folded row stays in step with the form.
    current: RwSignal<Option<RecorderSettings>>,
) -> impl IntoView {
    let retain = RwSignal::new(10_u32);
    let kept = RwSignal::new(true);
    let summary_days = RwSignal::new(365_u32);
    let trouble = RwSignal::new(None::<String>);
    let saving = RwSignal::new(false);
    // The check that draws itself once a save has landed.
    let done = RwSignal::new(false);
    // Set once the person changes a field. A save's answer arrives a moment later and must
    // not put the form back to what it was when they clicked, unless they haven't typed since.
    let touched = RwSignal::new(false);

    Effect::new(move |_| {
        let settings = current.get();
        if touched.get_untracked() {
            return;
        }
        if let Some(settings) = settings {
            retain.set(settings.retain_days.max(1));
            match settings.summary_days {
                None => kept.set(true),
                Some(days) => {
                    kept.set(false);
                    summary_days.set(days.max(1));
                }
            }
        }
    });

    // What's unsaved. Nothing is, until the home has answered: saving the defaults over a
    // slower answer would throw away whatever the file already says.
    let changed = move || {
        current.with(|saved| {
            saved.as_ref().is_some_and(|saved| {
                saved
                    != &RecorderSettings {
                        retain_days: retain.get().max(1),
                        summary_days: if kept.get() {
                            None
                        } else {
                            Some(summary_days.get().max(1))
                        },
                    }
            })
        })
    };

    let save = move |_| {
        if !owner.get_untracked() || saving.get_untracked() || !changed() {
            return;
        }
        saving.set(true);
        trouble.set(None);
        let sent = RecorderSettings {
            retain_days: retain.get_untracked().max(1),
            summary_days: if kept.get_untracked() {
                None
            } else {
                Some(summary_days.get_untracked().max(1))
            },
        };
        spawn_local(async move {
            match api::save_recorder(&sent).await {
                Ok(saved) => {
                    let still = RecorderSettings {
                        retain_days: retain.get_untracked().max(1),
                        summary_days: if kept.get_untracked() {
                            None
                        } else {
                            Some(summary_days.get_untracked().max(1))
                        },
                    };
                    // They kept editing while this was in flight: the answer is for the older
                    // numbers, so the form stays where their hands are and the check waits.
                    let settled = still == sent;
                    if settled {
                        touched.set(false);
                        done.set(true);
                    }
                    current.set(Some(saved));
                    if settled {
                        gloo_timers::future::TimeoutFuture::new(1600).await;
                        let _ = done.try_set(false);
                    }
                }
                Err(why) => trouble.set(Some(why)),
            }
            let _ = saving.try_set(false);
        });
    };

    view! {
        <div class="history-settings">
            <div class="setting">
                <div class="setting-words">
                    <span class="setting-name">"Detailed history"</span>
                    <p class="muted small">
                        "Every change, for this many days. A device's day is drawn from these, \
                         and a sensor that measures or counts also keeps a five-minute summary \
                         for the same days."
                    </p>
                </div>
                {days("Detailed history, in days", retain, owner, touched)}
            </div>
            <div>
                <div class="setting">
                    <div class="setting-words">
                        <span class="setting-name">"Hourly summaries"</span>
                        <p class="muted small">
                            "One an hour, after the detailed days are gone. A week, a month, and \
                             a year are drawn from these. Keeping them is the usual choice."
                        </p>
                    </div>
                    <div class="history-choice" inert=move || (!owner.get()).then_some("")>
                        {crate::segmented::segmented(
                            "Hourly summaries",
                            vec![(true, "Kept"), (false, "For a number of days")],
                            kept.into(),
                            move |forever| {
                                if !owner.get_untracked() {
                                    return;
                                }
                                touched.set(true);
                                // A finite summary shorter than the detailed history is refused.
                                // Start from a year, or from the detailed history when that is
                                // already longer.
                                if !forever && kept.get_untracked() {
                                    summary_days.set(365.max(retain.get_untracked()));
                                }
                                kept.set(forever);
                            },
                        )}
                    </div>
                </div>
                // Rolled open rather than mounted, so the days arrive the way every other
                // drawer does. Inert while kept forever: nothing in there to tab to.
                <div class="drawer" class:open=move || !kept.get() inert=move || kept.get().then_some("")>
                    <div class="drawer-inner">
                        <div class="setting">
                            <div class="setting-words">
                                <span class="setting-name">"How long"</span>
                                <p class="muted small">
                                    "At least as many days as the detailed history."
                                </p>
                            </div>
                            {days("Days of hourly summaries", summary_days, owner, touched)}
                        </div>
                    </div>
                </div>
            </div>
            {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
            {move || owner.get().then(|| view! {
                <div class="place-actions">
                    <span
                        class="muted small"
                        class:history-saved=move || done.get()
                        class:history-unsaved=move || !done.get() && changed()
                    >
                        {move || {
                            if done.get() {
                                "Saved. Irori is already keeping history for that long."
                            } else if changed() {
                                "Not saved yet."
                            } else {
                                ""
                            }
                        }}
                    </span>
                    <button
                        type="button"
                        class="add-one"
                        class:busy=move || saving.get()
                        class:done=move || done.get()
                        disabled=move || saving.get() || !changed()
                        on:click=save
                    >
                        <span class="add-one-label">"Save"</span>
                        <svg class="add-one-check" viewBox="0 0 24 24" aria-hidden="true">
                            <path d="M5 12.5 10 17.5 19 7" pathLength="1" fill="none"
                                stroke="currentColor" stroke-width="2.4" stroke-linecap="round"
                                stroke-linejoin="round" />
                        </svg>
                    </button>
                </div>
            })}
        </div>
    }
}

/// A number of days: typed, or nudged by one. The digits take the same ember hop a reading
/// does when they move (`[data-moved]`).
fn days(
    label: &'static str,
    value: RwSignal<u32>,
    owner: Signal<bool>,
    touched: RwSignal<bool>,
) -> impl IntoView {
    let way = RwSignal::new(None::<&'static str>);
    let put = move |next: u32| {
        let next = next.max(1);
        let before = value.get_untracked();
        if next == before {
            return;
        }
        let letter_b = way.get_untracked().is_none_or(|word| word.ends_with('a'));
        way.set(Some(match (next > before, letter_b) {
            (true, false) => "up-a",
            (true, true) => "up-b",
            (false, false) => "down-a",
            (false, true) => "down-b",
        }));
        touched.set(true);
        value.set(next);
    };
    view! {
        <span class="days-control">
            <span class="stepper days" role="group" aria-label=label>
                <button
                    type="button"
                    aria-label="One day fewer"
                    disabled=move || !owner.get() || value.get() <= 1
                    on:click=move |_| put(value.get_untracked().saturating_sub(1))
                >
                    "−"
                </button>
                <input
                    type="number"
                    min="1"
                    inputmode="numeric"
                    autocomplete="off"
                    aria-label=label
                    disabled=move || !owner.get()
                    data-moved=move || way.get()
                    prop:value=move || value.get().to_string()
                    on:input=move |event| {
                        touched.set(true);
                        if let Ok(days) = event_target_value(&event).parse::<u32>()
                            && days >= 1
                        {
                            put(days);
                        }
                    }
                />
                <button
                    type="button"
                    aria-label="One day more"
                    disabled=move || !owner.get() || value.get() == u32::MAX
                    on:click=move |_| put(value.get_untracked().saturating_add(1))
                >
                    "+"
                </button>
            </span>
            <span class="muted small">"days"</span>
        </span>
    }
}
