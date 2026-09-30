//! Time on the page: the browser's clock, and times the way a person reads them.

use irori_types::Timestamp;

/// Now, by the browser's clock.
pub fn now() -> Timestamp {
    from_millis(js_sys::Date::now() as i64)
}

pub fn from_millis(ms: i64) -> Timestamp {
    Timestamp::from_jiff(
        jiff::Timestamp::from_millisecond(ms).unwrap_or(jiff::Timestamp::UNIX_EPOCH),
    )
}

pub fn millis(at: Timestamp) -> i64 {
    at.as_jiff().as_millisecond()
}

/// "3 min ago".
pub fn ago(at: &Timestamp) -> String {
    let seconds = (millis(now()) - millis(*at)) / 1000;
    match seconds {
        s if s < 45 => "just now".into(),
        s if s < 90 * 60 => format!("{} min ago", (s + 30) / 60),
        s if s < 36 * 3600 => format!("{} h ago", (s + 1800) / 3600),
        s => format!("{} days ago", (s + 43_200) / 86_400),
    }
}

/// The time of day, in the browser's own zone: "22:04:31".
pub fn clock(at: &Timestamp) -> String {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(millis(*at) as f64));
    format!(
        "{:02}:{:02}:{:02}",
        date.get_hours(),
        date.get_minutes(),
        date.get_seconds()
    )
}

/// Day and time: "Sep 29, 22:04".
pub fn when(at: &Timestamp) -> String {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(millis(*at) as f64));
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{} {}, {:02}:{:02}",
        MONTHS[date.get_month() as usize % 12],
        date.get_date(),
        date.get_hours(),
        date.get_minutes()
    )
}

/// "1:12", "8:48", "1:02:03": how long, to the second.
pub fn span(ms: i64) -> String {
    let s = (ms.max(0) + 500) / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// A `datetime-local` input's value for `at`, in the browser's zone.
pub fn local_input(at: &Timestamp) -> String {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(millis(*at) as f64));
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}",
        date.get_full_year(),
        date.get_month() + 1,
        date.get_date(),
        date.get_hours(),
        date.get_minutes()
    )
}

/// Reads a `datetime-local` input's value back, in the browser's zone.
pub fn parse_local(value: &str) -> Option<Timestamp> {
    let ms = js_sys::Date::parse(value);
    (!ms.is_nan()).then(|| from_millis(ms as i64))
}
