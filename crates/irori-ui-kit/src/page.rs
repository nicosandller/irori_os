//! The page's end of the bridge.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use futures_channel::oneshot;
use serde::Serialize;
use serde::de::DeserializeOwned;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;

use crate::message::{Hello, Reply, Request, Theme, VERSION};

type Pending = Rc<RefCell<HashMap<u64, oneshot::Sender<Result<serde_json::Value, String>>>>>;

/// Something the shell told the page without being asked.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Theme(Theme),
    /// Back or forward moved to this path inside the page.
    Path(String),
}

/// The page's handle on the shell. Cheap to clone.
#[derive(Clone)]
pub struct Bridge {
    pending: Pending,
    next: Rc<RefCell<u64>>,
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bridge").finish_non_exhaustive()
    }
}

impl Bridge {
    /// Starts listening to the shell. `on_event` hears its events; answers find their requests.
    pub fn connect(on_event: impl Fn(Event) + 'static) -> Self {
        let pending: Pending = Rc::default();
        let listening = Rc::clone(&pending);
        let on_message =
            Closure::<dyn Fn(web_sys::MessageEvent)>::new(move |message: web_sys::MessageEvent| {
                // Only the shell that framed us talks to us.
                let from_parent = match (message.source(), web_sys::window()) {
                    (Some(source), Some(window)) => {
                        window.parent().ok().flatten().is_some_and(|parent| {
                            js_sys::Object::is(source.as_ref(), parent.as_ref())
                        })
                    }
                    _ => false,
                };
                if !from_parent {
                    return;
                }
                let Some(text) = message.data().as_string() else {
                    return;
                };
                let Ok(reply) = serde_json::from_str::<Reply>(&text) else {
                    return;
                };
                if reply.irori != VERSION {
                    return;
                }
                if let Some(id) = reply.id {
                    if let Some(answer) = listening.borrow_mut().remove(&id) {
                        let _ = answer.send(match reply.error {
                            Some(error) => Err(error),
                            None => Ok(reply.value.unwrap_or(serde_json::Value::Null)),
                        });
                    }
                    return;
                }
                let value = reply.value.unwrap_or(serde_json::Value::Null);
                match reply.event.as_deref() {
                    Some("theme") => {
                        if let Ok(theme) = serde_json::from_value::<Theme>(value) {
                            on_event(Event::Theme(theme));
                        }
                    }
                    Some("path") => {
                        if let Some(path) = value.as_str() {
                            on_event(Event::Path(path.to_owned()));
                        }
                    }
                    _ => {}
                }
            });
        if let Some(window) = web_sys::window() {
            let _ = window
                .add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref());
        }
        // The page lives as long as its listener does.
        on_message.forget();
        Self {
            pending,
            next: Rc::new(RefCell::new(0)),
        }
    }

    /// Asks the shell to do `op`.
    pub async fn call(
        &self,
        op: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = {
            let mut next = self.next.borrow_mut();
            *next += 1;
            *next
        };
        let (tx, rx) = oneshot::channel();
        self.pending.borrow_mut().insert(id, tx);
        let request = Request {
            irori: VERSION,
            id,
            op: op.to_owned(),
            args,
        };
        let text = serde_json::to_string(&request).map_err(|e| e.to_string())?;
        let parent = web_sys::window()
            .and_then(|window| window.parent().ok().flatten())
            .ok_or_else(|| "this page has to be opened from Irori".to_owned())?;
        // `*`: the shell's origin can't be named from inside an opaque frame, and the page's
        // own policy (`frame-ancestors 'self'`) already means only Irori can frame it.
        parent
            .post_message(&text.into(), "*")
            .map_err(|_| "couldn't reach Irori".to_owned())?;
        rx.await
            .unwrap_or_else(|_| Err("Irori didn't answer".to_owned()))
    }

    async fn call_as<T: DeserializeOwned>(
        &self,
        op: &str,
        args: serde_json::Value,
    ) -> Result<T, String> {
        let value = self.call(op, args).await?;
        serde_json::from_value(value).map_err(|e| format!("Irori's answer didn't fit: {e}"))
    }

    /// The theme and where the page should start.
    pub async fn hello(&self) -> Result<Hello, String> {
        self.call_as("hello", serde_json::Value::Null).await
    }

    /// Asks the page's engine.
    pub async fn rpc<T: DeserializeOwned>(
        &self,
        method: &str,
        params: impl Serialize,
    ) -> Result<T, String> {
        let params = serde_json::to_value(params).map_err(|e| e.to_string())?;
        self.call_as(
            "rpc",
            serde_json::json!({ "method": method, "params": params }),
        )
        .await
    }

    /// Every entity, device and area, as the shell has them.
    pub async fn registry<T: DeserializeOwned>(&self) -> Result<T, String> {
        self.call_as("registry", serde_json::Value::Null).await
    }

    /// Every entity's current state.
    pub async fn states<T: DeserializeOwned>(&self) -> Result<T, String> {
        self.call_as("states", serde_json::Value::Null).await
    }

    /// The last day of one entity's changes.
    pub async fn history<T: DeserializeOwned>(&self, entity_id: &str) -> Result<T, String> {
        self.call_as("history", serde_json::json!({ "entity_id": entity_id }))
            .await
    }

    /// Puts `path` in the address bar, after `/apps/<extension>/`.
    pub async fn navigate(&self, path: &str) {
        let _ = self
            .call("navigate", serde_json::json!({ "path": path }))
            .await;
    }
}

/// Makes this page look like the shell: its tokens on `<html>`, and `data-theme` and
/// `data-motion` for the page's own stylesheet to key on.
pub fn apply_theme(theme: &Theme) {
    let Some(root) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.document_element())
    else {
        return;
    };
    let Ok(root) = root.dyn_into::<web_sys::HtmlElement>() else {
        return;
    };
    let style = root.style();
    for (name, value) in &theme.tokens {
        let _ = style.set_property(name, value);
    }
    let _ = root.set_attribute("data-theme", if theme.dark { "dark" } else { "light" });
    let _ = root.set_attribute(
        "data-motion",
        if theme.reduced_motion { "off" } else { "on" },
    );
}
