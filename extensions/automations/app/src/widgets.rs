//! Controls the page is made of: a switch, and a combo box — a big text field that searches as
//! you type, with the matches in a list below it.

use leptos::prelude::*;

/// On/off, as a switch that slides.
#[component]
pub fn Toggle(
    #[prop(into)] on: Signal<bool>,
    #[prop(into)] set: Callback<bool>,
    #[prop(optional, into)] label: String,
) -> impl IntoView {
    view! {
        <button
            type="button"
            role="switch"
            class="switch"
            class:on=move || on.get()
            aria-checked=move || on.get().to_string()
            aria-label=label.clone()
            title=label
            on:click=move |event| {
                event.stop_propagation();
                set.run(!on.get_untracked());
            }
        >
            <span class="knob"></span>
        </button>
    }
}

/// One thing a combo box offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    /// What picking it gives.
    pub value: String,
    /// What a person reads.
    pub label: String,
    /// Smaller, beside the label: an entity's id, its state now.
    pub detail: String,
    /// What it's part of, read before the label: the device an entity belongs to.
    pub group: String,
}

impl Choice {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            detail: String::new(),
            group: String::new(),
        }
    }

    pub fn group(mut self, group: impl Into<String>) -> Self {
        self.group = group.into();
        self
    }

    /// The label as one line of text, what it's part of first.
    fn whole(&self) -> String {
        if self.group.is_empty() {
            self.label.clone()
        } else {
            format!("{} – {}", self.group, self.label)
        }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    fn matches(&self, query: &str) -> bool {
        let query = query.to_lowercase();
        query.split_whitespace().all(|word| {
            self.label.to_lowercase().contains(word)
                || self.group.to_lowercase().contains(word)
                || self.value.to_lowercase().contains(word)
                || self.detail.to_lowercase().contains(word)
        })
    }
}

/// `text` with the first match of `query` marked.
fn marked(text: &str, query: &str) -> AnyView {
    let lower = text.to_lowercase();
    let word = query
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    match (!word.is_empty()).then(|| lower.find(&word)).flatten() {
        Some(start)
            if text.is_char_boundary(start) && text.is_char_boundary(start + word.len()) =>
        {
            let (before, rest) = text.split_at(start);
            let (hit, after) = rest.split_at(word.len());
            view! { {before.to_owned()}<mark>{hit.to_owned()}</mark>{after.to_owned()} }.into_any()
        }
        _ => text.to_owned().into_any(),
    }
}

/// A text field that searches `choices` as you type, the matches in a list below it. Arrow keys
/// move through them, Enter picks, Escape closes. With `custom`, what's typed can be picked
/// as it is (a number, say) when nothing matches.
#[component]
pub fn Combo(
    #[prop(into)] choices: Signal<Vec<Choice>>,
    #[prop(into)] value: Signal<String>,
    #[prop(into)] pick: Callback<String>,
    #[prop(optional, into)] placeholder: String,
    #[prop(optional)] custom: bool,
) -> impl IntoView {
    let open = RwSignal::new(false);
    let query = RwSignal::new(String::new());
    let active = RwSignal::new(0usize);
    let input = NodeRef::<leptos::html::Input>::new();

    let shown = move || {
        let v = value.get();
        choices.with(|all| {
            all.iter()
                .find(|c| c.value == v)
                .map(Choice::whole)
                .unwrap_or(v)
        })
    };
    // The picked one's two parts, when it has two: drawn over the field so that a long device
    // name is what gets cut short, where the field's own text would lose its end, the name.
    let current_parts = move || {
        let v = value.get();
        choices.with(|all| {
            all.iter()
                .find(|c| c.value == v && !c.group.is_empty())
                .map(|c| (c.group.clone(), c.label.clone()))
        })
    };
    let parted = move || !open.get() && current_parts().is_some();
    let current_detail = move || {
        let v = value.get();
        choices.with(|all| all.iter().find(|c| c.value == v).map(|c| c.detail.clone()))
    };
    let matches = move || {
        let q = query.get();
        choices.with(|all| {
            all.iter()
                .filter(|c| q.trim().is_empty() || c.matches(&q))
                .take(60)
                .cloned()
                .collect::<Vec<_>>()
        })
    };
    let choose = move |value: String| {
        pick.run(value);
        open.set(false);
        query.set(String::new());
        if let Some(input) = input.get_untracked() {
            let _ = input.blur();
        }
    };

    view! {
        // The hint beside the value ("now") gets room of its own, so the two never overlap.
        <div class="combo" class:open=move || open.get()
            style=move || {
                let chars = if open.get() { 0 } else { current_detail().map_or(0, |d| d.chars().count()) };
                format!("--detail-chars:{chars}")
            }>
            <input
                type="text"
                node_ref=input
                class="combo-input"
                class:parted=parted
                autocomplete="off"
                spellcheck="false"
                placeholder=placeholder
                prop:value=move || if open.get() { query.get() } else { shown() }
                on:focus=move |_| {
                    query.set(String::new());
                    active.set(0);
                    open.set(true);
                }
                on:blur=move |_| open.set(false)
                on:input=move |event| {
                    query.set(event_target_value(&event));
                    active.set(0);
                    open.set(true);
                }
                on:keydown=move |event: web_sys::KeyboardEvent| {
                    let count = matches().len();
                    match event.key().as_str() {
                        "ArrowDown" => {
                            event.prevent_default();
                            open.set(true);
                            active.update(|a| *a = (*a + 1).min(count.saturating_sub(1)));
                        }
                        "ArrowUp" => {
                            event.prevent_default();
                            active.update(|a| *a = a.saturating_sub(1));
                        }
                        "Enter" => {
                            event.prevent_default();
                            let found = matches();
                            if let Some(choice) = found.get(active.get_untracked()) {
                                choose(choice.value.clone());
                            } else if custom && !query.get_untracked().trim().is_empty() {
                                choose(query.get_untracked().trim().to_owned());
                            }
                        }
                        "Escape" => {
                            // Only the list closes, not whatever this field sits in.
                            event.prevent_default();
                            open.set(false);
                            if let Some(input) = input.get_untracked() {
                                let _ = input.blur();
                            }
                        }
                        _ => {}
                    }
                }
            />
            {move || parted().then(current_parts).flatten().map(|(group, name)| view! {
                <span class="combo-shown" aria-hidden="true">
                    <span class="combo-group">{group}</span>
                    <span class="combo-sep">"–"</span>
                    <span class="combo-name">{name}</span>
                </span>
            })}
            {move || (!open.get()).then(|| current_detail().filter(|d| !d.is_empty()).map(|d| view! {
                <span class="combo-detail">{d}</span>
            }))}
            <span class="combo-chevron" aria-hidden="true"></span>
            <ul class="combo-list" role="listbox">
                {move || {
                    let q = query.get();
                    let found = matches();
                    if found.is_empty() {
                        let text = if custom && !q.trim().is_empty() {
                            format!("Press Enter to use “{}”", q.trim())
                        } else {
                            "Nothing matches".to_owned()
                        };
                        return view! { <li class="combo-empty">{text}</li> }.into_any();
                    }
                    let current = value.get();
                    found.into_iter().enumerate().map(|(i, choice)| {
                        let value = choice.value.clone();
                        let chosen = choice.value == current;
                        view! {
                            <li
                                role="option"
                                class="combo-option"
                                class:active=move || active.get() == i
                                class:chosen=chosen
                                aria-selected=chosen.to_string()
                                // Before the field loses focus and closes the list. Picking
                                // redraws the list at once, so the press mustn't go on to what
                                // holds it: by then the option isn't in the page to say where
                                // it was.
                                on:pointerdown=move |event| {
                                    event.prevent_default();
                                    event.stop_propagation();
                                    choose(value.clone());
                                }
                                on:pointerenter=move |_| active.set(i)
                            >
                                <span class="combo-label">
                                    // The part that gives way when the two don't fit: the
                                    // device is the same down a run of rows, the name isn't.
                                    {(!choice.group.is_empty()).then(|| view! {
                                        <span class="combo-group">{marked(&choice.group, &q)}</span>
                                        <span class="combo-sep" aria-hidden="true">"–"</span>
                                    })}
                                    <span class="combo-name">{marked(&choice.label, &q)}</span>
                                </span>
                                {(!choice.detail.is_empty()).then(|| view! {
                                    <span class="combo-option-detail">{marked(&choice.detail, &q)}</span>
                                })}
                            </li>
                        }
                    }).collect_view().into_any()
                }}
            </ul>
        </div>
    }
}
