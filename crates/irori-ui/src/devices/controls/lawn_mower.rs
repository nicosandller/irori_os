use irori_types::{Entity, LawnMowerCapabilities, MowerActivity, State};
use leptos::prelude::*;

use super::{UNKNOWN, tuck};
use crate::devices::Controls;

pub(crate) fn mower_words(activity: MowerActivity) -> &'static str {
    match activity {
        MowerActivity::Mowing => "Mowing",
        MowerActivity::Docked => "Docked",
        MowerActivity::Paused => "Paused",
        MowerActivity::Returning => "Going home",
        MowerActivity::Error => "Stuck",
    }
}

/// Which of a mower's three buttons make sense right now, as (start, pause, dock). Stuck, or
/// not saying, everything it can do is offered.
pub(crate) fn offered(activity: Option<MowerActivity>) -> (bool, bool, bool) {
    match activity {
        Some(MowerActivity::Mowing) => (false, true, true),
        Some(MowerActivity::Docked) => (true, false, false),
        Some(MowerActivity::Paused) => (true, false, true),
        Some(MowerActivity::Returning) => (true, true, false),
        Some(MowerActivity::Error) | None => (true, true, true),
    }
}

/// A mower's row. Drawn once and kept, so the buttons that make sense take the others' place.
pub(crate) fn mower_control(
    entity: &Entity,
    capabilities: LawnMowerCapabilities,
    state: Signal<Option<State>>,
    offline: Signal<bool>,
    controls: Controls,
) -> AnyView {
    let activity = Memo::new(move |_| match state.get() {
        Some(State::LawnMower(mower)) => Some(mower.activity),
        _ => None,
    });
    let can = Memo::new(move |_| offered(activity.get()));
    let entity_id = entity.id.clone();
    let disable = {
        let entity_id = entity_id.clone();
        Signal::derive(move || {
            offline.get() || controls.busy.with(|busy| busy.contains(&entity_id))
        })
    };
    let button = |label: &'static str, action: &'static str, shown: Signal<bool>| {
        let entity_id = entity_id.clone();
        let button = view! {
            <button
                type="button"
                class="press"
                disabled=move || disable.get()
                on:click=move |_| controls.act.run((entity_id.clone(), action, None))
            >
                {label}
            </button>
        }
        .into_any();
        tuck(button, shown)
    };
    view! {
        <>
            <span
                class="reading"
                class:on=move || matches!(
                    activity.get(),
                    Some(MowerActivity::Mowing | MowerActivity::Returning)
                )
            >
                {move || activity.get().map_or(UNKNOWN, mower_words)}
            </span>
            <span class="cover-buttons">
                {capabilities.start_mowing.then(|| {
                    button("Mow", "start_mowing", Signal::derive(move || can.get().0))
                })}
                {capabilities.pause.then(|| button("Pause", "pause", Signal::derive(move || can.get().1)))}
                {capabilities.dock.then(|| button("Dock", "dock", Signal::derive(move || can.get().2)))}
            </span>
        </>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use irori_types::MowerActivity;

    use super::offered;

    #[test]
    fn a_mower_offers_only_what_makes_sense_now() {
        assert_eq!(offered(Some(MowerActivity::Docked)), (true, false, false));
        assert_eq!(offered(Some(MowerActivity::Mowing)), (false, true, true));
        assert_eq!(offered(Some(MowerActivity::Paused)), (true, false, true));
        assert_eq!(offered(None), (true, true, true));
    }
}
