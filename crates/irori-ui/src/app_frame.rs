//! An extension's own page, framed beside the sidebar (`docs/specs/automations.md` §B3–B4).
//!
//! The page runs in `<iframe sandbox="allow-scripts">`: an opaque origin that can't read this
//! page's storage or call Irori's API as it. It asks for what it needs over the bridge, and this
//! end answers — only messages from its own frame, and only what the extension's declared scopes
//! allow.

use irori_ui_kit::message::{Hello, Reply, Request, TOKENS, Theme, VERSION, scope_for};
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_location, use_params_map};
use web_sys::wasm_bindgen::JsCast;

use crate::{Live, Motion, api};

/// `/apps/:id/*rest`: the page of extension `id`, at `rest` inside it.
#[component]
pub fn AppPage() -> impl IntoView {
    let params = use_params_map();
    let id = Memo::new(move |_| params.read().get("id").unwrap_or_default());
    // A new frame only for a different extension: moving within one page is the page's business.
    move || {
        let id = id.get();
        view! { <Frame id=id /> }
    }
}

/// The part of the address after `/apps/<id>/`.
fn rest_of(pathname: &str, id: &str) -> String {
    pathname
        .strip_prefix(&format!("/apps/{id}"))
        .unwrap_or("")
        .trim_start_matches('/')
        .to_owned()
}

#[component]
fn Frame(id: String) -> impl IntoView {
    let live = expect_context::<Live>();
    let motion = expect_context::<Motion>();
    let location = use_location();

    let app = {
        let id = id.clone();
        Memo::new(move |_| {
            live.home.with(|home| {
                home.extensions
                    .iter()
                    .find(|(extension, _)| extension.as_str() == id)
                    .and_then(|(_, extension)| {
                        extension
                            .app
                            .clone()
                            .map(|app| (app, extension.state.clone(), extension.engine))
                    })
            })
        })
    };
    let label = move || app.get().map(|(app, _, _)| app.label).unwrap_or_default();

    // Whether its files are there; asked once. `None` while asking.
    let built = RwSignal::new(None::<bool>);
    {
        let id = id.clone();
        spawn_local(async move {
            let entries = api::fetch_apps().await.unwrap_or_default();
            built.set(Some(
                entries
                    .iter()
                    .any(|entry| entry.extension == id && entry.built),
            ));
        });
    }

    let frame = NodeRef::<leptos::html::Iframe>::new();
    // Where the page starts, read once: the frame isn't reloaded when the page moves itself.
    let src = format!(
        "/pages/{id}/#/{}",
        rest_of(&location.pathname.get_untracked(), &id)
    );
    // The last path the page put in the address bar, so back/forward can tell it where to go
    // and its own moves aren't echoed back to it.
    let said_path = RwSignal::new(rest_of(&location.pathname.get_untracked(), &id));
    let greeted = RwSignal::new(false);

    let post = move |reply: &Reply| {
        let Some(frame) = frame.get_untracked() else {
            return;
        };
        let Some(target) = frame.content_window() else {
            return;
        };
        if let Ok(text) = serde_json::to_string(reply) {
            // `*`: the frame's origin is opaque, so there's no name to address it by.
            let _ = target.post_message(&text.into(), "*");
        }
    };

    let theme = move || Theme {
        dark: media("(prefers-color-scheme: dark)"),
        reduced_motion: !motion.0.get_untracked() || media("(prefers-reduced-motion: reduce)"),
        tokens: tokens(),
    };

    // Settings' motion switch reaches the page as it changes.
    Effect::new(move |_| {
        motion.0.track();
        if greeted.get_untracked() {
            post(&Reply::event(
                "theme",
                serde_json::to_value(theme()).unwrap_or_default(),
            ));
        }
    });

    // Back and forward inside the page.
    {
        let id = id.clone();
        Effect::new(move |_| {
            let path = rest_of(&location.pathname.get(), &id);
            if greeted.get_untracked() && path != said_path.get_untracked() {
                said_path.set(path.clone());
                post(&Reply::event("path", serde_json::Value::String(path)));
            }
        });
    }

    let listening = {
        let id = id.clone();
        window_event_listener(ev::message, move |message: web_sys::MessageEvent| {
            // Only our own frame. Its origin reads as the string "null", which proves nothing;
            // the window it came from does.
            let ours = match (message.source(), frame.get_untracked()) {
                (Some(source), Some(frame)) => frame
                    .content_window()
                    .is_some_and(|window| js_sys_is(&source, &window)),
                _ => false,
            };
            if !ours {
                return;
            }
            let Some(text) = message.data().as_string() else {
                return;
            };
            let Ok(request) = serde_json::from_str::<Request>(&text) else {
                return;
            };
            if request.irori != VERSION {
                return;
            }
            let scopes = app
                .get_untracked()
                .map(|(app, _, _)| app.api)
                .unwrap_or_default();
            if let Some(scope) = scope_for(&request.op)
                && !scopes.iter().any(|declared| declared == scope)
            {
                post(&Reply::answer(
                    request.id,
                    Err(format!("this extension didn't ask for {scope}")),
                ));
                return;
            }
            let id = id.clone();
            match request.op.as_str() {
                "hello" => {
                    greeted.set(true);
                    let hello = Hello {
                        theme: theme(),
                        path: said_path.get_untracked(),
                    };
                    post(&Reply::answer(
                        request.id,
                        serde_json::to_value(hello).map_err(|e| e.to_string()),
                    ));
                }
                "registry" => {
                    let value = live.home.with_untracked(|home| {
                        serde_json::json!({
                            "entities": home.entities,
                            "devices": home.devices,
                            "areas": home.areas,
                        })
                    });
                    post(&Reply::answer(request.id, Ok(value)));
                }
                "states" => {
                    let value = live
                        .home
                        .with_untracked(|home| serde_json::to_value(&home.states));
                    post(&Reply::answer(request.id, value.map_err(|e| e.to_string())));
                }
                "history" => spawn_local(async move {
                    let entity = request.args["entity_id"].as_str().unwrap_or_default();
                    let result = match entity.parse::<irori_types::EntityId>() {
                        Ok(entity) => api::entity_history(&entity).await.and_then(|states| {
                            serde_json::to_value(states).map_err(|e| e.to_string())
                        }),
                        Err(error) => Err(error.to_string()),
                    };
                    post(&Reply::answer(request.id, result));
                }),
                "rpc" => spawn_local(async move {
                    let method = request.args["method"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    let params = request.args["params"].clone();
                    let result = api::app_rpc(&id, &method, params).await;
                    post(&Reply::answer(request.id, result));
                }),
                "navigate" => {
                    let path = request.args["path"]
                        .as_str()
                        .unwrap_or_default()
                        .trim_start_matches('/')
                        .to_owned();
                    said_path.set(path.clone());
                    // Straight into history, not through the router: the router would treat it
                    // as a new page and redraw the frame.
                    if let Ok(history) = window().history() {
                        let _ = history.push_state_with_url(
                            &web_sys::wasm_bindgen::JsValue::NULL,
                            "",
                            Some(&format!("/apps/{id}/{path}")),
                        );
                    }
                    post(&Reply::answer(request.id, Ok(serde_json::Value::Null)));
                }
                // What went wrong inside the page, where its own console is out of sight.
                "log" => leptos::logging::error!("{id}'s page: {}", request.args),
                other => post(&Reply::answer(
                    request.id,
                    Err(format!("the bridge has no `{other}`")),
                )),
            }
        })
    };
    on_cleanup(move || listening.remove());

    view! {
        {move || match (app.get(), built.get()) {
            (None, _) => view! {
                <section class="card">
                    <h1>"There's no page here"</h1>
                    <p class="muted">"No running extension has a page at this address."</p>
                </section>
            }
            .into_any(),
            (Some(_), Some(false)) => view! {
                <section class="card">
                    <h1>{label()}</h1>
                    <p class="muted">
                        "This extension's page wasn't built, so there's nothing to show yet. "
                        "From a checkout of Irori, "
                        <code>"cargo xtask ui"</code>
                        " builds it; then install the extension again."
                    </p>
                </section>
            }
            .into_any(),
            (Some((_, state, _)), _) if state != "running" && state != "degraded" => view! {
                <section class="card">
                    <h1>{label()}</h1>
                    <p class="muted">"It isn't running right now; see the Extensions page."</p>
                </section>
            }
            .into_any(),
            _ => ().into_any(),
        }}
        <iframe
            class="app-frame"
            class:hidden=move || !matches!(built.get(), Some(true)) || app.get().is_none()
            node_ref=frame
            title=label
            src=src
            sandbox="allow-scripts"
        ></iframe>
    }
}

fn js_sys_is(a: &web_sys::js_sys::Object, b: &web_sys::Window) -> bool {
    let b: &web_sys::js_sys::Object = b.unchecked_ref();
    web_sys::js_sys::Object::is(a, b)
}

fn media(query: &str) -> bool {
    window()
        .match_media(query)
        .ok()
        .flatten()
        .is_some_and(|list| list.matches())
}

/// The shell's tokens as they compute right now (dark mode included).
fn tokens() -> std::collections::BTreeMap<String, String> {
    let Some(root) = document().document_element() else {
        return Default::default();
    };
    let Ok(Some(style)) = window().get_computed_style(&root) else {
        return Default::default();
    };
    TOKENS
        .iter()
        .filter_map(|name| {
            let value = style.get_property_value(name).ok()?;
            let value = value.trim();
            (!value.is_empty()).then(|| ((*name).to_owned(), value.to_owned()))
        })
        .collect()
}
