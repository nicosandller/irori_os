//! Putting text on the clipboard, from the shell or from a page in its sandboxed frame.
//!
//! `navigator.clipboard` is only there on a page the browser calls secure, and a home's Irori
//! is as often as not opened over plain http on the LAN. The old way — select the text and
//! ask the document to copy it — works there, and inside a sandboxed frame, so it is what is
//! fallen back on.

use js_sys::{Function, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;

/// Copies `text`, and says whether it really was: the browser refuses without a press of the
/// person's own, or without permission. Saying "Copied" for a copy that didn't happen is the
/// one thing a copy button must not do. Call it from the press itself.
pub async fn copy(text: &str) -> bool {
    if let Some(asked) = ask_navigator(text)
        && JsFuture::from(asked).await.is_ok()
    {
        return true;
    }
    by_selection(text).unwrap_or(false)
}

/// `navigator.clipboard.writeText(text)`, where there is such a thing.
fn ask_navigator(text: &str) -> Option<Promise> {
    let window: JsValue = web_sys::window()?.into();
    let navigator = Reflect::get(&window, &JsValue::from_str("navigator")).ok()?;
    let clipboard = Reflect::get(&navigator, &JsValue::from_str("clipboard")).ok()?;
    let write: Function = Reflect::get(&clipboard, &JsValue::from_str("writeText"))
        .ok()?
        .dyn_into()
        .ok()?;
    write
        .call1(&clipboard, &JsValue::from_str(text))
        .ok()?
        .dyn_into()
        .ok()
}

/// The text in a box nobody sees, selected, and the document asked to copy what is selected.
fn by_selection(text: &str) -> Option<bool> {
    let document = web_sys::window()?.document()?;
    let body = document.body()?;
    let held = document.create_element("textarea").ok()?;
    held.set_text_content(Some(text));
    held.set_attribute("readonly", "").ok()?;
    held.set_attribute("aria-hidden", "true").ok()?;
    held.set_attribute(
        "style",
        "position:fixed;top:0;left:-9999px;opacity:0;pointer-events:none",
    )
    .ok()?;
    body.append_child(&held).ok()?;
    let call = |on: &JsValue, name: &str, args: &[&str]| -> Option<JsValue> {
        let function: Function = Reflect::get(on, &JsValue::from_str(name))
            .ok()?
            .dyn_into()
            .ok()?;
        let args: js_sys::Array = args.iter().map(|arg| JsValue::from_str(arg)).collect();
        Reflect::apply(&function, on, &args).ok()
    };
    call(&held, "select", &[]);
    let copied = call(&document, "execCommand", &["copy"]).and_then(|done| done.as_bool());
    held.remove();
    copied
}
