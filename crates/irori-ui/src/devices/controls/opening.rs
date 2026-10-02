//! Something that opens and closes, a cover or a valve: where it is, buttons to open, stop
//! and close it, and sliders for its position and, on a cover, its tilt.

use irori_types::{Entity, OpenState};
use irori_types::{OpeningAbilities, OpeningState};
use leptos::prelude::*;

use super::{UNKNOWN, fill};
use crate::devices::Controls;

/// Where a cover or a valve is, in words: "Open · 40%", "Closing".
pub(crate) fn opening_words(opening: OpeningState) -> String {
    let state = match opening.state {
        OpenState::Open => "Open",
        OpenState::Opening => "Opening",
        OpenState::Closed => "Closed",
        OpenState::Closing => "Closing",
    };
    match opening.position {
        Some(position) if opening.state == OpenState::Open => format!("{state} · {position}%"),
        _ => state.to_owned(),
    }
}

/// Something that opens and closes, a cover or a valve: where it is, buttons to open, stop and
/// close it, and sliders for its position and tilt when it has them. Like a light's sliders,
/// only letting go sends anything.
///
/// `tilt` is `Some` for a cover whose slats tilt, with how far they are, when it's said.
pub(crate) fn opening_control(
    entity: &Entity,
    abilities: OpeningAbilities,
    current: Option<OpeningState>,
    tilt: Option<Option<u8>>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let entity_id = entity.id.clone();
    let disable = {
        let entity_id = entity_id.clone();
        move || offline || controls.busy.get().contains(&entity_id)
    };
    let button = |label: &'static str, action: &'static str| {
        let entity_id = entity_id.clone();
        let disable = disable.clone();
        view! {
            <button
                type="button"
                class="press"
                disabled=disable
                on:click=move |_| controls.act.run((entity_id.clone(), action, None))
            >
                {label}
            </button>
        }
    };
    let slider = |label: &'static str, action: &'static str, field: &'static str, at: u8| {
        let entity_id = entity_id.clone();
        let disable = disable.clone();
        let dragged = RwSignal::new(at);
        view! {
            <label class="dim" title=label>
                <span class="lv">{move || format!("{}%", dragged.get())}</span>
                <input
                    type="range"
                    min="0"
                    max="100"
                    step="1"
                    aria-label=format!("{label} for {}", entity.name)
                    prop:value=at.to_string()
                    style:--fill=move || format!("{}%", fill(dragged.get(), 0, 100))
                    disabled=disable
                    on:input:target=move |ev| {
                        if let Ok(value) = ev.target().value().parse::<u8>() {
                            dragged.set(value);
                        }
                    }
                    on:change:target=move |ev| {
                        if let Ok(value) = ev.target().value().parse::<u8>() {
                            controls.act.run((
                                entity_id.clone(),
                                action,
                                Some(serde_json::json!({ field: value })),
                            ));
                        }
                    }
                />
            </label>
        }
    };
    let position = abilities.position.then(|| {
        let at = current.and_then(|c| c.position).unwrap_or(0);
        slider("Position", "set_position", "position", at)
    });
    let tilt = tilt.map(|at| slider("Tilt", "set_tilt", "tilt", at.unwrap_or(0)));
    view! {
        <>
            <span class="reading">
                {current.map_or_else(|| UNKNOWN.to_owned(), opening_words)}
            </span>
            <span class="cover-buttons">
                {button("Open", "open")}
                {abilities.stop.then(|| button("Stop", "stop"))}
                {button("Close", "close")}
            </span>
            {(position.is_some() || tilt.is_some())
                .then(|| view! { <span class="light-controls">{position}{tilt}</span> })}
        </>
    }
    .into_any()
}
