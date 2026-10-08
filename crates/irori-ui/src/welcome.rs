//! The welcome: the first time IroriOS is opened, it asks who you are and where the home is.
//!
//! A prompt and not a gate (`docs/specs/config.md` §2). Everything works without it, every step
//! has a way out, and "Not now" is remembered by Irori rather than by the browser, so it isn't
//! asked again from the next screen somebody opens. Settings holds the same two questions for
//! whenever they are wanted.
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
const STEPS: [&str; 4] = ["Welcome", "You", "Your home", "Done"];

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

    let name = RwSignal::new(String::new());
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
    // Not now: said to Irori, so no other screen asks again.
    let not_now = move |_| {
        spawn_local(async move {
            let _ = api::dismiss_welcome().await;
            close();
        });
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
        if let Some(why) = password_trouble(&secret, &again.get_untracked(), false) {
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

    let finish = move |_| {
        // Done is as good as "not now" for every screen after this one.
        spawn_local(async move {
            let _ = api::dismiss_welcome().await;
            close();
        });
    };

    let body = move || {
        match step.get() {
        0 => view! {
            <div class="welcome-step add-step welcome-hello">
                <span class="start-box welcome-mark" aria-hidden="true">
                    <span class="start-ember"></span>
                </span>
                <h1>"Welcome to IroriOS"</h1>
                <p class="welcome-lede">
                    "The hearth at the center of the home. Two questions before you start: \
                     who you are, and where the home is. Both can wait, and both are in \
                     Settings whenever you want them."
                </p>
                <div class="welcome-actions">
                    <button type="button" class="quiet-button" on:click=not_now>"Not now"</button>
                    <button
                        type="button"
                        class="welcome-next"
                        on:click=move |_| go(if has_owner() { 2 } else { 1 })
                    >
                        "Set it up"
                    </button>
                </div>
            </div>
        }
        .into_any(),
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
                    "A password, if you want one"
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
                    {move || {
                        if password.with(String::is_empty) {
                            "Without a password the home stays open: anyone who can reach \
                             IroriOS on your network can use it and change it. That is how it \
                             has worked until now, and you can set one later."
                        } else {
                            "With a password, IroriOS asks who is there before it shows \
                             anything. There is no way to recover a forgotten one from the \
                             page; it's reset from the files on the machine."
                        }
                    }}
                </p>
                {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
                <div class="welcome-actions">
                    <button type="button" class="quiet-button" on:click=move |_| go(2)>
                        "Skip"
                    </button>
                    <button type="submit" class="welcome-next" disabled=move || busy.get()>
                        "Continue"
                    </button>
                </div>
            </form>
        }
        .into_any(),
        2 => view! {
            <div class="welcome-step add-step welcome-wide">
                <h1>"Where is the home?"</h1>
                <p class="welcome-lede">
                    "Automations use it: a time of day needs the time zone, and sunrise and \
                     sunset need the place."
                </p>
                <PlacePicker draft=draft />
                {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
                <div class="welcome-actions">
                    <button type="button" class="quiet-button" on:click=move |_| go(3)>
                        "Skip"
                    </button>
                    <button
                        type="button"
                        class="welcome-next"
                        disabled=move || busy.get() || draft.with(HomeSettings::is_empty)
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
                        {move || match (made.get(), session.with(|s| s.as_ref().is_some_and(|s| s.locked))) {
                            (Some(_), true) => "You're the owner, and IroriOS asks who is there.",
                            (Some(_), false) => "You're the owner. The home is open, with no password.",
                            (None, _) if has_owner() => "The home already has an owner.",
                            (None, _) => "Nobody was set up. The home is open to whoever can reach it.",
                        }}
                    </li>
                    <li>
                        {move || match placed.get() {
                            Some(home) => format!("The home is at {}.", crate::place::summary(&home)),
                            None => "No place was set, so automations can't fire by the clock \
                                     or the sun yet."
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
/// The people are offered by name, because this is a home and not a bank: the person at the
/// door picks themselves and types their password.
#[component]
pub fn SignIn() -> impl IntoView {
    let crate::Session(session) = expect_context::<crate::Session>();
    let people = move || session.with(|s| s.as_ref().map(|s| s.people.clone()).unwrap_or_default());
    // Whoever is first, until somebody picks: a home with one person has nothing to pick.
    let who = RwSignal::new(None::<irori_types::UserId>);
    let chosen = move || {
        who.get()
            .or_else(|| people().first().map(|person| person.id.clone()))
    };
    let password = RwSignal::new(String::new());
    let trouble = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    // Flips on each wrong try, so the field shakes each time and not only the first.
    let wrong = RwSignal::new(0u32);

    let enter = move |event: ev::SubmitEvent| {
        event.prevent_default();
        let Some(user) = chosen() else {
            return;
        };
        if busy.get_untracked() {
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
                    <div class="signin-people" role="group" aria-label="Who you are">
                        <For each=people key=|person| person.id.clone() let:person>
                            {
                                let (id, picked) = (person.id.clone(), person.id.clone());
                                view! {
                                    <button
                                        type="button"
                                        class="signin-person"
                                        aria-pressed=move || (chosen().as_ref() == Some(&id)).to_string()
                                        on:click=move |_| {
                                            who.set(Some(picked.clone()));
                                            trouble.set(None);
                                        }
                                    >
                                        <span class="person-initial" aria-hidden="true">
                                            {crate::users::initial(person.name.as_str())}
                                        </span>
                                        {person.name.as_str().to_owned()}
                                    </button>
                                }
                            }
                        </For>
                    </div>
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
