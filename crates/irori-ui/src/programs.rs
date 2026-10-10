//! The Programs row of Settings: tokens for programs that aren't this page.
//!
//! An owner makes one, copies the secret once, and can take it back. A token never changes
//! how the home is set up. One that names an extension is only for that extension to connect.

use irori_types::{ApiScope, ExtensionId, Name, TokenId};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{self, TokenRow};

/// What a program may be allowed to do, in the order the form offers it, each with a line
/// about what that comes to.
const SCOPES: [(&str, &str, ApiScope); 5] = [
    (
        "See devices and rooms",
        "Their names, and which room each is in.",
        ApiScope::RegistryRead,
    ),
    (
        "See what devices are doing",
        "On or off, readings, and when each last changed.",
        ApiScope::StatesRead,
    ),
    (
        "Hear when something changes",
        "Told as it happens, without having to ask again.",
        ApiScope::EventsRead,
    ),
    (
        "Control devices",
        "Switch them, dim them, set them. The one that changes anything.",
        ApiScope::ServicesCall,
    ),
    (
        "See recent history",
        "What a device reported over the last day.",
        ApiScope::HistoryRead,
    ),
];

/// A key: what a token is.
const KEY: &str = r#"<circle cx="8" cy="15" r="4.2" fill="none" stroke="currentColor" stroke-width="1.8"/><path d="M11 12l8.5-8.5M16 6.5l2.5 2.5M13.5 9l2 2" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>"#;

/// How long the copy button says it copied.
const COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1600);

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
        .find(|(_, _, known)| *known == scope)
        .map(|(words, _, _)| *words)
        .unwrap_or("something else")
}

/// What this token is for, as the few words under its name: one for each thing it may do.
fn may_do(token: &TokenRow) -> Vec<String> {
    if let Some(extension) = &token.extension {
        return vec![format!("connects {extension}")];
    }
    if token.scopes.is_empty() {
        return vec!["nothing".to_owned()];
    }
    token
        .scopes
        .iter()
        .map(|scope| scope_words(*scope).to_owned())
        .collect()
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
    // The token just made: its name, and the secret that is shown this once.
    let made = RwSignal::new(None::<(String, String)>);
    let copied = RwSignal::new(None::<bool>);
    let adding = RwSignal::new(false);
    // Whether the token being made is for an extension to connect with, not for a program.
    let for_extension = RwSignal::new(false);
    // The token somebody has pressed Revoke on, while they are asked whether they meant it.
    let revoking = RwSignal::new(None::<TokenId>);
    let busy = RwSignal::new(false);

    let add = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }
        let typed = name.get_untracked();
        let name_of = match Name::try_from(typed.trim()) {
            Ok(name) => name,
            Err(error) => {
                trouble.set(Some(error.to_string()));
                return;
            }
        };
        let body = if for_extension.get_untracked() {
            let connecting = extension.get_untracked();
            let Ok(id) = ExtensionId::try_from(connecting.trim()) else {
                trouble.set(Some(
                    "An extension's id is lowercase words separated by underscores.".to_owned(),
                ));
                return;
            };
            serde_json::json!({ "name": name_of.as_str(), "extension": id.as_str() })
        } else {
            let scopes = picked.get_untracked();
            if scopes.is_empty() {
                trouble.set(Some("Pick at least one thing it may do.".to_owned()));
                return;
            }
            serde_json::json!({ "name": name_of.as_str(), "scopes": scopes })
        };
        trouble.set(None);
        busy.set(true);
        spawn_local(async move {
            match api::create_token(&body).await {
                Ok(created) => {
                    copied.set(None);
                    made.set(Some((name_of.to_string(), created.secret)));
                    name.set(String::new());
                    extension.set(String::new());
                    picked.set(Vec::new());
                    for_extension.set(false);
                    adding.set(false);
                    if let Ok(tokens) = api::fetch_tokens().await {
                        programs.set(tokens);
                    }
                }
                Err(why) => trouble.set(Some(why)),
            }
            busy.set(false);
        });
    };

    let revoke = move |id: TokenId| {
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
            revoking.set(None);
        });
    };

    let copy = move |_| {
        let Some((_, secret)) = made.get_untracked() else {
            return;
        };
        spawn_local(async move {
            copied.set(Some(crate::log_window::write_to_clipboard(&secret).await));
            set_timeout(move || copied.set(None), COPIED_FOR);
        });
    };

    // The switch between the two kinds of token marks the one chosen with a highlight that
    // slides, as every switcher's does.
    let kinds = NodeRef::<leptos::html::Div>::new();
    crate::glide::across(kinds, "button.chosen", move || for_extension.track());

    view! {
        <p class="muted small tokens-about">
            "A token lets something that isn't this page reach the home: a script, a dashboard \
             on a tablet, an extension running on another machine. It can do only what it was \
             made for, and never changes how the home is set up."
        </p>
        <ul class="tokens">
            {move || {
                let tokens = programs.get();
                if tokens.is_empty() {
                    return view! { <li class="tokens-none muted">"No tokens yet."</li> }.into_any();
                }
                tokens.into_iter().enumerate().map(|(place, token)| {
                    let id = token.id.clone();
                    let (asked, sure, keep) = (id.clone(), id.clone(), id.clone());
                    let label = token.name.clone();
                    let about = may_do(&token);
                    let connects = token.extension.is_some();
                    view! {
                        // One after another, so a list arriving reads as a list.
                        <li class="token" style=format!("--place: {place}")>
                            <span class="token-mark" class:connects=connects aria-hidden="true">
                                <svg viewBox="0 0 24 24" inner_html=KEY></svg>
                            </span>
                            <span class="token-words">
                                <span class="token-name">{label}</span>
                                <span class="token-may">
                                    {about.into_iter().map(|words| view! {
                                        <span class="scope-chip">{words}</span>
                                    }).collect_view()}
                                </span>
                            </span>
                            // Asked where the button was, not in a box the browser draws: a
                            // revoke can't be undone, and the question belongs beside the
                            // token it is about.
                            {move || if revoking.get().as_ref() == Some(&asked) {
                                let (sure, keep) = (sure.clone(), keep.clone());
                                view! {
                                    <span class="token-sure">
                                        <span class="muted small">"It stops working at once."</span>
                                        <button type="button" class="press danger"
                                            on:click=move |_| revoke(sure.clone())>
                                            "Revoke"
                                        </button>
                                        <button type="button" class="press"
                                            on:click=move |_| {
                                                if revoking.get_untracked().as_ref() == Some(&keep) {
                                                    revoking.set(None);
                                                }
                                            }>
                                            "Keep"
                                        </button>
                                    </span>
                                }.into_any()
                            } else {
                                let id = id.clone();
                                view! {
                                    <button type="button" class="press"
                                        on:click=move |_| revoking.set(Some(id.clone()))>
                                        "Revoke"
                                    </button>
                                }.into_any()
                            }}
                        </li>
                    }
                }).collect_view().into_any()
            }}
        </ul>
        // The secret, the one time it can be seen: said as the thing to do next, with the way
        // to do it beside it.
        {move || made.get().map(|(label, secret)| view! {
            <div class="token-secret" role="status">
                <p class="token-secret-head">
                    <svg viewBox="0 0 24 24" aria-hidden="true">
                        <circle cx="12" cy="12" r="9.5" pathLength="1" />
                        <path d="M7.5 12.5l3 3 6-6.5" pathLength="1" />
                    </svg>
                    <span><b>{label}</b>" is ready. This is its secret, and this is the only time it's shown."</span>
                </p>
                <div class="token-secret-row">
                    <code>{secret}</code>
                    <button
                        type="button"
                        class="log-copy"
                        class:done=move || copied.get() == Some(true)
                        class:failed=move || copied.get() == Some(false)
                        aria-label="Copy the secret"
                        title=move || match copied.get() {
                            Some(true) => "Copied",
                            Some(false) => "Copy failed — the browser didn't allow it",
                            None => "Copy the secret",
                        }
                        on:click=copy
                    >
                        {crate::log_window::copy_icon()}
                    </button>
                </div>
                <div class="token-secret-foot">
                    <span class="muted small">
                        "Put it in the program now. Irori keeps only enough to recognise it."
                    </span>
                    <button type="button" class="press" on:click=move |_| made.set(None)>
                        "I've copied it"
                    </button>
                </div>
            </div>
        })}
        {move || owner().then(|| view! {
            <div class="token-new">
                {move || (!adding.get()).then(|| view! {
                    <button type="button" class="press token-add" on:click=move |_| {
                        trouble.set(None);
                        adding.set(true);
                    }>
                        <span aria-hidden="true">"+"</span>" New token"
                    </button>
                })}
                <div class="drawer" class:open=move || adding.get() inert=move || (!adding.get()).then_some("")>
                    <form class="drawer-inner token-form" on:submit=add>
                        <label class="settings-field">
                            "Name"
                            <input
                                type="text"
                                autocomplete="off"
                                placeholder="Kitchen tablet"
                                prop:value=move || name.get()
                                on:input=move |event| name.set(event_target_value(&event))
                            />
                        </label>
                        <div class="settings-field">
                            <span id="token-kind">"It is for"</span>
                            <div class="switcher" role="group" aria-labelledby="token-kind" node_ref=kinds>
                                <span class="glide" aria-hidden="true"></span>
                                <button
                                    type="button"
                                    class:chosen=move || !for_extension.get()
                                    aria-pressed=move || (!for_extension.get()).to_string()
                                    on:click=move |_| for_extension.set(false)
                                >
                                    "A program"
                                </button>
                                <button
                                    type="button"
                                    class:chosen=move || for_extension.get()
                                    aria-pressed=move || for_extension.get().to_string()
                                    on:click=move |_| for_extension.set(true)
                                >
                                    "An extension"
                                </button>
                            </div>
                        </div>
                        {move || if for_extension.get() {
                            view! {
                                <label class="settings-field token-kind-body">
                                    "The extension's id"
                                    <input
                                        type="text"
                                        autocomplete="off"
                                        spellcheck="false"
                                        placeholder="my_extension"
                                        prop:value=move || extension.get()
                                        on:input=move |event| extension.set(event_target_value(&event))
                                    />
                                    <span class="muted small">
                                        "The token is only for that extension to connect with, \
                                         from wherever it runs."
                                    </span>
                                </label>
                            }.into_any()
                        } else {
                            view! {
                                <div class="scope-picks token-kind-body" role="group" aria-label="What it may do">
                                    {SCOPES.into_iter().map(|(words, detail, scope)| {
                                        let on = move || picked.with(|picked| picked.contains(&scope));
                                        view! {
                                            <button
                                                type="button"
                                                class="scope-pick"
                                                class:on=on
                                                aria-pressed=move || on().to_string()
                                                on:click=move |_| picked.update(|picked| {
                                                    if picked.contains(&scope) {
                                                        picked.retain(|have| *have != scope);
                                                    } else {
                                                        picked.push(scope);
                                                    }
                                                })
                                            >
                                                <svg class="scope-tick" viewBox="0 0 24 24" aria-hidden="true">
                                                    <rect x="3.5" y="3.5" width="17" height="17" rx="4.5" />
                                                    <path d="M7.5 12.5l3 3 6-6.5" pathLength="1" />
                                                </svg>
                                                <span class="scope-words">
                                                    <span>{words}</span>
                                                    <span class="muted small">{detail}</span>
                                                </span>
                                            </button>
                                        }
                                    }).collect_view()}
                                </div>
                            }.into_any()
                        }}
                        <div class="settings-form-actions">
                            <button type="button" on:click=move |_| {
                                trouble.set(None);
                                adding.set(false);
                            }>
                                "Cancel"
                            </button>
                            <button type="submit" class="add" disabled=move || busy.get()>
                                {move || if busy.get() { "Creating…" } else { "Create" }}
                            </button>
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

    #[test]
    fn a_token_says_each_thing_it_may_do() {
        assert_eq!(may_do(&token("tablet")), ["See what devices are doing"]);
        let connecting = TokenRow {
            extension: Some(ExtensionId::try_from("weather").expect("an id")),
            ..token("weather")
        };
        assert_eq!(may_do(&connecting), ["connects weather"]);
        let idle = TokenRow {
            scopes: Vec::new(),
            ..token("idle")
        };
        assert_eq!(may_do(&idle), ["nothing"]);
    }
}
