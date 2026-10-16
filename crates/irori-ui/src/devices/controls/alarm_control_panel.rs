use irori_types::{AlarmCapabilities, AlarmStatus, ArmMode, CodeFormat, Entity, State};
use leptos::prelude::*;

use super::{UNKNOWN, tuck};
use crate::devices::Controls;

pub(crate) fn alarm_words(state: AlarmStatus) -> &'static str {
    match state {
        AlarmStatus::Disarmed => "Disarmed",
        AlarmStatus::ArmedHome => "Armed, home",
        AlarmStatus::ArmedAway => "Armed, away",
        AlarmStatus::ArmedNight => "Armed, night",
        AlarmStatus::ArmedVacation => "Armed, vacation",
        AlarmStatus::ArmedCustomBypass => "Armed, custom",
        AlarmStatus::Pending => "About to sound",
        AlarmStatus::Arming => "Arming",
        AlarmStatus::Disarming => "Disarming",
        AlarmStatus::Triggered => "Going off",
    }
}

/// What a panel's arm buttons say.
fn arm_label(mode: ArmMode) -> &'static str {
    match mode {
        ArmMode::Home => "Arm home",
        ArmMode::Away => "Arm away",
        ArmMode::Night => "Arm night",
        ArmMode::Vacation => "Arm vacation",
        ArmMode::CustomBypass => "Arm custom",
    }
}

/// The action a mode's button sends: the part of its service after the dot.
fn arm_action(mode: ArmMode) -> &'static str {
    mode.service().action()
}

/// Which of a panel's buttons make sense right now: nobody disarms a disarmed alarm. While it's
/// disarming there's nothing to press; not saying, everything is offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Offered {
    pub arm: bool,
    pub disarm: bool,
}

pub(crate) fn offered(status: Option<AlarmStatus>) -> Offered {
    let (arm, disarm) = match status {
        Some(AlarmStatus::Disarmed) => (true, false),
        Some(AlarmStatus::Disarming) => (false, false),
        // Armed, counting down either way, or going off: the one thing to do is stop it.
        Some(_) => (false, true),
        None => (true, true),
    };
    Offered { arm, disarm }
}

/// An alarm panel's row. Drawn once and kept, so the buttons that make sense take the others'
/// place as it arms and disarms.
///
/// Arming is sent at once, unless the panel wants its code for it. Disarming lets someone in,
/// so it asks first, and asks for the code where the panel takes one. Setting it off isn't
/// offered here: a flow can, a slip of the finger shouldn't.
pub(crate) fn alarm_control(
    entity: &Entity,
    capabilities: &AlarmCapabilities,
    state: Signal<Option<State>>,
    offline: Signal<bool>,
    controls: Controls,
) -> AnyView {
    let status = Memo::new(move |_| match state.get() {
        Some(State::AlarmControlPanel(panel)) => Some(panel.state),
        _ => None,
    });
    let can = Memo::new(move |_| offered(status.get()));
    let entity_id = entity.id.clone();
    let name = entity.name.to_string();
    let disable = {
        let entity_id = entity_id.clone();
        Signal::derive(move || {
            offline.get() || controls.busy.with(|busy| busy.contains(&entity_id))
        })
    };
    // What's being asked about: the button's words, its action, and whether it lets someone in.
    let asking: RwSignal<Option<(&'static str, &'static str, bool)>> = RwSignal::new(None);
    let code = RwSignal::new(String::new());
    let code_format = capabilities.code_format;
    let code_to_arm = code_format.is_some() && capabilities.code_arm_required;
    let code_to_disarm = code_format.is_some() && capabilities.code_disarm_required;
    let send = {
        let entity_id = entity_id.clone();
        move |action: &'static str| {
            let typed = code.get_untracked();
            let data = (!typed.is_empty()).then(|| serde_json::json!({ "code": typed }));
            controls.act.run((entity_id.clone(), action, data));
            code.set(String::new());
            asking.set(None);
        }
    };
    let arms = capabilities
        .arm_modes
        .iter()
        .map(|mode| {
            let (label, action) = (arm_label(*mode), arm_action(*mode));
            let send = send.clone();
            let button = view! {
                <button
                    type="button"
                    class="press"
                    disabled=move || disable.get()
                    on:click=move |_| {
                        if code_to_arm {
                            asking.set(Some((label, action, false)));
                        } else {
                            send(action);
                        }
                    }
                >
                    {label}
                </button>
            }
            .into_any();
            tuck(button, Signal::derive(move || can.get().arm))
        })
        .collect_view();
    let disarm = view! {
        <button
            type="button"
            class="press"
            disabled=move || disable.get()
            on:click=move |_| asking.set(Some(("Disarm", "alarm_disarm", true)))
        >
            "Disarm"
        </button>
    }
    .into_any();
    let confirm = move || {
        asking.get().map(|(label, action, lets_in)| {
            let send = send.clone();
            let needs_code = if lets_in { code_to_disarm } else { code_to_arm };
            view! {
                <crate::modal::Modal
                    title=format!("{label}: {name}?")
                    on_close=Callback::new(move |()| {
                        code.set(String::new());
                        asking.set(None);
                    })
                >
                    {lets_in.then(|| view! {
                        <p>"This turns the alarm off. Irori sends it as soon as you confirm."</p>
                    })}
                    {code_format.map(|format| view! {
                        <label class="lock-code">
                            "Code"
                            <input
                                type="password"
                                autocomplete="off"
                                inputmode=if format == CodeFormat::Number { "numeric" } else { "text" }
                                required=needs_code
                                prop:value=move || code.get()
                                on:input:target=move |ev| code.set(ev.target().value())
                            />
                        </label>
                    })}
                    <div class="ext-actions">
                        <button
                            type="button"
                            class="danger"
                            on:click=move |_| {
                                code.set(String::new());
                                asking.set(None);
                            }
                        >
                            "Cancel"
                        </button>
                        <button
                            type="button"
                            class="add solid"
                            disabled=move || needs_code && code.get().is_empty()
                            on:click=move |_| send(action)
                        >
                            {label}
                        </button>
                    </div>
                </crate::modal::Modal>
            }
        })
    };
    view! {
        <>
            <span
                class="reading"
                class:on=move || status.get().is_some_and(|status| status != AlarmStatus::Disarmed)
            >
                {move || status.get().map_or(UNKNOWN, alarm_words)}
            </span>
            <span class="cover-buttons">
                {arms}
                {tuck(disarm, Signal::derive(move || can.get().disarm))}
            </span>
            {confirm}
        </>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use irori_types::{AlarmStatus, ArmMode};

    use super::{Offered, arm_action, offered};

    #[test]
    fn a_panel_offers_only_what_makes_sense_now() {
        let can = |arm, disarm| Offered { arm, disarm };
        assert_eq!(offered(Some(AlarmStatus::Disarmed)), can(true, false));
        assert_eq!(offered(Some(AlarmStatus::ArmedAway)), can(false, true));
        // Counting down, or going off: stopping it is the one thing to do.
        assert_eq!(offered(Some(AlarmStatus::Arming)), can(false, true));
        assert_eq!(offered(Some(AlarmStatus::Triggered)), can(false, true));
        assert_eq!(offered(Some(AlarmStatus::Disarming)), can(false, false));
        assert_eq!(offered(None), can(true, true));
    }

    #[test]
    fn an_arm_button_sends_its_modes_own_service() {
        assert_eq!(arm_action(ArmMode::Home), "alarm_arm_home");
        assert_eq!(arm_action(ArmMode::CustomBypass), "alarm_arm_custom_bypass");
    }
}
