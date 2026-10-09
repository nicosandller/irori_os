//! The Programs row of Settings: tokens for programs that aren't this page.
//!
//! An owner makes one, copies the secret once, and can take it back. A token never changes
//! how the home is set up. One that names an extension is only for that extension to connect.

use irori_types::{ApiScope, ExtensionId, Name, TokenId};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{self, TokenRow};

/// What a program may be allowed to do, in the order the form offers it.
const SCOPES: [(&str, ApiScope); 5] = [
    ("See devices and rooms", ApiScope::RegistryRead),
    ("See what devices are doing", ApiScope::StatesRead),
    ("Hear when something changes", ApiScope::EventsRead),
    ("Control devices", ApiScope::ServicesCall),
    ("See recent history", ApiScope::HistoryRead),
];

/// How the row stands while it's folded.
pub fn summary(tokens: &[TokenRow]) -> String {
    match tokens.len() {
        0 => "none yet".to_owned(),
        1 => "1 program".to_owned(),
        n => format!("{n} programs"),
    }
}

fn scope_words(scope: ApiScope) -> &'static str {
    SCOPES
        .iter()
        .find(|(_, known)| *known == scope)
        .map(|(words, _)| *words)
        .unwrap_or("something else")
}

/// What this token is for, in a line under its name.
fn may_do(token: &TokenRow) -> String {
    if let Some(extension) = &token.extension {
        return format!("connects {extension}");
    }
    if token.scopes.is_empty() {
        return "nothing".to_owned();
    }
    token
        .scopes
        .iter()
        .map(|scope| scope_words(*scope))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every token, as last fetched. Kept above the row so its folded state can say how it stands.
#[derive(Debug, Clone, Copy)]
pub struct Programs(pub RwSignal<Vec<TokenRow>>);

#[component]
pub fn Section() -> impl IntoView {
    let Programs(programs) = expect_context::<Programs>();
    let crate::Session(session) = expect_context::<crate::Session>();
    let owner = move || session.with(|session| session.as_ref().is_none_or(|s| s.owner));
    let trouble = RwSignal::new(None::<String>);
    let name = RwSignal::new(String::new());
    let extension = RwSignal::new(String::new());
    let picked = RwSignal::new(Vec::<ApiScope>::new());
    let secret = RwSignal::new(None::<String>);
    let adding = RwSignal::new(false);

    let add = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let typed = name.get_untracked();
        let name_of = match Name::try_from(typed.trim()) {
            Ok(name) => name,
            Err(error) => {
                trouble.set(Some(error.to_string()));
                return;
            }
        };
        let connecting = extension.get_untracked();
        let connecting = connecting.trim();
        let body = if connecting.is_empty() {
            let scopes = picked.get_untracked();
            if scopes.is_empty() {
                trouble.set(Some(
                    "Pick what it may do, or name the extension it connects.".to_owned(),
                ));
                return;
            }
            serde_json::json!({ "name": name_of.as_str(), "scopes": scopes })
        } else {
            let Ok(id) = ExtensionId::try_from(connecting) else {
                trouble.set(Some(
                    "An extension's id is lowercase words separated by underscores.".to_owned(),
                ));
                return;
            };
            serde_json::json!({ "name": name_of.as_str(), "extension": id.as_str() })
        };
        trouble.set(None);
        spawn_local(async move {
            match api::create_token(&body).await {
                Ok(created) => {
                    secret.set(Some(created.secret));
                    name.set(String::new());
                    extension.set(String::new());
                    picked.set(Vec::new());
                    adding.set(false);
                    if let Ok(tokens) = api::fetch_tokens().await {
                        programs.set(tokens);
                    }
                }
                Err(why) => trouble.set(Some(why)),
            }
        });
    };

    let revoke = move |id: TokenId| {
        let Some(window) = web_sys::window() else {
            return;
        };
        if !window
            .confirm_with_message(
                "Revoke this token? The program using it can no longer reach the home.",
            )
            .unwrap_or(false)
        {
            return;
        }
        spawn_local(async move {
            match api::revoke_token(&id).await {
                Ok(()) => {
                    trouble.set(None);
                    if let Ok(tokens) = api::fetch_tokens().await {
                        programs.set(tokens);
                    }
                }
                Err(why) => trouble.set(Some(why)),
            }
        });
    };

    view! {
        <ul class="people">
            {move || programs.get().into_iter().map(|token| {
                let id = token.id.clone();
                let label = token.name.clone();
                let about = may_do(&token);
                view! {
                    <li class="person">
                        <span class="person-words">
                            <span class="person-name">{label}</span>
                            <span class="muted small">{about}</span>
                        </span>
                        <button type="button" class="press" on:click=move |_| revoke(id.clone())>
                            "Revoke"
                        </button>
                    </li>
                }
            }).collect::<Vec<_>>()}
        </ul>
        {move || secret.get().map(|secret| view! {
            <label class="settings-field">
                "Its secret"
                <input type="text" readonly autocomplete="off" spellcheck="false" prop:value=secret />
            </label>
            <p class="muted small">"Shown once. Copy it now."</p>
        })}
        {move || owner().then(|| view! {
            <div>
                <button type="button" class="press" on:click=move |_| adding.update(|open| *open = !*open)>
                    "New token"
                </button>
                <div class="drawer" class:open=move || adding.get() inert=move || (!adding.get()).then_some("")>
                    <form class="drawer-inner person-form" on:submit=add>
                        <label class="settings-field">
                            "Name"
                            <input
                                type="text"
                                autocomplete="off"
                                prop:value=move || name.get()
                                on:input=move |event| name.set(event_target_value(&event))
                            />
                        </label>
                        {SCOPES.into_iter().map(|(words, scope)| view! {
                            <label class="settings-field settings-field-checkbox">
                                <input
                                    type="checkbox"
                                    prop:checked=move || picked.with(|picked| picked.contains(&scope))
                                    on:change:target=move |event| {
                                        let on = event.target().checked();
                                        picked.update(|picked| {
                                            if on && !picked.contains(&scope) {
                                                picked.push(scope);
                                            } else if !on {
                                                picked.retain(|have| *have != scope);
                                            }
                                        });
                                    }
                                />
                                <span>{words}</span>
                            </label>
                        }).collect::<Vec<_>>()}
                        <label class="settings-field">
                            "Extension, if this token is for one to connect"
                            <input
                                type="text"
                                autocomplete="off"
                                spellcheck="false"
                                prop:value=move || extension.get()
                                on:input=move |event| extension.set(event_target_value(&event))
                            />
                        </label>
                        <p class="muted small">
                            "Filled in, the token is only for that extension. Leave it empty and tick what a program may do."
                        </p>
                        <div class="settings-form-actions">
                            <button type="button" on:click=move |_| adding.set(false)>"Cancel"</button>
                            <button type="submit" class="add">"Create"</button>
                        </div>
                    </form>
                </div>
            </div>
        })}
        {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(name: &str) -> TokenRow {
        TokenRow {
            id: TokenId::try_from(name).expect("an id"),
            name: name.to_owned(),
            scopes: vec![ApiScope::StatesRead],
            extension: None,
            created: 0,
        }
    }

    #[test]
    fn the_row_says_how_many_programs() {
        assert_eq!(summary(&[]), "none yet");
        assert_eq!(summary(&[token("tablet")]), "1 program");
        assert_eq!(summary(&[token("tablet"), token("wall")]), "2 programs");
    }
}
