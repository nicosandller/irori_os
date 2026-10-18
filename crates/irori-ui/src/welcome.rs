//! The welcome: the first time IroriOS is opened, it asks who you are and where the home is.
//!
//! It is how a home gets its owner. A home is reached from more than the room it is in, so the
//! owner has a password and the welcome doesn't end without one; until then the page is the
//! welcome. The time zone is asked next and is filled in already. Both are in Settings after.
//!
//! Four steps, moving the way the Add device window's steps do: the one being left slides
//! away and the next comes in from the side it's going to.

use irori_types::HomeSettings;
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::place::{Place, PlacePicker};
use crate::transition;
use crate::users::password_trouble;

/// The steps, in order. What each is called is what the dots along the bottom are read as.
const STEPS: [&str; 4] = ["Welcome", "You", "Time zone", "Done"];

/// Whether the welcome is on show. Set by the page as it boots, and by Settings to open it
/// again.
#[derive(Debug, Clone, Copy)]
pub struct Showing(pub RwSignal<bool>);

#[component]
pub fn Welcome() -> impl IntoView {
    let Showing(showing) = expect_context::<Showing>();
    let crate::Session(session) = expect_context::<crate::Session>();
    let Place(saved) = expect_context::<Place>();
    let crate::users::People(people) = expect_context::<crate::users::People>();

    let step = RwSignal::new(0usize);
    let go = move |to: usize| {
        let forward = to > step.get_untracked();
        transition::around(transition::step(forward), move || step.set(to));
    };
    let trouble = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);

    // A home from before passwords were needed may already have its owner: their name is
    // there to keep or change.
    let name = RwSignal::new(session.with_untracked(|session| {
        session
            .as_ref()
            .and_then(|session| session.user.as_ref())
            .map(|user| user.name.as_str().to_owned())
            .unwrap_or_default()
    }));
    let (password, again) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let draft = RwSignal::new(saved.get_untracked().unwrap_or_default());
    // What was set on the way, for the last step to say back.
    let made = RwSignal::new(None::<String>);
    let placed = RwSignal::new(None::<HomeSettings>);
    // Somebody is already the owner when the welcome is opened again from Settings.
    let has_owner = move || session.with(|s| s.as_ref().is_some_and(|s| s.setup.owner));

    let close = move || {
        showing.set(false);
        crate::users::refresh(people, session);
    };
    let you = move |event: ev::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }
        let typed = match crate::users::name_of(&name.get_untracked()) {
            Ok(name) => name,
            Err(why) => return trouble.set(Some(why)),
        };
        let secret = password.get_untracked();
        if let Some(why) = password_trouble(&secret, &again.get_untracked(), true) {
            return trouble.set(Some(why.to_owned()));
        }
        busy.set(true);
        trouble.set(None);
        spawn_local(async move {
            match api::set_up(&typed, &secret).await {
                Ok(now) => {
                    session.set(Some(now));
                    made.set(Some(typed.as_str().to_owned()));
                    go(2);
                }
                Err(why) => trouble.set(Some(why)),
            }
            busy.set(false);
        });
    };

    let home = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        trouble.set(None);
        let wanted = draft.get_untracked();
        spawn_local(async move {
            match api::save_place(&wanted).await {
                Ok(kept) => {
                    saved.set(Some(kept.clone()));
                    placed.set(Some(kept));
                    go(3);
                }
                Err(why) => trouble.set(Some(why)),
            }
            busy.set(false);
        });
    };

    let finish = move |_| close();

    let body = move || {
        match step.get() {
        0 => view! {
            <div class="welcome-step add-step welcome-hello">
                <span class="start-box welcome-mark" aria-hidden="true">
                    <span class="start-ember"></span>
                </span>
                <h1>"Welcome to IroriOS"</h1>
                <p class="welcome-lede">
                    "The hearth at the center of the home. Two things before you start: who \
                     you are, with a password that keeps the home yours, and what time it is \
                     there."
                </p>
                <div class="welcome-actions">
                    <button
                        type="button"
                        class="welcome-next"
                        on:click=move |_| go(if has_owner() { 2 } else { 1 })
                    >
                        "Set it up"
                    </button>
                    // A home that is being moved or rebuilt brings its past with it.
                    {move || (!has_owner()).then(|| view! {
                        <button type="button" class="welcome-later" on:click=move |_| go(4)>
                            "Restore from a backup"
                        </button>
                    })}
                </div>
            </div>
        }
        .into_any(),
        4 => view! { <RestoreFirst back=Callback::new(move |()| go(0)) /> }.into_any(),
        1 => view! {
            <form class="welcome-step add-step" on:submit=you>
                <h1>"Who are you?"</h1>
                <p class="welcome-lede">
                    "You'll be this home's owner: the one who can change how it's set up and \
                     let other people in."
                </p>
                <label class="settings-field">
                    "Your name"
                    <input
                        type="text"
                        autocomplete="name"
                        autofocus
                        prop:value=move || name.get()
                        on:input=move |event| name.set(event_target_value(&event))
                    />
                </label>
                <label class="settings-field">
                    "A password"
                    <input
                        type="password"
                        autocomplete="new-password"
                        prop:value=move || password.get()
                        on:input=move |event| password.set(event_target_value(&event))
                    />
                </label>
                // Folded away until there is a password to repeat.
                <div class="drawer" class:open=move || !password.with(String::is_empty)
                    inert=move || password.with(String::is_empty).then_some("")>
                    <div class="drawer-inner">
                        <label class="settings-field">
                            "Again, to be sure"
                            <input
                                type="password"
                                autocomplete="new-password"
                                prop:value=move || again.get()
                                on:input=move |event| again.set(event_target_value(&event))
                            />
                        </label>
                    </div>
                </div>
                <p class="muted small">
                    "IroriOS asks who is there before it shows anything, so the home is \
                     yours wherever it is reached from. At least 8 characters. A forgotten \
                     password can't be recovered from the page; it's reset from the files \
                     on the machine."
                </p>
                {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
                <div class="welcome-actions">
                    <button type="submit" class="welcome-next" disabled=move || busy.get()>
                        "Continue"
                    </button>
                </div>
            </form>
        }
        .into_any(),
        2 => view! {
            <div class="welcome-step add-step welcome-wide">
                <h1>"What time is it at home?"</h1>
                <p class="welcome-lede">
                    "Automations go by the home's time zone. Putting the home on the map is \
                     up to you: it is what sunrise and sunset are worked out from."
                </p>
                <PlacePicker draft=draft />
                {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
                <div class="welcome-actions">
                    // No Skip here: the zone is filled in from this browser, so going on costs
                    // nothing, and the map above it can simply be left alone.
                    <button
                        type="button"
                        class="welcome-next"
                        disabled=move || busy.get() || draft.with(|draft| draft.time_zone.is_none())
                        on:click=home
                    >
                        "Continue"
                    </button>
                </div>
            </div>
        }
        .into_any(),
        _ => view! {
            <div class="welcome-step add-step welcome-hello">
                <svg class="welcome-check" viewBox="0 0 48 48" aria-hidden="true">
                    <circle cx="24" cy="24" r="21" pathLength="1" />
                    <path d="M14 25 21.5 32.5 35 17" pathLength="1" />
                </svg>
                <h1>
                    {move || match made.get() {
                        Some(name) => format!("That's it, {name}"),
                        None => "That's it".to_owned(),
                    }}
                </h1>
                <ul class="welcome-said">
                    <li>
                        {move || match made.get() {
                            Some(_) => "You're the owner, and IroriOS asks who is there.",
                            None => "The home already has its owner.",
                        }}
                    </li>
                    <li>
                        {move || match placed.get() {
                            Some(home) => format!("The home is at {}.", crate::place::summary(&home)),
                            None => "No time zone was set, so automations can't fire by the \
                                     clock yet."
                                .to_owned(),
                        }}
                    </li>
                </ul>
                <p class="muted small">"Both are in Settings, under Users and Location and time zone."</p>
                <div class="welcome-actions">
                    <button type="button" class="welcome-next" on:click=finish>"Open IroriOS"</button>
                </div>
            </div>
        }
        .into_any(),
    }
    };

    view! {
        <div class="welcome" role="dialog" aria-modal="true" aria-label="Welcome to IroriOS">
            <div class="welcome-card">
                {body}
                <ol class="welcome-dots" aria-label="Steps">
                    {STEPS
                        .iter()
                        .enumerate()
                        .map(|(i, name)| view! {
                            <li
                                class:here=move || step.get() == i
                                class:past={move || step.get() > i}
                                aria-current=move || (step.get() == i).then_some("step")
                            >
                                <span class="visually-hidden">{*name}</span>
                            </li>
                        })
                        .collect_view()}
                </ol>
            </div>
        </div>
    }
}

/// The sign-in page: a home that asks who is there, and a browser that hasn't said.
///
/// A name and a password, both typed. The page doesn't say who lives here: a home may be
/// reached from further away than its own network.
#[component]
pub fn SignIn() -> impl IntoView {
    let crate::Session(session) = expect_context::<crate::Session>();
    let who = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let trouble = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    // Flips on each wrong try, so the field shakes each time and not only the first.
    let wrong = RwSignal::new(0u32);

    let enter = move |event: ev::SubmitEvent| {
        event.prevent_default();
        let user = who.get_untracked().trim().to_owned();
        if user.is_empty() || busy.get_untracked() {
            return;
        }
        busy.set(true);
        trouble.set(None);
        let typed = password.get_untracked();
        spawn_local(async move {
            match api::sign_in(&user, &typed).await {
                Ok(now) => session.set(Some(now)),
                Err(why) => {
                    trouble.set(Some(why));
                    password.set(String::new());
                    wrong.update(|wrong| *wrong += 1);
                }
            }
            let _ = busy.try_set(false);
        });
    };

    view! {
        <div class="welcome signin">
            <form class="welcome-card" on:submit=enter>
                <div class="welcome-step welcome-hello">
                    <span class="start-box welcome-mark" aria-hidden="true">
                        <span class="start-ember"></span>
                    </span>
                    <h1>"Who's there?"</h1>
                    <label class="settings-field signin-password">
                        "Your name"
                        <input
                            type="text"
                            autocomplete="username"
                            autocapitalize="words"
                            autofocus
                            prop:value=move || who.get()
                            on:input=move |event| who.set(event_target_value(&event))
                        />
                    </label>
                    <label
                        class="settings-field signin-password"
                        class:wrong-a=move || !wrong.get().is_multiple_of(2)
                        class:wrong-b={move || wrong.get() != 0 && wrong.get().is_multiple_of(2)}
                    >
                        "Password"
                        <input
                            type="password"
                            autocomplete="current-password"
                            autofocus
                            prop:value=move || password.get()
                            on:input=move |event| password.set(event_target_value(&event))
                        />
                    </label>
                    {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
                    <div class="welcome-actions">
                        <button type="submit" class="welcome-next" disabled=move || busy.get()>
                            {move || if busy.get() { "Checking…" } else { "Come in" }}
                        </button>
                    </div>
                </div>
            </form>
        </div>
    }
}

/// Instead of setting up: upload a backup and restore it. The home has no owner yet, so
/// this is open the way setting up is; once restored, the people in the backup are the ones
/// who sign in.
#[component]
fn RestoreFirst(back: Callback<()>) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let name = RwSignal::new(None::<String>);
    let key = RwSignal::new(String::new());
    let checked = RwSignal::new(None::<api::BackupVerified>);
    let trouble = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let restarting = RwSignal::new(false);
    let from = RwSignal::new(None::<String>);
    Effect::new(move |_| {
        let (Some(from), Some(health)) = (from.get(), live.health.get()) else {
            return;
        };
        if health.boot_id != from {
            let _ = window().location().reload();
        }
    });

    let picked = move |event: ev::Event| {
        use web_sys::wasm_bindgen::JsCast as _;
        let Some(input) = event
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
        else {
            return;
        };
        let Some(file) = input.files().and_then(|files| files.get(0)) else {
            return;
        };
        busy.set(true);
        trouble.set(None);
        checked.set(None);
        spawn_local(async move {
            match api::upload_backup(file, true).await {
                Ok(sent) => name.set(Some(sent)),
                Err(why) => trouble.set(Some(why)),
            }
            busy.set(false);
        });
    };
    let check = move |_| {
        let Some(sent) = name.get_untracked() else {
            return;
        };
        busy.set(true);
        trouble.set(None);
        let typed = key.get_untracked();
        spawn_local(async move {
            match api::setup_verify_backup(&sent, &typed).await {
                Ok(verified) => checked.set(Some(verified)),
                Err(why) => {
                    checked.set(None);
                    trouble.set(Some(why));
                }
            }
            busy.set(false);
        });
    };
    let restore = move |_| {
        let (Some(sent), Some(boot)) = (
            name.get_untracked(),
            live.health.get_untracked().map(|h| h.boot_id),
        ) else {
            return;
        };
        busy.set(true);
        trouble.set(None);
        let typed = key.get_untracked();
        spawn_local(async move {
            match api::setup_restore_backup(&sent, &typed).await {
                Ok(_) => {
                    restarting.set(true);
                    from.set(Some(boot));
                }
                Err(why) => {
                    trouble.set(Some(why));
                    busy.set(false);
                }
            }
        });
    };

    view! {
        <div class="welcome-step add-step">
            <h1>"Restore a backup"</h1>
            <p class="welcome-lede">
                "Upload the backup file from your old home. IroriOS verifies it, restores the home as \
                 it was, and restarts. You'll sign in with the users and passwords that were in the backup."
            </p>
            <label class="press backup-upload" class:disabled=move || busy.get() || restarting.get()>
                {move || match name.get() {
                    Some(sent) => format!("Uploaded: {sent}. Choose a different file…"),
                    None => "Choose backup file…".to_owned(),
                }}
                <input type="file" hidden accept=".gz,.enc,application/gzip"
                    disabled=move || busy.get() || restarting.get() on:change=picked />
            </label>
            {move || name.get().map(|_| view! {
                <label class="settings-field">
                    "Recovery key (if the backup is encrypted)"
                    <input type="text" autocomplete="off" spellcheck="false"
                        placeholder="XXXXX-XXXXX-XXXXX-XXXXX-XXXXX"
                        prop:value=move || key.get()
                        on:input=move |e| { key.set(event_target_value(&e)); checked.set(None); } />
                </label>
            })}
            {move || checked.get().map(|v| view! {
                <div class="backup-checked" role="status">
                    <p>
                        "Backup is valid. Made by Irori "{v.manifest.irori_version.clone()}" on "
                        {crate::backups::when(&v.manifest.created_at)}", "{v.files}" files."
                    </p>
                    {v.warnings.iter().map(|w| view! { <p class="muted small">"Note: "{w.clone()}</p> }).collect_view()}
                </div>
            })}
            {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
            <div class="welcome-actions">
                <button type="button" class="welcome-later" disabled=move || restarting.get()
                    on:click=move |_| back.run(())>"Back"</button>
                <button type="button" class="welcome-later"
                    disabled=move || busy.get() || name.get().is_none() on:click=check>
                    "Verify"
                </button>
                <button type="button" class="welcome-next"
                    disabled=move || busy.get() || name.get().is_none() on:click=restore>
                    {move || if restarting.get() { "Restarting…" } else { "Restore and restart" }}
                </button>
            </div>
        </div>
    }
}
