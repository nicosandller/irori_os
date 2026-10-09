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
    // Set once the person changes a field. A save's answer arrives a moment later and must
    // not put the form back to what it was when they clicked.
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

    let save = move || {
        if !owner.get_untracked() || saving.get_untracked() {
            return;
        }
        saving.set(true);
        trouble.set(None);
        let settings = RecorderSettings {
            retain_days: retain.get_untracked().max(1),
            summary_days: if kept.get_untracked() {
                None
            } else {
                Some(summary_days.get_untracked().max(1))
            },
        };
        spawn_local(async move {
            match api::save_recorder(&settings).await {
                Ok(saved) => current.set(Some(saved)),
                Err(why) => trouble.set(Some(why)),
            }
            saving.set(false);
        });
    };

    view! {
        <div class="settings-form">
            <p class="muted small">
                "Detailed history is every change, kept for the number of days below, plus a \
                 five-minute summary for a sensor that measures or counts. The device page's day \
                 is drawn from the changes. Hourly summaries are the week, month, and year, and \
                 keeping them is the usual choice."
            </p>
            <label class="settings-field">
                <span>"Detailed history, in days"</span>
                <input
                    type="number"
                    min="1"
                    disabled=move || !owner.get()
                    prop:value=move || retain.get().to_string()
                    on:input=move |event| {
                        touched.set(true);
                        if let Ok(days) = event_target_value(&event).parse::<u32>()
                            && days >= 1
                        {
                            retain.set(days);
                        }
                    }
                />
            </label>
            <div class="settings-field">
                <span>"Hourly summaries"</span>
                {crate::segmented::segmented(
                    "Hourly summaries",
                    vec![(true, "Kept"), (false, "For a number of days")],
                    kept.into(),
                    move |forever| {
                        touched.set(true);
                        // A finite summary shorter than the detailed history is refused. Start
                        // from a year, or from the detailed history when that is already longer.
                        if !forever && kept.get_untracked() {
                            summary_days.set(365.max(retain.get_untracked()));
                        }
                        kept.set(forever);
                    },
                )}
            </div>
            {move || (!kept.get()).then(|| view! {
                <label class="settings-field">
                    <span>"Days of hourly summaries"</span>
                    <input
                        type="number"
                        min="1"
                        disabled=move || !owner.get()
                        prop:value=move || summary_days.get().to_string()
                        on:input=move |event| {
                            touched.set(true);
                            if let Ok(days) = event_target_value(&event).parse::<u32>()
                                && days >= 1
                            {
                                summary_days.set(days);
                            }
                        }
                    />
                </label>
            })}
            {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
            {move || owner.get().then(|| view! {
                <div class="settings-form-actions">
                    <button
                        type="button"
                        class="add"
                        disabled=move || saving.get()
                        on:click=move |_| save()
                    >
                        {move || if saving.get() { "Saving…" } else { "Save" }}
                    </button>
                </div>
            })}
        </div>
    }
}
