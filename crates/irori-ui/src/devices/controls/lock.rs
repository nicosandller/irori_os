use irori_types::{Entity, LockCapabilities, LockStatus, State};
use leptos::prelude::*;

use super::{UNKNOWN, tuck};
use crate::devices::Controls;

pub(crate) fn lock_words(state: LockStatus) -> &'static str {
    match state {
        LockStatus::Locked => "Locked",
        LockStatus::Unlocked => "Unlocked",
        LockStatus::Locking => "Locking",
        LockStatus::Unlocking => "Unlocking",
        LockStatus::Jammed => "Jammed",
        LockStatus::Open => "Open",
        LockStatus::Opening => "Opening",
    }
}

/// Which of a lock's buttons make sense right now: nobody locks a locked door. While the bolt
/// is moving there's nothing to press; jammed, or not saying, everything is offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Offered {
    pub lock: bool,
    pub unlock: bool,
    /// Pulling the latch, where the lock can.
    pub open: bool,
}

pub(crate) fn offered(can_open: bool, status: Option<LockStatus>) -> Offered {
    let (lock, unlock, open) = match status {
        Some(LockStatus::Locked) => (false, true, true),
        Some(LockStatus::Unlocked) => (true, false, true),
        Some(LockStatus::Open) => (true, false, false),
        Some(LockStatus::Locking | LockStatus::Unlocking | LockStatus::Opening) => {
            (false, false, false)
        }
        Some(LockStatus::Jammed) | None => (true, true, true),
    };
    Offered {
        lock,
        unlock,
        open: open && can_open,
    }
}

/// How the padlock beside the reading is drawn: shut, open, working at it, or stuck.
fn drawn(status: Option<LockStatus>) -> &'static str {
    match status {
        Some(LockStatus::Locked) => "shut",
        Some(LockStatus::Unlocked | LockStatus::Open) => "open",
        Some(LockStatus::Locking | LockStatus::Unlocking | LockStatus::Opening) => "moving",
        Some(LockStatus::Jammed) => "stuck",
        None => "unknown",
    }
}

/// A lock's row. Drawn once and kept, so the padlock's shackle lifts and drops as the lock
/// does, and the one button that makes sense takes the other's place.
///
/// Locking is sent at once. Unlocking and opening let someone in, so they ask first — and ask
/// for the code, where the lock wants one.
pub(crate) fn lock_control(
    entity: &Entity,
    capabilities: &LockCapabilities,
    state: Signal<Option<State>>,
    offline: Signal<bool>,
    controls: Controls,
) -> AnyView {
    let status = Memo::new(move |_| match state.get() {
        Some(State::Lock(lock)) => Some(lock.state),
        _ => None,
    });
    let can_open = capabilities.open;
    let can = Memo::new(move |_| offered(can_open, status.get()));
    let entity_id = entity.id.clone();
    let name = entity.name.to_string();
    let disable = {
        let entity_id = entity_id.clone();
        Signal::derive(move || {
            offline.get() || controls.busy.with(|busy| busy.contains(&entity_id))
        })
    };
    let asking: RwSignal<Option<(&'static str, &'static str)>> = RwSignal::new(None);
    let code = RwSignal::new(String::new());
    let needs_code = capabilities.requires_code;
    let code_format = capabilities.code_format.clone();
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
    let ask = |label: &'static str, action: &'static str| {
        view! {
            <button
                type="button"
                class="press"
                disabled=move || disable.get()
                on:click=move |_| asking.set(Some((label, action)))
            >
                {label}
            </button>
        }
        .into_any()
    };
    let lock = {
        let send = send.clone();
        view! {
            <button
                type="button"
                class="press"
                disabled=move || disable.get()
                on:click=move |_| send("lock")
            >
                "Lock"
            </button>
        }
        .into_any()
    };
    let confirm = move || {
        asking.get().map(|(label, action)| {
            let send = send.clone();
            let code_format = code_format.clone();
            view! {
                <crate::modal::Modal
                    title=format!("{label} {name}?")
                    on_close=Callback::new(move |()| {
                        code.set(String::new());
                        asking.set(None);
                    })
                >
                    <p>"This lets someone in. Irori sends it as soon as you confirm."</p>
                    {(needs_code || code_format.is_some()).then(|| view! {
                        <label class="lock-code">
                            "Code"
                            <input
                                type="password"
                                autocomplete="off"
                                pattern=code_format.clone()
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
            <span class="reading lock-reading" class:on=move || drawn(status.get()) == "open">
                <svg
                    class="padlock"
                    data-lock=move || drawn(status.get())
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.8"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                    aria-hidden="true"
                >
                    <path class="shackle" d="M8 11V7.500a4 4 0 0 1 8 0V11" />
                    <rect x="5" y="11" width="14" height="10" rx="2" />
                    <path d="M12 15v2.500" />
                </svg>
                {move || status.get().map_or(UNKNOWN, lock_words)}
            </span>
            <span class="cover-buttons">
                {tuck(lock, Signal::derive(move || can.get().lock))}
                {tuck(ask("Unlock", "unlock"), Signal::derive(move || can.get().unlock))}
                {can_open.then(|| tuck(ask("Open", "open"), Signal::derive(move || can.get().open)))}
            </span>
            {confirm}
        </>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use irori_types::LockStatus;

    use super::{Offered, drawn, offered};

    #[test]
    fn a_lock_offers_only_what_makes_sense_now() {
        let (yes, no) = (true, false);
        let can = |lock, unlock, open| Offered { lock, unlock, open };
        assert_eq!(offered(yes, Some(LockStatus::Locked)), can(no, yes, yes));
        assert_eq!(offered(yes, Some(LockStatus::Unlocked)), can(yes, no, yes));
        assert_eq!(offered(yes, Some(LockStatus::Open)), can(yes, no, no));
        // Nothing to press while the bolt is moving.
        assert_eq!(offered(yes, Some(LockStatus::Unlocking)), can(no, no, no));
        // Stuck, or silent: every way out is offered.
        assert_eq!(offered(yes, Some(LockStatus::Jammed)), can(yes, yes, yes));
        assert_eq!(offered(yes, None), can(yes, yes, yes));
        // One with no latch to pull never offers Open.
        assert_eq!(offered(no, Some(LockStatus::Locked)), can(no, yes, no));
    }

    #[test]
    fn the_padlock_is_drawn_as_the_lock_stands() {
        assert_eq!(drawn(Some(LockStatus::Locked)), "shut");
        assert_eq!(drawn(Some(LockStatus::Open)), "open");
        assert_eq!(drawn(Some(LockStatus::Locking)), "moving");
        assert_eq!(drawn(Some(LockStatus::Jammed)), "stuck");
    }
}
