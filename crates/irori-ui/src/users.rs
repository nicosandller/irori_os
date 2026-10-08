//! The Users row of Settings: who is allowed in, and what each may do.
//!
//! Two kinds of person. An owner runs the home; a user sees everything and controls devices.
//! A home with one person and no password is open, as Irori always was; the first password is
//! what makes it ask who is there, and everyone added after that has one of their own.

use irori_types::{Name, Role, UserId};
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{self, UserRow};
use crate::icons::{Icon, icon};

/// How the row stands while it's folded.
pub fn summary(users: &[UserRow]) -> String {
    let locked = users.iter().any(|user| user.has_password);
    match users {
        [] => "nobody yet".to_owned(),
        [only] if !locked => format!("{} · no password", only.name.as_str()),
        [only] => only.name.as_str().to_owned(),
        many => {
            let owners = many.iter().filter(|user| user.role == Role::Owner).count();
            format!(
                "{} people · {owners} owner{}",
                many.len(),
                if owners == 1 { "" } else { "s" }
            )
        }
    }
}

/// What's wrong with a new password as it stands, if anything. `needed` is whether one has to
/// be given at all.
pub fn password_trouble(password: &str, again: &str, needed: bool) -> Option<&'static str> {
    if password.is_empty() && again.is_empty() {
        return needed.then_some("A password is needed here.");
    }
    if password.chars().count() < irori_types::PASSWORD_MIN_LEN {
        return Some("A password needs at least 8 characters.");
    }
    (password != again).then_some("The two passwords aren't the same.")
}

/// The first letter of a name, for the circle before it.
pub fn initial(name: &str) -> String {
    name.trim()
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".to_owned())
}

/// Everyone, as last fetched. Kept above the row so its folded state can say how it stands.
#[derive(Debug, Clone, Copy)]
pub struct People(pub RwSignal<Vec<UserRow>>);

/// Asks again who is there and who this browser is: a password set or taken away changes both.
pub fn refresh(people: RwSignal<Vec<UserRow>>, session: RwSignal<Option<api::Session>>) {
    spawn_local(async move {
        if let Ok(users) = api::fetch_users().await {
            let _ = people.try_set(users);
        }
        if let Ok(now) = api::fetch_session().await {
            let _ = session.try_set(Some(now));
        }
    });
}

/// Two password fields and what's wrong with them. `password` holds what was typed.
fn password_fields(
    password: RwSignal<String>,
    again: RwSignal<String>,
    label: &'static str,
) -> impl IntoView {
    view! {
        <label class="settings-field">
            {label}
            <input
                type="password"
                autocomplete="new-password"
                prop:value=move || password.get()
                on:input=move |event| password.set(event_target_value(&event))
            />
        </label>
        <label class="settings-field">
            "Again, to be sure"
            <input
                type="password"
                autocomplete="new-password"
                prop:value=move || again.get()
                on:input=move |event| again.set(event_target_value(&event))
            />
        </label>
    }
}

#[component]
pub fn Section() -> impl IntoView {
    let People(people) = expect_context::<People>();
    let crate::Session(session) = expect_context::<crate::Session>();
    let owner = move || session.with(|session| session.as_ref().is_none_or(|s| s.owner));
    let me = move || {
        session.with(|session| {
            session
                .as_ref()
                .and_then(|session| session.user.as_ref().map(|user| user.id.clone()))
        })
    };
    let locked = move || people.with(|people| people.iter().any(|user| user.has_password));
    let trouble = RwSignal::new(None::<String>);
    // Whose password form is open, and who is being asked about before they're removed.
    let keying = RwSignal::new(None::<UserId>);
    let asking = RwSignal::new(None::<UserId>);
    let (password, again) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    // The password somebody has now, typed to change their own.
    let current = RwSignal::new(String::new());
    let adding = RwSignal::new(false);
    let new_name = RwSignal::new(String::new());
    let new_role = RwSignal::new(Role::User);

    let after = move |result: Result<Vec<UserRow>, String>| match result {
        Ok(users) => {
            trouble.set(None);
            people.set(users);
            keying.set(None);
            asking.set(None);
            adding.set(false);
            password.set(String::new());
            again.set(String::new());
            current.set(String::new());
            new_name.set(String::new());
            refresh(people, session);
        }
        Err(why) => trouble.set(Some(why)),
    };

    let set_password = move |id: UserId, password: String| {
        let had = current.get_untracked();
        spawn_local(async move {
            let mut edit = serde_json::json!({ "password": password });
            if !had.is_empty() {
                edit["current_password"] = had.into();
            }
            after(api::edit_user(&id, &edit).await);
        });
    };
    let set_role = move |id: UserId, role: Role| {
        spawn_local(async move {
            let edit = serde_json::json!({ "role": role });
            after(api::edit_user(&id, &edit).await);
        });
    };
    let remove = move |id: UserId| {
        spawn_local(async move { after(api::remove_user(&id).await) });
    };
    let add = move |event: ev::SubmitEvent| {
        event.prevent_default();
        let Some(name) = crate::settings::named(new_name.get_untracked(), trouble) else {
            return;
        };
        let first = people.with_untracked(Vec::is_empty);
        let typed = password.get_untracked();
        if let Some(why) = password_trouble(&typed, &again.get_untracked(), !first) {
            trouble.set(Some(why.to_owned()));
            return;
        }
        let role = new_role.get_untracked();
        spawn_local(async move {
            if first {
                // The first person is the owner, and setting them up is what the welcome does.
                match api::set_up(&name, &typed).await {
                    Ok(now) => {
                        session.set(Some(now));
                        after(api::fetch_users().await);
                    }
                    Err(why) => trouble.set(Some(why)),
                }
            } else {
                after(api::add_user(&name, role, &typed).await);
            }
        });
    };
    let sign_out = move |_| {
        spawn_local(async move {
            let _ = api::sign_out().await;
            if let Ok(now) = api::fetch_session().await {
                session.set(Some(now));
            }
        });
    };

    let row = move |user: UserRow| {
        let id = user.id.clone();
        let mine = me().as_ref() == Some(&id);
        let may_key = owner() || mine;
        let (key_id, ask_id, role_id, gone_id, save_id) =
            (id.clone(), id.clone(), id.clone(), id.clone(), id.clone());
        let (keying_here, asking_here) = (id.clone(), id.clone());
        let has_password = user.has_password;
        let role = user.role;
        view! {
            <li class="person">
                <span class="person-initial" aria-hidden="true">{initial(user.name.as_str())}</span>
                <span class="person-words">
                    <span class="person-name">
                        {user.name.as_str().to_owned()}
                        {mine.then(|| view! { <span class="muted small">" · you"</span> })}
                    </span>
                    <span class="muted small">
                        {if has_password { "has a password" } else { "no password" }}
                    </span>
                </span>
                {if owner() {
                    crate::choices::choices(
                        "What they may do",
                        vec![
                            ("owner".to_owned(), "Owner".to_owned()),
                            ("user".to_owned(), "User".to_owned()),
                        ],
                        move || Some(role.label().to_owned()),
                        || false,
                        move |picked| {
                            let role = if picked == "owner" { Role::Owner } else { Role::User };
                            set_role(role_id.clone(), role);
                        },
                    )
                } else {
                    view! { <span class="person-role">{role.label()}</span> }.into_any()
                }}
                {may_key.then(|| view! {
                    <button
                        type="button"
                        class="press"
                        aria-expanded=move || (keying.get().as_ref() == Some(&keying_here)).to_string()
                        on:click=move |_| {
                            password.set(String::new());
                            again.set(String::new());
                            current.set(String::new());
                            trouble.set(None);
                            keying.update(|open| {
                                *open = (open.as_ref() != Some(&key_id)).then(|| key_id.clone());
                            });
                        }
                    >
                        {if has_password { "Change password" } else { "Set a password" }}
                    </button>
                })}
                {(owner() && !mine).then(|| view! {
                    // Asks before it does anything: the button widens into the question.
                    <button
                        type="button"
                        class="icon-button delete"
                        class:asking=move || asking.get().as_ref() == Some(&asking_here)
                        aria-label=format!("Remove {}", user.name.as_str())
                        on:click=move |_| {
                            if asking.get_untracked().as_ref() == Some(&ask_id) {
                                remove(gone_id.clone());
                            } else {
                                asking.set(Some(ask_id.clone()));
                            }
                        }
                    >
                        {icon(Icon::Remove)}
                        <span class="asking-words">"Remove?"</span>
                    </button>
                })}
                <div
                    class="drawer person-key"
                    class:open={
                        let id = id.clone();
                        move || keying.get().as_ref() == Some(&id)
                    }
                    inert={
                        let id = id.clone();
                        move || (keying.get().as_ref() != Some(&id)).then_some("")
                    }
                >
                    <form
                        class="drawer-inner person-form"
                        on:submit=move |event: ev::SubmitEvent| {
                            event.prevent_default();
                            let typed = password.get_untracked();
                            match password_trouble(&typed, &again.get_untracked(), true) {
                                Some(why) => trouble.set(Some(why.to_owned())),
                                None => set_password(save_id.clone(), typed),
                            }
                        }
                    >
                        // Your own is changed with the one you have. An owner resetting
                        // somebody else's doesn't know theirs, and isn't asked.
                        {(mine && has_password).then(|| view! {
                            <label class="settings-field">
                                "Your current password"
                                <input
                                    type="password"
                                    autocomplete="current-password"
                                    prop:value=move || current.get()
                                    on:input=move |event| current.set(event_target_value(&event))
                                />
                            </label>
                        })}
                        {password_fields(password, again, "New password")}
                        <div class="settings-form-actions">
                            <button type="button" on:click=move |_| keying.set(None)>"Cancel"</button>
                            <button type="submit" class="add">"Save password"</button>
                        </div>
                    </form>
                </div>
            </li>
        }
    };

    view! {
        <p class="muted setting-note">
            {move || {
                if people.with(Vec::is_empty) {
                    "Nobody has been set up, so anyone who can reach IroriOS is looking after \
                     the home. Say who you are to make it yours."
                } else if !locked() {
                    "This home is open: anyone who can reach IroriOS can use it and change it. \
                     Setting a password is what makes it ask who is there."
                } else {
                    "IroriOS asks who is there. An owner runs the home; a user sees everything \
                     and controls devices, and changes nothing else."
                }
            }}
        </p>
        <ul class="people">
            // Keyed by everything a row shows, so a changed row is drawn again and an
            // unchanged one keeps the form somebody is typing in.
            <For
                each=move || people.get()
                key=|user| (user.id.clone(), user.name.clone(), user.role, user.has_password)
                children=row
            />
        </ul>
        {move || owner().then(|| {
            let first = people.with(Vec::is_empty);
            let can_add = first || locked();
            view! {
                <div class="namer" class:open=move || adding.get()>
                    <button
                        type="button"
                        class="namer-call"
                        aria-expanded=move || adding.get().to_string()
                        disabled=!can_add
                        on:click=move |_| {
                            trouble.set(None);
                            password.set(String::new());
                            again.set(String::new());
                            adding.update(|open| *open = !*open);
                        }
                    >
                        <span class="namer-plus">{icon(Icon::Add)}</span>
                        {if first { "Set up the owner" } else { "Add a person" }}
                    </button>
                    {(!can_add).then(|| view! {
                        <p class="muted small">
                            "Set a password for yourself first. With more than one person, \
                             IroriOS has to ask who is there."
                        </p>
                    })}
                    <div class="drawer" class:open=move || adding.get()
                        inert=move || (!adding.get()).then_some("")>
                        <form class="drawer-inner person-form" on:submit=add>
                            <label class="settings-field">
                                "Name"
                                <input
                                    type="text"
                                    autocomplete="off"
                                    prop:value=move || new_name.get()
                                    on:input=move |event| new_name.set(event_target_value(&event))
                                />
                            </label>
                            {(!first).then(|| view! {
                                <div class="settings-field">
                                    <span>"What they may do"</span>
                                    {crate::choices::choices(
                                        "What they may do",
                                        vec![
                                            ("user".to_owned(), "User: uses the home".to_owned()),
                                            ("owner".to_owned(), "Owner: runs it".to_owned()),
                                        ],
                                        move || Some(new_role.get().label().to_owned()),
                                        || false,
                                        move |picked| {
                                            new_role.set(if picked == "owner" {
                                                Role::Owner
                                            } else {
                                                Role::User
                                            });
                                        },
                                    )}
                                </div>
                            })}
                            {password_fields(
                                password,
                                again,
                                if first { "Password (leave empty for none)" } else { "Password" },
                            )}
                            <div class="settings-form-actions">
                                <button type="button" on:click=move |_| adding.set(false)>
                                    "Cancel"
                                </button>
                                <button type="submit" class="add">
                                    {if first { "Set up" } else { "Add" }}
                                </button>
                            </div>
                        </form>
                    </div>
                </div>
            }
        })}
        {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
        {move || (locked() && me().is_some()).then(|| view! {
            <p class="people-out">
                <button type="button" class="press" on:click=sign_out>"Sign out"</button>
            </p>
        })}
    }
}

/// Reads what was typed as a name for the welcome and the sign-in page, which have no row to
/// say what's wrong in.
pub fn name_of(typed: &str) -> Result<Name, String> {
    crate::places::name_of(typed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(name: &str, role: Role, has_password: bool) -> UserRow {
        UserRow {
            id: UserId::try_from(name.to_lowercase()).expect("an id"),
            name: Name::try_from(name).expect("a name"),
            role,
            has_password,
        }
    }

    #[test]
    fn the_row_says_who_is_in_the_home() {
        assert_eq!(summary(&[]), "nobody yet");
        assert_eq!(
            summary(&[person("Nico", Role::Owner, false)]),
            "Nico · no password"
        );
        assert_eq!(summary(&[person("Nico", Role::Owner, true)]), "Nico");
        assert_eq!(
            summary(&[
                person("Nico", Role::Owner, true),
                person("Guest", Role::User, true)
            ]),
            "2 people · 1 owner"
        );
    }

    #[test]
    fn a_new_password_is_checked_before_it_is_sent() {
        // Optional and left empty is fine; needed and left empty isn't.
        assert_eq!(password_trouble("", "", false), None);
        assert!(password_trouble("", "", true).is_some());
        assert!(password_trouble("short", "short", false).is_some());
        assert!(password_trouble("long enough", "long enuogh", false).is_some());
        assert_eq!(password_trouble("long enough", "long enough", true), None);
    }

    #[test]
    fn a_person_s_circle_holds_their_first_letter() {
        assert_eq!(initial(" nico"), "N");
        assert_eq!(initial("éloïse"), "É");
        assert_eq!(initial(""), "?");
    }
}
