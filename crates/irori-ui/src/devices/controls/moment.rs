//! A day, a time of day or a moment the device holds, set from here with the browser's own
//! pickers.

use irori_types::{Entity, State};
use leptos::prelude::*;

use crate::devices::Controls;

/// A date: a day picker, sent when a day is picked.
pub(crate) fn date_control(
    entity: &Entity,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let current = match value {
        Some(State::Date(date)) => date.value.to_string(),
        _ => String::new(),
    };
    picker(entity, "date", current, offline, controls, |picked| {
        Some(serde_json::json!({ "date": picked }))
    })
}

/// A time of day: a clock picker, to the second.
pub(crate) fn time_control(
    entity: &Entity,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let current = match value {
        Some(State::Time(time)) => time.value.to_string(),
        _ => String::new(),
    };
    picker(entity, "time", current, offline, controls, |picked| {
        Some(serde_json::json!({ "time": picked }))
    })
}

/// A moment: a day and a time, shown and picked in this browser's own time zone and sent as
/// the instant that is.
pub(crate) fn datetime_control(
    entity: &Entity,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let current = match value {
        Some(State::Datetime(moment)) => local_from_instant(moment.value),
        _ => String::new(),
    };
    picker(
        entity,
        "datetime-local",
        current,
        offline,
        controls,
        |picked| {
            instant_from_local(&picked).map(|instant| serde_json::json!({ "datetime": instant }))
        },
    )
}

fn picker(
    entity: &Entity,
    input: &'static str,
    current: String,
    offline: bool,
    controls: Controls,
    data: impl Fn(String) -> Option<serde_json::Value> + 'static,
) -> AnyView {
    let entity_id = entity.id.clone();
    let disable = {
        let entity_id = entity_id.clone();
        move || offline || controls.busy.get().contains(&entity_id)
    };
    view! {
        <input
            class="text-control moment-control"
            type=input
            // A time of day is held to the second.
            step=(input != "date").then_some("1")
            aria-label=entity.name.to_string()
            prop:value=current
            disabled=disable
            on:change:target=move |ev| {
                let picked = ev.target().value();
                // Cleared: there's no "no value" to set it to, so nothing is sent.
                if picked.is_empty() {
                    return;
                }
                if let Some(data) = data(picked) {
                    controls.act.run((entity_id.clone(), "set_value", Some(data)));
                }
            }
        />
    }
    .into_any()
}

/// An instant as a `datetime-local` input holds it: `2026-10-14T08:45:00`, on this browser's
/// clock.
fn local_from_instant(instant: irori_types::Timestamp) -> String {
    #[allow(clippy::cast_precision_loss)]
    let date = web_sys::js_sys::Date::new(&web_sys::wasm_bindgen::JsValue::from_f64(
        instant.as_second() as f64 * 1000.0,
    ));
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        date.get_full_year(),
        date.get_month() + 1,
        date.get_date(),
        date.get_hours(),
        date.get_minutes(),
        date.get_seconds()
    )
}

/// What a `datetime-local` input holds, as the instant it is on this browser's clock. `None`
/// for something that isn't a moment.
fn instant_from_local(picked: &str) -> Option<String> {
    // A string with a time and no offset is read as local time, which is what was picked.
    let date = web_sys::js_sys::Date::new(&web_sys::wasm_bindgen::JsValue::from_str(picked));
    let millis = date.get_time();
    millis.is_finite().then(|| {
        // Whole seconds: `2026-10-14T06:45:00.000Z` without the `.000`.
        let text = String::from(date.to_iso_string());
        match text.split_once('.') {
            Some((seconds, _)) => format!("{seconds}Z"),
            None => text,
        }
    })
}
