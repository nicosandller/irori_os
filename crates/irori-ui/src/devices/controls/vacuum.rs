use irori_types::{Entity, State, VacuumActivity, VacuumCapabilities};
use leptos::prelude::*;

use super::{UNKNOWN, tuck};
use crate::devices::Controls;

pub(crate) fn vacuum_words(activity: VacuumActivity) -> &'static str {
    match activity {
        VacuumActivity::Cleaning => "Cleaning",
        VacuumActivity::Docked => "Docked",
        VacuumActivity::Idle => "Idle",
        VacuumActivity::Paused => "Paused",
        VacuumActivity::Returning => "Going home",
        VacuumActivity::Error => "Stuck",
    }
}

/// Which of a vacuum's buttons make sense right now: nobody starts one that's cleaning, or
/// sends home one that's docked. Stuck, or not saying, everything it can do is offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Offered {
    pub start: bool,
    pub pause: bool,
    pub stop: bool,
    pub home: bool,
}

pub(crate) fn offered(activity: Option<VacuumActivity>) -> Offered {
    let (start, pause, stop, home) = match activity {
        Some(VacuumActivity::Cleaning) => (false, true, true, true),
        Some(VacuumActivity::Docked) => (true, false, false, false),
        Some(VacuumActivity::Idle) => (true, false, false, true),
        Some(VacuumActivity::Paused) => (true, false, true, true),
        Some(VacuumActivity::Returning) => (false, true, true, false),
        Some(VacuumActivity::Error) | None => (true, true, true, true),
    };
    Offered {
        start,
        pause,
        stop,
        home,
    }
}

/// A vacuum's row. Drawn once and kept, so the buttons that make sense take the others' place
/// as it goes about the house. Where it has fan speeds, they're a list beside them.
pub(crate) fn vacuum_control(
    entity: &Entity,
    capabilities: &VacuumCapabilities,
    state: Signal<Option<State>>,
    offline: Signal<bool>,
    controls: Controls,
) -> AnyView {
    let activity = Memo::new(move |_| match state.get() {
        Some(State::Vacuum(vacuum)) => Some(vacuum.activity),
        _ => None,
    });
    let fan_speed = Memo::new(move |_| match state.get() {
        Some(State::Vacuum(vacuum)) => vacuum.fan_speed,
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
    let caps = capabilities.clone();
    let speeds = (!caps.fan_speeds.is_empty()).then(|| {
        let entity_id = entity_id.clone();
        let name = entity.name.to_string();
        view! {
            <select
                class="select-control"
                aria-label=format!("{name} fan speed")
                disabled=move || disable.get()
                on:change:target=move |ev| {
                    controls.act.run((
                        entity_id.clone(),
                        "set_fan_speed",
                        Some(serde_json::json!({ "fan_speed": ev.target().value() })),
                    ));
                }
            >
                // Until it says which, none is shown as chosen.
                <option value="" disabled selected=move || fan_speed.get().is_none()>"Fan"</option>
                {caps.fan_speeds.iter().cloned().map(|speed| {
                    let (chosen, value) = (speed.clone(), speed.clone());
                    view! {
                        <option
                            value=value
                            selected=move || fan_speed.get().as_deref() == Some(chosen.as_str())
                        >
                            {speed}
                        </option>
                    }
                }).collect_view()}
            </select>
        }
    });
    view! {
        <>
            <span
                class="reading"
                class:on=move || matches!(
                    activity.get(),
                    Some(VacuumActivity::Cleaning | VacuumActivity::Returning)
                )
            >
                {move || activity.get().map_or(UNKNOWN, vacuum_words)}
            </span>
            <span class="cover-buttons">
                {caps.start.then(|| button("Start", "start", Signal::derive(move || can.get().start)))}
                {caps.pause.then(|| button("Pause", "pause", Signal::derive(move || can.get().pause)))}
                {caps.stop.then(|| button("Stop", "stop", Signal::derive(move || can.get().stop)))}
                {caps.return_home.then(|| {
                    button("Dock", "return_to_base", Signal::derive(move || can.get().home))
                })}
                {caps.locate.then(|| button("Find", "locate", Signal::stored(true)))}
                {speeds}
            </span>
        </>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use irori_types::VacuumActivity;

    use super::{Offered, offered};

    #[test]
    fn a_vacuum_offers_only_what_makes_sense_now() {
        let can = |start, pause, stop, home| Offered {
            start,
            pause,
            stop,
            home,
        };
        let (yes, no) = (true, false);
        assert_eq!(offered(Some(VacuumActivity::Docked)), can(yes, no, no, no));
        assert_eq!(
            offered(Some(VacuumActivity::Cleaning)),
            can(no, yes, yes, yes)
        );
        assert_eq!(
            offered(Some(VacuumActivity::Returning)),
            can(no, yes, yes, no)
        );
        assert_eq!(
            offered(Some(VacuumActivity::Paused)),
            can(yes, no, yes, yes)
        );
        assert_eq!(offered(None), can(yes, yes, yes, yes));
    }
}
