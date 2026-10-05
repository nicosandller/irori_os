//! Covers and valves: both open and close, and some go to a position in between.

use irori_types::{Entity, OpenState};
use irori_types::{OpeningAbilities, OpeningState};
use leptos::prelude::*;

use super::{UNKNOWN, fill, tuck};
use crate::devices::Controls;

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

/// Which buttons make sense right now: nobody opens what is fully open or closes what is
/// closed, and Stop is only for something that is moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Offered {
    pub open: bool,
    pub stop: bool,
    pub close: bool,
}

pub(crate) fn offered(abilities: OpeningAbilities, current: Option<OpeningState>) -> Offered {
    let Some(current) = current else {
        // Not saying where it is: either way might be wanted.
        return Offered {
            open: true,
            stop: false,
            close: true,
        };
    };
    let moving = matches!(current.state, OpenState::Opening | OpenState::Closing);
    // "Open" covers everything that isn't shut; only all the way open has nowhere further to go.
    let fully_open =
        current.state == OpenState::Open && current.position.is_none_or(|position| position >= 100);
    Offered {
        open: !fully_open && current.state != OpenState::Opening,
        stop: moving && abilities.stop,
        close: !matches!(current.state, OpenState::Closed | OpenState::Closing),
    }
}

/// A cover's or a valve's row. Drawn once and kept: where it is arrives through `current`, the
/// buttons come and go in place, and a slider being dragged isn't drawn again under the finger.
pub(crate) fn opening_control(
    entity: &Entity,
    abilities: OpeningAbilities,
    current: Signal<Option<OpeningState>>,
    // `None` when it has no tilt; otherwise where the slats are, when it says.
    tilt: Option<Signal<Option<u8>>>,
    offline: Signal<bool>,
    controls: Controls,
) -> AnyView {
    let entity_id = entity.id.clone();
    let disable = {
        let entity_id = entity_id.clone();
        Signal::derive(move || {
            offline.get() || controls.busy.with(|busy| busy.contains(&entity_id))
        })
    };
    let can = Memo::new(move |_| offered(abilities, current.get()));
    let button = |label: &'static str, action: &'static str| {
        let entity_id = entity_id.clone();
        view! {
            <button
                type="button"
                class="press"
                disabled=move || disable.get()
                on:click=move |_| controls.act.run((entity_id.clone(), action, None))
            >
                {label}
            </button>
        }
        .into_any()
    };
    let slider =
        |label: &'static str, action: &'static str, field: &'static str, at: Signal<Option<u8>>| {
            let entity_id = entity_id.clone();
            // Where it says it is, and where the finger is while it's being dragged.
            let dragged = RwSignal::new(at.get_untracked().unwrap_or(0));
            Effect::new(move |_| dragged.set(at.get().unwrap_or(0)));
            view! {
                <label class="dim" title=label>
                    <span class="lv">{move || format!("{}%", dragged.get())}</span>
                    <input
                        type="range"
                        min="0"
                        max="100"
                        step="1"
                        aria-label=format!("{label} for {}", entity.name)
                        prop:value=move || dragged.get().to_string()
                        style:--fill=move || format!("{}%", fill(dragged.get(), 0, 100))
                        disabled=move || disable.get()
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
        slider(
            "Position",
            "set_position",
            "position",
            Signal::derive(move || current.get().and_then(|current| current.position)),
        )
    });
    let tilt = tilt.map(|at| slider("Tilt", "set_tilt", "tilt", at));
    let moving = move || {
        current
            .get()
            .is_some_and(|current| matches!(current.state, OpenState::Opening | OpenState::Closing))
    };
    view! {
        <>
            <span class="reading" class:moving=moving>
                {move || current.get().map_or_else(|| UNKNOWN.to_owned(), opening_words)}
            </span>
            <span class="cover-buttons">
                {tuck(button("Open", "open"), Signal::derive(move || can.get().open))}
                {abilities
                    .stop
                    .then(|| tuck(button("Stop", "stop"), Signal::derive(move || can.get().stop)))}
                {tuck(button("Close", "close"), Signal::derive(move || can.get().close))}
            </span>
            {(position.is_some() || tilt.is_some())
                .then(|| view! { <span class="light-controls">{position}{tilt}</span> })}
        </>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use irori_types::{OpenState, OpeningAbilities, OpeningState};

    use super::{Offered, offered};

    #[test]
    fn a_cover_offers_only_what_makes_sense_now() {
        let able = OpeningAbilities {
            position: true,
            stop: true,
        };
        let at = |state, position| Some(OpeningState { state, position });
        let can = |open, stop, close| Offered { open, stop, close };
        assert_eq!(
            offered(able, at(OpenState::Closed, Some(0))),
            can(true, false, false)
        );
        assert_eq!(
            offered(able, at(OpenState::Open, Some(100))),
            can(false, false, true)
        );
        // Half open can go either way.
        assert_eq!(
            offered(able, at(OpenState::Open, Some(40))),
            can(true, false, true)
        );
        // One that doesn't say how far is open, and that's all.
        assert_eq!(
            offered(able, at(OpenState::Open, None)),
            can(false, false, true)
        );
        // Moving: stop it, or send it back the other way.
        assert_eq!(
            offered(able, at(OpenState::Opening, Some(40))),
            can(false, true, true)
        );
        assert_eq!(
            offered(able, at(OpenState::Closing, Some(40))),
            can(true, true, false)
        );
        // One that can't be stopped never offers to.
        let plain = OpeningAbilities::default();
        assert!(!offered(plain, at(OpenState::Opening, None)).stop);
        assert_eq!(offered(able, None), can(true, false, true));
    }
}
