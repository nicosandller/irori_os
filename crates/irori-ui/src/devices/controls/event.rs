//! An event: what happened last, and when.

use irori_types::{EntityState, State};
use leptos::prelude::*;

/// An event: what happened last, and when. The time is what says it's news: "double" from a
/// minute ago and "double" from yesterday are different things.
pub(crate) fn happened(state: Option<&EntityState>) -> AnyView {
    let last = state.and_then(|state| match &state.state {
        Some(State::Event(event)) => Some((event.event_type.clone(), state.last_changed)),
        _ => None,
    });
    match last {
        Some((event_type, at)) => view! {
            <span class="reading" title=at.to_string()>{event_type}</span>
        }
        .into_any(),
        None => view! { <span class="reading">"Nothing yet"</span> }.into_any(),
    }
}
