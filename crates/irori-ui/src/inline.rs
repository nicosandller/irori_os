//! Text that is changed where it stands: a name with a small pencil beside it, which turns
//! into a field in the same place, with its own Save and Cancel right there.
//!
//! One field is open at a time on a page, and what's being typed is kept by the page rather
//! than by this, so a reading arriving underneath doesn't throw it away (ROADMAP D33).

use leptos::prelude::*;

use crate::icons::{Icon, icon};

/// `shown` is how it reads when it isn't being changed. `start` opens the field (and fills
/// `draft`), `save` is Enter or the tick, `cancel` is Escape or the cross.
#[allow(clippy::too_many_arguments)]
pub fn editable(
    label: String,
    placeholder: &'static str,
    editing: Signal<bool>,
    draft: RwSignal<String>,
    shown: impl Fn() -> AnyView + Send + Sync + 'static,
    start: impl Fn() + Clone + Send + Sync + 'static,
    save: impl Fn() + Clone + Send + Sync + 'static,
    cancel: impl Fn() + Clone + Send + Sync + 'static,
) -> AnyView {
    let field = NodeRef::<leptos::html::Input>::new();
    // Opening it is asking to type: the field takes the cursor, with what's there selected.
    Effect::new(move |_| {
        if editing.get() {
            request_animation_frame(move || {
                if let Some(field) = field.get_untracked() {
                    let _ = field.focus();
                    field.select();
                }
            });
        }
    });
    view! {
        <span class="editable" class:editing=move || editing.get()>
            {move || {
                if editing.get() {
                    let (save, cancel, escape) = (save.clone(), cancel.clone(), cancel.clone());
                    view! {
                        <form
                            class="inline-edit"
                            on:submit=move |event| {
                                event.prevent_default();
                                save();
                            }
                            on:keydown=move |event| {
                                if event.key() == "Escape" {
                                    escape();
                                }
                            }
                        >
                            <input
                                type="text"
                                node_ref=field
                                aria-label=label.clone()
                                placeholder=placeholder
                                prop:value=draft
                                on:input:target=move |event| draft.set(event.target().value())
                            />
                            <button type="submit" class="icon-button save" aria-label="Save" title="Save">
                                {icon(Icon::Check)}
                            </button>
                            <button
                                type="button"
                                class="icon-button"
                                aria-label="Cancel"
                                title="Cancel"
                                on:click=move |_| cancel()
                            >
                                {icon(Icon::Close)}
                            </button>
                        </form>
                    }
                    .into_any()
                } else {
                    let start = start.clone();
                    view! {
                        {shown()}
                        <button
                            type="button"
                            class="icon-button pencil"
                            aria-label=label.clone()
                            title=label.clone()
                            on:click=move |_| start()
                        >
                            {icon(Icon::Edit)}
                        </button>
                    }
                    .into_any()
                }
            }}
        </span>
    }
    .into_any()
}
