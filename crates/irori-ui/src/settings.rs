//! The Settings page: what IroriOS is and what it looks after, over one table of the things
//! there are to set, each a row that says how it stands and opens in place.
//!
//! In the order someone setting a home up is likely to want them: the instance and the machine
//! under it, how the page moves, whether a model answers, the floors and areas that say what's
//! where, the people allowed in (none yet), and what Irori has been saying. Every row starts
//! folded; what it says beside its name is usually all that was wanted.

use std::collections::BTreeSet;

use irori_types::Name;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_location;

use crate::api;
use crate::fold::{Head, fold};
use crate::icons::{Icon, icon};

/// The rows, by the id each goes by in the address (`/settings#assistant`).
const SECTIONS: [&str; 6] = [
    "system",
    "appearance",
    "assistant",
    "floors-and-areas",
    "users",
    "logs",
];

#[component]
pub fn Settings() -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let crate::Motion(motion) = expect_context::<crate::Motion>();
    let trouble = RwSignal::new(None::<String>);
    // Which rows are open. Nothing remembers this: the page opens folded every time.
    let open = RwSignal::new(BTreeSet::<&'static str>::new());

    // A row named in the address opens and comes into view: Ask sends people to
    // `#assistant` until a model is ready.
    let location = use_location();
    Effect::new(move |_| {
        let hash = location.hash.get();
        let wanted = hash.trim_start_matches('#');
        let Some(id) = SECTIONS.into_iter().find(|id| *id == wanted) else {
            return;
        };
        open.update(|open| {
            open.insert(id);
        });
        request_animation_frame(move || {
            if let Some(row) = document().get_element_by_id(id) {
                row.scroll_into_view();
            }
        });
    });

    // Whether a restart is under way. The button stays "Restarting…" until the core answers with a
    // different instance — the new boot's — which is how "it's back" is known. Anything about
    // uptime would be guesswork: a process that happened to start a minute before you pressed
    // looks exactly like one that restarted a minute later, and a slow restart could pass any
    // freshness bound. So the core says which boot it is (`/api/health`'s boot_id), and the
    // effect below lets the button go again the moment the boot changes. Nothing here needs to
    // wait for the POST itself: the server answers Acceptance before it actually goes.
    let restarting = RwSignal::new(false);
    // The boot the restart set off from: the boot_id the health showed when the button was
    // pressed. None — not armed.
    let restart_from = RwSignal::new(None::<String>);
    Effect::new(move |_| {
        let Some(from) = restart_from.get() else {
            return;
        };
        let Some(health) = live.health.get() else {
            return;
        };
        // The instance we set off from is gone once the core answers with a different one. The
        // old instance's own answers keep its own boot_id, so they can't clear the button early.
        if health.boot_id != from {
            restarting.set(false);
            restart_from.set(None);
        }
    });
    let restart = move || {
        if restarting.get_untracked() {
            return;
        }
        // Only arm once we know which instance we're leaving. An empty baseline is no baseline:
        // a health answer still in flight from the old instance would then look like a boot
        // change from empty and clear the button before the restart had happened. (The button is
        // also disabled until the first health lands, for the same reason.)
        let Some(boot_id) = live.health.get_untracked().map(|health| health.boot_id) else {
            return;
        };
        if !window()
            .confirm_with_message(
                "Restart Irori? It stays where it runs while it starts again (same container, \
                 same service) — the page just goes quiet for a few seconds, and every device \
                 reconnects.",
            )
            .unwrap_or(false)
        {
            return;
        }
        restarting.set(true);
        restart_from.set(Some(boot_id));
        spawn_local(async move {
            match api::restart().await {
                // The new instance is coming up; the page notices it on its own.
                api::RestartSent::Accepted => {}
                // The living server answered no — 403, 429... — so nothing is restarting: give
                // the button back and say why.
                api::RestartSent::Refused(why) => {
                    restarting.set(false);
                    restart_from.set(None);
                    trouble.set(Some(why));
                }
                // No answer at all. It may be the restart under way (the old server drained as
                // its answer was on its way), so the baseline stays armed and clears only when a
                // different boot answers — a transport failure must not re-arm the button before
                // the new boot is verified.
                api::RestartSent::Lost => {}
            }
        });
    };

    // How much Irori has said, for the Logs row while it's folded: asked once, as the page
    // opens. Opened, the log keeps itself up to date.
    let said = RwSignal::new(None::<(usize, usize, usize)>);
    spawn_local(async move {
        if let Ok(lines) = api::fetch_system_log().await {
            said.set(Some(crate::log_window::tally(&lines)));
        }
    });

    // The home's arrangement in three numbers, and nothing that changes with a reading.
    let arrangement = Memo::new(move |_| {
        live.home.with(|home| {
            crate::places::summary(
                home.floors.len(),
                home.areas.len(),
                home.devices
                    .iter()
                    .filter(|device| device.area_id.is_none())
                    .count(),
            )
        })
    });

    let is_open =
        move |id: &'static str| Signal::derive(move || open.with(|open| open.contains(id)));
    let row =
        move |id: &'static str, kind: Icon, title: &'static str, state: AnyView, body: AnyView| {
            fold(
                Some(id.to_owned()),
                Head {
                    icon: Some(icon(kind)),
                    title: title.into_any(),
                    state: Some(state),
                    actions: None,
                },
                is_open(id),
                move || {
                    open.update(|open| {
                        if !open.remove(id) {
                            open.insert(id);
                        }
                    })
                },
                body,
            )
        };

    view! {
        // The page's heading is the instance itself: its name and mark, and what it holds.
        <crate::start::Hero>
            // The same Ask as a device's page has, about Irori itself: a conversation of its
            // own, told what these rows hold. Before a model is ready it opens the Assistant
            // row below, which is on this page.
            <crate::assistant::Ask
                scope="settings".to_owned()
                title="Irori's settings".to_owned()
            />
            <button
                type="button"
                // Disabled while a restart is under way, and until the first health says which
                // instance this is — arming without one would let the old instance's own
                // answer clear the button before the restart happened.
                disabled=move || restarting.get() || live.health.get().is_none()
                on:click=move |_| restart()
            >
                {move || if restarting.get() { "Restarting…" } else { "Restart" }}
            </button>
        </crate::start::Hero>

        {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}

        <div class="settings-table">
            {row(
                "system",
                Icon::System,
                "System",
                (move || live.health.with(|health| crate::machine::state(health.as_ref())))
                    .into_any(),
                view! { <crate::machine::Panel open=is_open("system") /> }.into_any(),
            )}
            {row(
                "appearance",
                Icon::Appearance,
                "Appearance",
                (move || if motion.get() { "motion on" } else { "motion off" }).into_any(),
                view! {
                    <div class="setting">
                        <div class="setting-words">
                            <span class="setting-name">"Motion"</span>
                            <p class="muted small">
                                "Switches that spring across, rows that roll open, and the Live \
                                 dot breathing while Irori answers. Remembered by this browser, \
                                 and always off when the system is set to reduce motion."
                            </p>
                        </div>
                        <button
                            type="button"
                            class="toggle"
                            aria-label="Motion"
                            aria-pressed=move || motion.get().to_string()
                            on:click=move |_| motion.update(|on| *on = !*on)
                        >
                            <span class="knob"></span>
                        </button>
                    </div>
                }
                .into_any(),
            )}
            {row(
                "assistant",
                Icon::Assistant,
                "Assistant",
                crate::assistant::state(),
                view! { <crate::assistant::Section /> }.into_any(),
            )}
            {row(
                "floors-and-areas",
                Icon::Places,
                "Floors and areas",
                (move || arrangement.get()).into_any(),
                view! { <crate::places::Section /> }.into_any(),
            )}
            {row(
                "users",
                Icon::Users,
                "Users",
                "none yet".into_any(),
                view! {
                    <p class="muted setting-note">
                        "No users yet — and nothing to sign in with. IroriOS is for the person in "
                        "the room with it: anyone who can reach it is looking after the home. "
                        "That changes before it runs in anyone else's home (ROADMAP M1.6)."
                    </p>
                }
                .into_any(),
            )}
            {row(
                "logs",
                Icon::Logs,
                "Logs",
                (move || said.get().map(|(lines, warnings, errors)| {
                    view! {
                        {log_lines(lines)}
                        {(errors > 0).then(|| view! {
                            " · "<span class="set">{log_count(errors, "error")}</span>
                        })}
                        {(errors == 0 && warnings > 0)
                            .then(|| format!(" · {}", log_count(warnings, "warning")))}
                    }
                }))
                .into_any(),
                view! {
                    <crate::log_window::LogView
                        source=crate::log_window::Source::System
                        watching=is_open("logs")
                    />
                }
                .into_any(),
            )}
        </div>
    }
}

fn log_lines(lines: usize) -> String {
    log_count(lines, "line")
}

fn log_count(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

/// Reads what was typed as a name, saying what's wrong with it rather than failing silently.
pub fn named(typed: String, trouble: RwSignal<Option<String>>) -> Option<Name> {
    match crate::places::name_of(&typed) {
        Ok(name) => Some(name),
        Err(why) => {
            trouble.set(Some(why));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_the_log_holds_reads_as_a_person_would_say_it() {
        assert_eq!(log_lines(400), "400 lines");
        assert_eq!(log_count(1, "error"), "1 error");
        assert_eq!(log_count(2, "warning"), "2 warnings");
    }
}
