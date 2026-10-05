//! A row that folds open: a heading that says what's inside and how it stands, and a drawer
//! that rolls down under it.
//!
//! One shape for everything on the page that opens in place — a group of devices, a section of
//! Settings, a floor — so they all open the same way: the chevron turns, and the drawer grows
//! from nothing to exactly its content's height (`.drawer`, as an entity's history does).
//!
//! The drawer's content is always there, folded or not, so it can roll both ways and so
//! anything being typed in it survives being folded away; while folded it is `inert`, so nobody
//! tabs into what they can't see.

use leptos::prelude::*;

/// What a fold's heading says.
pub struct Head {
    /// An icon before the title, if this kind of row has one.
    pub icon: Option<AnyView>,
    pub title: AnyView,
    /// How what's inside stands, after the title and quieter: a count, a version, a model.
    pub state: Option<AnyView>,
    /// Buttons that act on the row itself. Beside the heading's button rather than in it: a
    /// button can't hold another.
    pub actions: Option<AnyView>,
}

/// `open` is read whenever the row paints; `toggle` is the heading being clicked.
pub fn fold(
    id: Option<String>,
    head: Head,
    open: Signal<bool>,
    toggle: impl Fn() + 'static,
    body: AnyView,
) -> AnyView {
    view! {
        <div class="fold-row" class:open=move || open.get() id=id>
            <div class="fold-head">
                <button
                    type="button"
                    class="fold-toggle"
                    aria-expanded=move || open.get().to_string()
                    on:click=move |_| toggle()
                >
                    <span class="chevron" aria-hidden="true"></span>
                    {head.icon}
                    <span class="fold-title">{head.title}</span>
                    {head.state.map(|state| view! { <span class="fold-state">{state}</span> })}
                </button>
                {head.actions.map(|actions| view! { <div class="fold-actions">{actions}</div> })}
            </div>
            <div class="drawer" class:open=move || open.get() inert=move || (!open.get()).then_some("")>
                <div class="drawer-inner">{body}</div>
            </div>
        </div>
    }
    .into_any()
}
