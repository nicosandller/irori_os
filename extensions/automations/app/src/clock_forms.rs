//! A time of day and the sun, as things to set by hand rather than write down
//! (`docs/specs/flows.md` §3, `rules.md` §5.2–5.3 and §6.3–6.4).
//!
//! A time is a hand on a 24-hour dial: noon at the top and midnight at the bottom, so the day
//! is where the sun would be, and the night half is shaded from the home's real sunrise and
//! sunset. The sun is a disc on its own arc over a horizon, with the six moments Irori knows
//! as stops on it.
//!
//! The inspector builds a form again after every edit, so nothing here can move by a CSS
//! transition on a kept element. What was shown last is remembered instead, and the new form
//! glides from it: the hand turns the short way round, the sun travels along its arc.
//!
//! When a time next comes round, and when the sun rises today, is asked of the engine
//! (`clock.next`). The page does no time zone arithmetic of its own.

use std::cell::Cell;
use std::collections::BTreeMap;

use irori_flow_types::cron::CronSpec;
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::windows::Windows;

type Edit = Box<dyn FnOnce(&mut Value)>;

/// How long a hand or the sun takes to get where it's going, in milliseconds: the page's
/// `--dur-expressive`.
const GLIDE_MS: f64 = 340.0;

/// The days of a week as they're written, and as a chip reads.
const DAYS: [(&str, &str, &str); 7] = [
    ("mon", "M", "Monday"),
    ("tue", "T", "Tuesday"),
    ("wed", "W", "Wednesday"),
    ("thu", "T", "Thursday"),
    ("fri", "F", "Friday"),
    ("sat", "S", "Saturday"),
    ("sun", "S", "Sunday"),
];

/// The sun's moments, in the order of a day: how each is written, read, and where it sits on
/// the arc as a part of the day (0 is solar midnight, a half is noon).
pub const SUN: [(&str, &str, f64); 6] = [
    ("dawn", "Dawn", 0.2),
    ("sunrise", "Sunrise", 0.25),
    ("noon", "Noon", 0.5),
    ("sunset", "Sunset", 0.75),
    ("dusk", "Dusk", 0.8),
    ("midnight", "Midnight", 0.0),
];

thread_local! {
    /// Where the dial's hand last pointed, in minutes of the day.
    static LAST_HAND: Cell<Option<f64>> = const { Cell::new(None) };
    /// Where the sun last stood on its arc, as a part of the day.
    static LAST_SUN: Cell<Option<f64>> = const { Cell::new(None) };
    /// Which way a time window was last said: between, after, or before.
    static LAST_SPAN: Cell<Option<f64>> = const { Cell::new(None) };
    /// Which end of a sun window a press on the arc sets: its start, or its end.
    static SUN_END: Cell<usize> = const { Cell::new(0) };
}

// ---- plain arithmetic, tested on the host ---------------------------------------------------

/// A time of day as minutes of it, however it was typed: `14:00`, `14.00`, `14`, `2pm`,
/// `2:30 pm`, and `HH:MM:SS` with its seconds left off.
pub fn minutes_of(text: &str) -> Option<i32> {
    let text = text.trim().to_ascii_lowercase();
    let (clock, half) = match text
        .strip_suffix("am")
        .or_else(|| text.strip_suffix("a.m."))
    {
        Some(clock) => (clock, Some(false)),
        None => match text
            .strip_suffix("pm")
            .or_else(|| text.strip_suffix("p.m."))
        {
            Some(clock) => (clock, Some(true)),
            None => (text.as_str(), None),
        },
    };
    let mut parts = clock.trim().split([':', '.']);
    let hour: i32 = parts.next()?.trim().parse().ok()?;
    let minute: i32 = match parts.next() {
        Some(minute) => minute.trim().parse().ok()?,
        None => 0,
    };
    let hour = match half {
        // Twelve on a twelve-hour clock is the start of its half: 12am is midnight.
        Some(afternoon) if (1..=12).contains(&hour) => hour % 12 + if afternoon { 12 } else { 0 },
        Some(_) => return None,
        None => hour,
    };
    ((0..24).contains(&hour) && (0..60).contains(&minute)).then_some(hour * 60 + minute)
}

/// Minutes of the day as `HH:MM`.
pub fn hhmm(minutes: i32) -> String {
    let minutes = minutes.rem_euclid(1440);
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// Where the hand points for a time, in degrees clockwise from straight up: noon is up,
/// midnight is down, six in the morning is to the left.
pub fn hand_angle(minutes: f64) -> f64 {
    minutes / 1440.0 * 360.0 - 180.0
}

/// The time a point around the dial stands for, to the nearest `step` minutes. `(dx, dy)` is
/// the point from the dial's middle, y downwards.
pub fn minutes_at(dx: f64, dy: f64, step: i32) -> i32 {
    // Clockwise from straight up.
    let angle = dx.atan2(-dy).to_degrees();
    let minutes = (angle + 180.0) / 360.0 * 1440.0;
    #[allow(clippy::cast_possible_truncation)]
    let snapped = (minutes / f64::from(step)).round() as i32 * step;
    snapped.rem_euclid(1440)
}

/// The short way from one time to another around the dial, in minutes: never more than half
/// a day either way.
pub fn short_way(from: f64, to: f64) -> f64 {
    (to - from + 720.0).rem_euclid(1440.0) - 720.0
}

/// A point on a circle of radius `r` around (100, 100), `minutes` into the day.
fn on_dial(minutes: f64, r: f64) -> (f64, f64) {
    let angle = hand_angle(minutes).to_radians();
    (100.0 + r * angle.sin(), 100.0 - r * angle.cos())
}

/// The ring segment from one time to another, clockwise, as an SVG path.
pub fn arc_path(from: f64, to: f64, outer: f64, inner: f64) -> String {
    let span = (to - from).rem_euclid(1440.0);
    let large = i32::from(span > 720.0);
    let (ax, ay) = on_dial(from, outer);
    let (bx, by) = on_dial(to, outer);
    let (cx, cy) = on_dial(to, inner);
    let (dx, dy) = on_dial(from, inner);
    format!(
        "M{ax:.2} {ay:.2}A{outer} {outer} 0 {large} 1 {bx:.2} {by:.2}L{cx:.2} {cy:.2}\
         A{inner} {inner} 0 {large} 0 {dx:.2} {dy:.2}Z"
    )
}

/// Which days a list of them is, in a few words.
pub fn days_words(days: &[String]) -> String {
    let has = |day: &str| days.iter().any(|d| d == day);
    let weekdays = ["mon", "tue", "wed", "thu", "fri"];
    let weekend = ["sat", "sun"];
    let all_of = |set: &[&str]| set.iter().all(|day| has(day));
    let none_of = |set: &[&str]| !set.iter().any(|day| has(day));
    if days.is_empty() || (all_of(&weekdays) && all_of(&weekend)) {
        "every day".to_owned()
    } else if all_of(&weekdays) && none_of(&weekend) {
        "on weekdays".to_owned()
    } else if all_of(&weekend) && none_of(&weekdays) {
        "at the weekend".to_owned()
    } else {
        let named: Vec<&str> = DAYS
            .iter()
            .filter(|(day, _, _)| has(day))
            .map(|(_, _, name)| *name)
            .collect();
        format!("on {}", list(&named))
    }
}

/// "a", "a and b", "a, b and c".
fn list(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [most @ .., last] => format!("{} and {last}", most.join(", ")),
    }
}

/// A compact duration (`-30m`, `1h30m`) as whole minutes, signed. `None` for one this form
/// can't hold: seconds, or more than a slider's worth.
pub fn offset_minutes(text: &str) -> Option<i32> {
    let text = text.trim();
    let (sign, mut rest) = match text.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, text.strip_prefix('+').unwrap_or(text)),
    };
    let mut total = 0;
    while !rest.is_empty() {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let amount: i32 = rest.get(..digits)?.parse().ok()?;
        let unit = rest.get(digits..digits + 1)?;
        // `ms` would be read as minutes; a form in minutes has no business with it.
        if rest.get(digits..digits + 2) == Some("ms") {
            return None;
        }
        total += match unit {
            "h" => amount * 60,
            "m" => amount,
            _ => return None,
        };
        rest = &rest[digits + 1..];
    }
    Some(sign * total)
}

/// Whole minutes as a compact duration, `None` for none at all.
pub fn offset_text(minutes: i32) -> Option<String> {
    let (sign, minutes) = (if minutes < 0 { "-" } else { "" }, minutes.abs());
    match (minutes / 60, minutes % 60) {
        (0, 0) => None,
        (0, m) => Some(format!("{sign}{m}m")),
        (h, 0) => Some(format!("{sign}{h}h")),
        (h, m) => Some(format!("{sign}{h}h{m}m")),
    }
}

/// "30 min", "1 h", "1 h 30 min".
fn span_words(minutes: i32) -> String {
    match (minutes.abs() / 60, minutes.abs() % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// A sun event as it's said in a sentence.
pub fn sun_word(event: &str) -> &'static str {
    match event {
        "dawn" => "dawn",
        "sunrise" => "sunrise",
        "noon" => "solar noon",
        "sunset" => "sunset",
        "dusk" => "dusk",
        "midnight" => "solar midnight",
        _ => "the sun",
    }
}

/// "at sunset", "30 min before sunset", "1 h after dawn".
pub fn sun_words(event: &str, offset: i32) -> String {
    let event = sun_word(event);
    match offset {
        0 => format!("at {event}"),
        early if early < 0 => format!("{} before {event}", span_words(early)),
        late => format!("{} after {event}", span_words(late)),
    }
}

/// One end of a sun window: "sunset", "30 min before sunset". An offset the words can't hold
/// (seconds) is said as it's written.
pub fn sun_end_words(event: &str, offset: Option<&str>) -> String {
    match offset.map(|text| (text, offset_minutes(text))) {
        None | Some((_, Some(0))) => sun_word(event).to_owned(),
        Some((_, Some(minutes))) => sun_words(event, minutes),
        Some((text, None)) => format!("{} ({text})", sun_word(event)),
    }
}

/// The offset an end of a sun window has: its own, or the one that moves both ends.
fn end_offset<'a>(window: &'a Value, own: &str) -> Option<&'a str> {
    window[own].as_str().or_else(|| window["offset"].as_str())
}

/// A window of the day in a few words: "between 22:00 and 06:00 on weekdays", "after 14:00",
/// "from 30 min before sunset until sunrise". `window` is a `time` or a `sun` condition.
pub fn window_words(window: &Value) -> String {
    let (after, before) = (window["after"].as_str(), window["before"].as_str());
    if window["type"] == "sun" {
        let from = after.map(|event| sun_end_words(event, end_offset(window, "after_offset")));
        let until = before.map(|event| sun_end_words(event, end_offset(window, "before_offset")));
        return match (from, until) {
            (Some(from), Some(until)) => format!("from {from} until {until}"),
            (Some(from), None) => format!("from {from} on"),
            (None, Some(until)) => format!("until {until}"),
            (None, None) => "whatever the sun is doing".to_owned(),
        };
    }
    // A time is said the way the dial reads it, whatever seconds it was written with.
    let said = |text: &str| minutes_of(text).map_or_else(|| text.to_owned(), hhmm);
    let hours = match (after.map(said), before.map(said)) {
        (Some(after), Some(before)) => Some(format!("between {after} and {before}")),
        (Some(after), None) => Some(format!("after {after}")),
        (None, Some(before)) => Some(format!("before {before}")),
        (None, None) => None,
    };
    let days = days_of(window);
    match (hours, days.is_empty()) {
        (Some(hours), true) => hours,
        (Some(hours), false) => format!("{hours} {}", days_words(&days)),
        (None, _) => days_words(&days),
    }
}

/// A time trigger in a sentence: "at 07:00 on weekdays".
pub fn time_words(at: &str, days: &[String]) -> String {
    format!("at {at} {}", days_words(days))
}

/// A cron in plain words, or what's wrong with it.
pub fn cron_words(cron: &str) -> Result<String, String> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const WEEK: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let spec = CronSpec::parse(cron)?;
    let numbers = |values: &[u8]| -> String {
        let words: Vec<String> = values.iter().map(|v| format!("{v:02}")).collect();
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        list(&words)
    };
    let named = |values: &[u8], names: &[&str], first: u8| -> String {
        let words: Vec<&str> = values
            .iter()
            .filter_map(|v| names.get(usize::from(v.saturating_sub(first))).copied())
            .collect();
        list(&words)
    };
    let when = match (spec.hours.as_slice(), spec.minutes.as_slice()) {
        ([hour], [minute]) => format!("at {hour:02}:{minute:02}"),
        (hours, minutes) if hours.len() == 24 && minutes.len() == 60 => "every minute".to_owned(),
        (hours, [minute]) if hours.len() == 24 => format!("every hour at :{minute:02}"),
        (hours, minutes) if hours.len() == 24 => format!("every hour at :{}", numbers(minutes)),
        (hours, minutes) if minutes.len() == 60 => {
            format!("every minute of hour {}", numbers(hours))
        }
        (hours, minutes) => format!("at :{} past {}", numbers(minutes), numbers(hours)),
    };
    let mut days = Vec::new();
    if spec.days_given {
        let dates: Vec<String> = spec.days.iter().map(|d| d.to_string()).collect();
        let dates: Vec<&str> = dates.iter().map(String::as_str).collect();
        days.push(format!("on day {} of the month", list(&dates)));
    }
    if spec.weekdays_given {
        days.push(format!("on {}", named(&spec.weekdays, &WEEK, 0)));
    }
    let days = match days.as_slice() {
        [] => "every day".to_owned(),
        // Cron's own rule: with both given, either is enough.
        both => both.join(" or "),
    };
    let months = if spec.months.len() == 12 {
        String::new()
    } else {
        format!(", in {}", named(&spec.months, &MONTHS, 1))
    };
    Ok(format!("{when}, {days}{months}"))
}

/// Where the sun stands on its arc `part` of the way through a day (0 is midnight, a half is
/// noon), in a drawing 280 wide and 150 high with the horizon at 100.
pub fn sun_at(part: f64) -> (f64, f64) {
    let angle = part.rem_euclid(1.0) * std::f64::consts::TAU;
    // A tall arc by day and a shallow one under the horizon: the night is only hinted at.
    let rise = if angle.cos() < 0.0 { 78.0 } else { 34.0 };
    (140.0 - 104.0 * angle.sin(), 100.0 + rise * angle.cos())
}

/// How far above the horizon the sun is, from 0 (at or under it) to 1 (noon).
pub fn height(part: f64) -> f64 {
    (-(part.rem_euclid(1.0) * std::f64::consts::TAU).cos()).max(0.0)
}

// ---- moving ---------------------------------------------------------------------------------

/// Whether to land at once: the page has been asked to keep still, or nobody is looking. A
/// page out of sight is given no animation frames, and a hand left half way would be wrong
/// for as long as it stayed there.
fn still() -> bool {
    let document = document();
    document.hidden()
        || document
            .document_element()
            .and_then(|root| root.get_attribute("data-motion"))
            .as_deref()
            == Some("off")
        || window()
            .match_media("(prefers-reduced-motion: reduce)")
            .ok()
            .flatten()
            .is_some_and(|query| query.matches())
}

/// Takes `shown` from where it is to `to` over [`GLIDE_MS`], arriving fast and settling long.
/// `to` is given as how far to travel, so a hand can go the short way round.
fn glide(shown: RwSignal<f64>, by: f64) {
    let from = shown.get_untracked();
    if by.abs() < f64::EPSILON {
        return;
    }
    if still() {
        shown.set(from + by);
        return;
    }
    let started = js_sys::Date::now();
    fn frame(shown: RwSignal<f64>, from: f64, by: f64, started: f64) {
        let t = ((js_sys::Date::now() - started) / GLIDE_MS).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - t).powi(4);
        // The form may have been built again, and this one put away.
        if shown.try_set(from + by * eased).is_some() {
            return;
        }
        if t < 1.0 {
            request_animation_frame(move || frame(shown, from, by, started));
        }
    }
    request_animation_frame(move || frame(shown, from, by, started));
}

// ---- what the engine says -------------------------------------------------------------------

/// What `clock.next` answers.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Clock {
    #[serde(default)]
    pub time_zone: Option<String>,
    #[serde(default)]
    pub location: bool,
    #[serde(default)]
    pub spoken: Option<String>,
    #[serde(default)]
    pub problem: Option<String>,
    #[serde(default)]
    pub now: Option<String>,
    /// Today's sun at home, each as `HH:MM`.
    #[serde(default)]
    pub sun: BTreeMap<String, String>,
}

/// Asks the engine about `trigger`, once, as the form is built.
fn ask(trigger: Value) -> RwSignal<Option<Clock>> {
    let clock = RwSignal::new(None);
    spawn_local(async move {
        let answer: Result<Clock, String> = crate::bridge()
            .rpc("clock.next", json!({ "trigger": trigger }))
            .await;
        if let Ok(answer) = answer {
            let _ = clock.try_set(Some(answer));
        }
    });
    clock
}

/// The line under a form: when it next fires, or what the home is missing for it to.
fn next_line(clock: RwSignal<Option<Clock>>, needs_location: bool) -> impl IntoView {
    move || {
        let clock = clock.get()?;
        let line = if clock.time_zone.is_none() {
            view! {
                <p class="clock-next missing">
                    "This can't fire yet: the home has no time zone. Set it in "
                    <strong>"Settings → Location and time zone"</strong>"."
                </p>
            }
            .into_any()
        } else if needs_location && !clock.location {
            view! {
                <p class="clock-next missing">
                    "This can't fire yet: Irori doesn't know where the home is. Set it in "
                    <strong>"Settings → Location and time zone"</strong>"."
                </p>
            }
            .into_any()
        } else if let Some(problem) = clock.problem {
            view! { <p class="clock-next missing">{problem}</p> }.into_any()
        } else {
            let zone = clock.time_zone.unwrap_or_default();
            match clock.spoken {
                Some(spoken) => view! {
                    <p class="clock-next">
                        <span class="clock-next-dot" aria-hidden="true"></span>
                        "Next: " <strong>{spoken}</strong>
                        <span class="muted">" · " {zone}</span>
                    </p>
                }
                .into_any(),
                None => view! {
                    <p class="clock-next missing">
                        "This doesn't happen at home in the coming year."
                    </p>
                }
                .into_any(),
            }
        };
        Some(line)
    }
}

/// The line under a window's form: whether it holds right now by the clock at home, or what
/// the home is missing for it ever to.
fn holds_line(window: Value, needs_location: bool) -> impl IntoView {
    let windows = expect_context::<Windows>();
    move || {
        let told = windows.told(&window);
        // Nothing until the engine has answered: a line that arrives says more than one that
        // changes its mind.
        let at_home = windows.at_home.get()?;
        let missing = |what: &'static str| {
            view! {
                <p class="clock-next missing">
                    "This never holds yet: " {what} " Set it in "
                    <strong>"Settings → Location and time zone"</strong>"."
                </p>
            }
            .into_any()
        };
        let line = if at_home.time_zone.is_none() {
            missing("the home has no time zone.")
        } else if needs_location && !at_home.location {
            missing("Irori doesn't know where the home is.")
        } else {
            let told = told?;
            let clock = at_home.now.unwrap_or_default();
            let zone = at_home.time_zone.unwrap_or_default();
            match (told.holds, told.why) {
                (Some(holds), _) => view! {
                    <p class="clock-next">
                        <span class=if holds { "check-dot holds" } else { "check-dot fails" }
                            aria-hidden="true"></span>
                        <strong>{if holds { "Holds now" } else { "Doesn't hold now" }}</strong>
                        <span class="muted">" · " {clock} " at home · " {zone}</span>
                    </p>
                }
                .into_any(),
                (None, Some(why)) => view! { <p class="clock-next missing">{why}</p> }.into_any(),
                (None, None) => return None,
            }
        };
        Some(line)
    }
}

// ---- a time of day --------------------------------------------------------------------------

fn days_of(value: &Value) -> Vec<String> {
    value["weekday"]
        .as_array()
        .map(|days| {
            days.iter()
                .filter_map(|day| day.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Writes which days, in the week's own order; every day, or none, is no `weekday` at all.
fn write_days(value: &mut Value, days: &[String]) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let ordered: Vec<&str> = DAYS
        .iter()
        .map(|(day, _, _)| *day)
        .filter(|day| days.iter().any(|d| d == day))
        .collect();
    if ordered.is_empty() || ordered.len() == 7 {
        object.remove("weekday");
    } else {
        object.insert("weekday".into(), json!(ordered));
    }
}

/// Seven chips, and three ways to set them all at once.
fn days_field(value: &Value, edit: impl Fn(Edit) + Clone + Send + Sync + 'static) -> impl IntoView {
    let chosen = days_of(value);
    // No list means every day, and the chips say so by all being lit.
    let lit = |day: &str| chosen.is_empty() || chosen.iter().any(|d| d == day);
    let all: Vec<String> = DAYS.iter().map(|(day, _, _)| (*day).to_owned()).collect();
    let set = {
        let edit = edit.clone();
        move |days: Vec<String>| edit(Box::new(move |v: &mut Value| write_days(v, &days)))
    };
    let shortcut = |label: &'static str, days: &[&str]| {
        let days: Vec<String> = days.iter().map(|day| (*day).to_owned()).collect();
        let now = if chosen.is_empty() {
            all.clone()
        } else {
            chosen.clone()
        };
        let pressed = now.len() == days.len() && days.iter().all(|day| now.contains(day));
        let set = set.clone();
        view! {
            <button type="button" class="day-set" aria-pressed=pressed.to_string()
                on:click=move |_| set(days.clone())>
                {label}
            </button>
        }
    };
    view! {
        <label>"On"</label>
        <div class="days" role="group" aria-label="Days of the week">
            {DAYS
                .iter()
                .map(|(day, letter, name)| {
                    let on = lit(day);
                    let set = set.clone();
                    let (all, chosen) = (all.clone(), chosen.clone());
                    view! {
                        <button type="button" class="day" title=*name aria-label=*name
                            aria-pressed=on.to_string()
                            on:click=move |_| {
                                let mut days = if chosen.is_empty() { all.clone() } else { chosen.clone() };
                                if on {
                                    days.retain(|d| d != day);
                                } else {
                                    days.push((*day).to_owned());
                                }
                                // The last day can't be taken away: a time on no day is no time.
                                if !days.is_empty() {
                                    set(days);
                                }
                            }>
                            {*letter}
                        </button>
                    }
                })
                .collect_view()}
        </div>
        <div class="day-sets">
            {shortcut("Every day", &["mon", "tue", "wed", "thu", "fri", "sat", "sun"])}
            {shortcut("Weekdays", &["mon", "tue", "wed", "thu", "fri"])}
            {shortcut("Weekends", &["sat", "sun"])}
        </div>
    }
}

/// What of the dial's ring is lit: the stretch of the day a window holds for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lit {
    /// Nothing: a trigger is a moment, not a stretch.
    Nothing,
    /// From the first hand round to the second.
    Between,
    /// From the hand to the day's end.
    After,
    /// From the day's start to the hand.
    Before,
}

/// The 24-hour dial. `hands` are the times on it (one for a trigger or an open window, two
/// for a window with both ends); dragging the dial moves the one nearest the pointer.
/// `commit` is handed which hand and its new time once it is let go.
fn dial(
    hands: Vec<(&'static str, i32)>,
    lit: Lit,
    clock: RwSignal<Option<Clock>>,
    commit: impl Fn(usize, i32) + Clone + 'static,
) -> impl IntoView {
    let face = NodeRef::<leptos::svg::Svg>::new();
    // Where each hand is drawn: its time, or wherever a drag has taken it.
    let shown: Vec<RwSignal<f64>> = hands
        .iter()
        .enumerate()
        .map(|(i, (_, minutes))| {
            let to = f64::from(*minutes);
            // Only the first hand remembers where it was: it is the one a typed time moves.
            let from = if i == 0 {
                LAST_HAND.with(Cell::get).unwrap_or(to)
            } else {
                to
            };
            let shown = RwSignal::new(from);
            glide(shown, short_way(from, to));
            shown
        })
        .collect();
    if let Some((_, minutes)) = hands.first() {
        LAST_HAND.with(|last| last.set(Some(f64::from(*minutes))));
    }
    let held = RwSignal::new(None::<usize>);
    let shown = StoredValue::new_local(shown);

    let minutes_under = move |event: &ev::PointerEvent| -> Option<i32> {
        let rect = face.get_untracked()?.get_bounding_client_rect();
        let dx = f64::from(event.client_x()) - (rect.left() + rect.width() / 2.0);
        let dy = f64::from(event.client_y()) - (rect.top() + rect.height() / 2.0);
        Some(minutes_at(dx, dy, 5))
    };
    let down = move |event: ev::PointerEvent| {
        // A drag across the dial is a hand being turned, not text being selected: without
        // this the hours, and whatever is around the dial, light up blue as it goes.
        event.prevent_default();
        let Some(minutes) = minutes_under(&event) else {
            return;
        };
        if let Some(face) = face.get_untracked() {
            let _ = face.set_pointer_capture(event.pointer_id());
        }
        // The hand nearest the pointer is the one being asked for.
        let nearest = shown.with_value(|shown| {
            shown
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    let away = |hand: &RwSignal<f64>| {
                        short_way(hand.get_untracked(), f64::from(minutes)).abs()
                    };
                    away(a).total_cmp(&away(b))
                })
                .map(|(i, _)| i)
        });
        held.set(nearest);
        if let Some(i) = nearest {
            shown.with_value(|shown| shown[i].set(f64::from(minutes)));
        }
    };
    let moved = move |event: ev::PointerEvent| {
        if let (Some(i), Some(minutes)) = (held.get_untracked(), minutes_under(&event)) {
            shown.with_value(|shown| shown[i].set(f64::from(minutes)));
        }
    };
    let up = move |event: ev::PointerEvent| {
        let Some(i) = held.get_untracked() else {
            return;
        };
        held.set(None);
        if let Some(minutes) = minutes_under(&event) {
            // Already there: the form that is built next has nothing to glide from.
            if i == 0 {
                LAST_HAND.with(|last| last.set(Some(f64::from(minutes))));
            }
            commit(i, minutes);
        }
    };

    let night = move || {
        let sun = clock.get()?.sun;
        let (rise, set) = (
            minutes_of(sun.get("sunrise")?)?,
            minutes_of(sun.get("sunset")?)?,
        );
        Some(arc_path(f64::from(set), f64::from(rise), 92.0, 70.0))
    };
    let readout = move || {
        shown.with_value(|shown| {
            #[allow(clippy::cast_possible_truncation)]
            let text: Vec<String> = shown
                .iter()
                .map(|hand| hhmm(hand.get().round() as i32))
                .collect();
            text.join(" – ")
        })
    };
    let window = hands.len() == 2;
    let span = move || {
        shown.with_value(|shown| {
            let hand = |i: usize| shown.get(i).map(RwSignal::get);
            // Midnight is where the day ends and starts: the bottom of the dial, both times.
            let (from, to) = match lit {
                Lit::Nothing => return None,
                Lit::Between => (hand(0)?, hand(1)?),
                Lit::After => (hand(0)?, 1440.0),
                Lit::Before => (0.0, hand(0)?),
            };
            // A window with no length is not drawn as one a whole day long.
            ((to - from).rem_euclid(1440.0) > 0.5).then(|| arc_path(from, to, 92.0, 70.0))
        })
    };

    view! {
        <svg class="dial" class:holding=move || held.get().is_some() viewBox="0 0 200 200"
            node_ref=face role="img" aria-label="A 24-hour dial"
            on:pointerdown=down on:pointermove=moved on:pointerup=up
            on:pointercancel=move |_| held.set(None)>
            <circle class="dial-ring" cx="100" cy="100" r="81" />
            // The night, from the home's own sunset round to its sunrise.
            {move || night().map(|d| view! { <path class="dial-night" d=d /> })}
            {move || span().map(|d| view! { <path class="dial-span" d=d /> })}
            {(0..24)
                .map(|hour| {
                    let (x1, y1) = on_dial(f64::from(hour * 60), if hour % 6 == 0 { 64.0 } else { 67.0 });
                    let (x2, y2) = on_dial(f64::from(hour * 60), 70.0);
                    view! { <line class="dial-tick" class:major={hour % 6 == 0} x1=x1 y1=y1 x2=x2 y2=y2 /> }
                })
                .collect_view()}
            {[(0, "00"), (6, "06"), (12, "12"), (18, "18")]
                .into_iter()
                .map(|(hour, label)| {
                    let (x, y) = on_dial(f64::from(hour * 60), 54.0);
                    view! { <text class="dial-hour" x=x y=y>{label}</text> }
                })
                .collect_view()}
            {shown.with_value(|shown| {
                shown
                    .iter()
                    .enumerate()
                    .map(|(i, hand)| {
                        let hand = *hand;
                        let label = hands[i].0;
                        view! {
                            <g class="dial-hand" class:held=move || held.get() == Some(i)
                                style=move || format!("rotate: {:.2}deg", hand_angle(hand.get()))>
                                <line x1="100" y1="100" x2="100" y2="19" />
                                <circle class="dial-knob" cx="100" cy="19" r="9" />
                                <title>{label}</title>
                            </g>
                        }
                    })
                    .collect_view()
            })}
            <circle class="dial-hub" cx="100" cy="100" r="3.5" />
            <text class="dial-time" x="100" y="100" class:pair=window>{readout}</text>
        </svg>
    }
}

/// A time of day: the dial, the time typed, and which days. Or, for a schedule the dial can't
/// say, a cron and what it means.
pub fn time_form(trigger: &Value, edit: impl Fn(Edit) + Clone + Send + Sync + 'static) -> AnyView {
    let clock = ask(trigger.clone());
    if let Some(cron) = trigger["cron"].as_str() {
        let (cron, typed) = (cron.to_owned(), edit.clone());
        let back = edit.clone();
        let words = cron_words(&cron);
        return view! {
            <label>"Schedule (cron: minute hour day month weekday)"</label>
            <input type="text" class="mono" prop:value=cron.clone() spellcheck="false"
                on:change=move |event| {
                    let text = event_target_value(&event).trim().to_owned();
                    typed(Box::new(move |t: &mut Value| t["cron"] = json!(text)));
                } />
            {match words {
                Ok(words) => view! { <p class="clock-reads">{words}</p> }.into_any(),
                Err(why) => view! { <p class="clock-next missing">{why}</p> }.into_any(),
            }}
            {next_line(clock, false)}
            <button type="button" class="link clock-switch"
                on:click=move |_| back(Box::new(|t: &mut Value| {
                    *t = json!({ "type": "time", "at": "07:00" });
                }))>
                "Use a time of day instead"
            </button>
        }
        .into_any();
    }
    let at = trigger["at"].as_str().unwrap_or("07:00").to_owned();
    let minutes = minutes_of(&at).unwrap_or(7 * 60);
    let (turned, typed, cron) = (edit.clone(), edit.clone(), edit.clone());
    let trouble = RwSignal::new(false);
    view! {
        <div class="clock-form">
            {dial(vec![("The time", minutes)], Lit::Nothing, clock, move |_, minutes| {
                let text = hhmm(minutes);
                turned(Box::new(move |t: &mut Value| t["at"] = json!(text)));
            })}
            <div class="clock-side">
                <label>"At"</label>
                <input type="text" class="clock-typed" class:wrong=move || trouble.get()
                    inputmode="numeric" placeholder="07:00" prop:value=hhmm(minutes)
                    aria-label="Time, as hours and minutes"
                    on:change=move |event| match minutes_of(&event_target_value(&event)) {
                        Some(minutes) => {
                            trouble.set(false);
                            let text = hhmm(minutes);
                            typed(Box::new(move |t: &mut Value| t["at"] = json!(text)));
                        }
                        None => trouble.set(true),
                    } />
                <p class="muted clock-hint">"Drag the hand, or type a time. By the clock at home."</p>
            </div>
        </div>
        {days_field(trigger, edit)}
        {next_line(clock, false)}
        <button type="button" class="link clock-switch"
            on:click=move |_| {
                let (hour, minute) = (minutes / 60, minutes % 60);
                cron(Box::new(move |t: &mut Value| {
                    *t = json!({ "type": "time", "cron": format!("{minute} {hour} * * *") });
                }));
            }>
            "Something the dial can't say? Use a cron schedule"
        </button>
    }
    .into_any()
}

/// The three ways a time window is said, as one switch. The chosen one slides under its
/// name from wherever it last was: the form is built again by the edit that chose it.
fn span_switch(chosen: Option<usize>, pick: impl Fn(usize) + Clone + 'static) -> impl IntoView {
    let to = chosen.map(|index| index as f64);
    let from = LAST_SPAN.with(Cell::get).or(to).unwrap_or(0.0);
    LAST_SPAN.with(|last| last.set(to));
    let at = RwSignal::new(from);
    if let Some(to) = to {
        glide(at, to - from);
    }
    view! {
        <div class="spans" role="group" aria-label="Which part of the day">
            {chosen.map(|_| view! {
                <span class="spans-thumb" style=move || format!("--at: {:.3}", at.get())></span>
            })}
            {["Between", "After", "Before"]
                .into_iter()
                .enumerate()
                .map(|(index, label)| {
                    let pick = pick.clone();
                    view! {
                        <button type="button" class:on={chosen == Some(index)}
                            aria-pressed=(chosen == Some(index)).to_string()
                            on:click=move |_| pick(index)>
                            {label}
                        </button>
                    }
                })
                .collect_view()}
        </div>
    }
}

/// A window of time: between two times, after one, or before one; on some days. The same
/// dial as a trigger's, with the stretch of the day it holds for lit.
pub fn time_window_form(
    condition: &Value,
    edit: impl Fn(Edit) + Clone + Send + Sync + 'static,
) -> AnyView {
    // The dial's night comes from the sun at home; any trigger asks for it.
    let clock = ask(json!({ "type": "time", "at": "12:00" }));
    let after = condition["after"].as_str().and_then(minutes_of);
    let before = condition["before"].as_str().and_then(minutes_of);
    let (chosen, lit, hands) = match (after, before) {
        (Some(after), Some(before)) => (
            Some(0),
            Lit::Between,
            vec![("From", "after", after), ("Until", "before", before)],
        ),
        (Some(after), None) => (Some(1), Lit::After, vec![("After", "after", after)]),
        (None, Some(before)) => (Some(2), Lit::Before, vec![("Before", "before", before)]),
        // Days alone: any time of day, on those days.
        (None, None) => (None, Lit::Nothing, Vec::new()),
    };
    let switch = {
        let edit = edit.clone();
        move |to: usize| {
            if chosen == Some(to) {
                return;
            }
            edit(Box::new(move |c: &mut Value| {
                // The time that's there stays where it is; the one that's missing starts an
                // hour from it, so the new window is somewhere near the old one.
                let (after, before) = match (after, before) {
                    (Some(after), Some(before)) => (after, before),
                    (Some(after), None) => (after, after + 60),
                    (None, Some(before)) => (before - 60, before),
                    (None, None) => (22 * 60, 6 * 60),
                };
                let Some(object) = c.as_object_mut() else {
                    return;
                };
                object.remove("after");
                object.remove("before");
                match to {
                    0 => {
                        object.insert("after".into(), json!(hhmm(after)));
                        object.insert("before".into(), json!(hhmm(before)));
                    }
                    // Whichever time the eye was on is the one that's kept.
                    1 => {
                        object.insert("after".into(), json!(hhmm(after)));
                    }
                    _ => {
                        let kept = if chosen == Some(1) { after } else { before };
                        object.insert("before".into(), json!(hhmm(kept)));
                    }
                }
            }));
        }
    };
    let keys: Vec<&'static str> = hands.iter().map(|(_, key, _)| *key).collect();
    let turned = edit.clone();
    let fields = hands
        .iter()
        .map(|(label, key, minutes)| {
            let (label, key, minutes) = (*label, *key, *minutes);
            let edit = edit.clone();
            let trouble = RwSignal::new(false);
            view! {
                <div>
                    <label>{label}</label>
                    <input type="text" class="clock-typed" class:wrong=move || trouble.get()
                        inputmode="text" prop:value=hhmm(minutes)
                        aria-label=format!("{label}, as a time of day")
                        on:change=move |event| match minutes_of(&event_target_value(&event)) {
                            Some(minutes) => {
                                trouble.set(false);
                                let text = hhmm(minutes);
                                edit(Box::new(move |c: &mut Value| c[key] = json!(text)));
                            }
                            None => trouble.set(true),
                        } />
                </div>
            }
        })
        .collect_view();
    let said = match (after, before) {
        (Some(after), Some(before)) if after > before => {
            "Through the night: from the first time, past midnight, until the second."
        }
        (Some(after), Some(before)) if after == before => {
            "The same time twice is no time at all: move one of them."
        }
        (Some(_), Some(_)) => "From the first time until the second.",
        (Some(_), None) => "From then until midnight.",
        (None, Some(_)) => "From midnight until then.",
        (None, None) => "Any time of day, on the days below.",
    };
    view! {
        {span_switch(chosen, switch)}
        <div class="clock-form">
            {dial(
                hands.iter().map(|(label, _, minutes)| (*label, *minutes)).collect(),
                lit,
                clock,
                move |hand, minutes| {
                    let (Some(key), text) = (keys.get(hand).copied(), hhmm(minutes)) else {
                        return;
                    };
                    turned(Box::new(move |c: &mut Value| c[key] = json!(text)));
                },
            )}
            <div class="clock-side">
                {fields}
                <p class="muted clock-hint">{said}" Type it like 14:00 or 2pm."</p>
            </div>
        </div>
        {days_field(condition, edit.clone())}
        {holds_line(condition.clone(), false)}
    }
    .into_any()
}

// ---- the sun --------------------------------------------------------------------------------

/// Where on the arc an event stands, with its offset nudging it along: an hour is a
/// twenty-fourth of the way round.
fn part_of(event: &str, offset: i32) -> f64 {
    let stop = SUN
        .iter()
        .find(|(name, _, _)| *name == event)
        .map_or(0.75, |(_, _, part)| *part);
    stop + f64::from(offset) / 1440.0
}

/// The sun on its arc. `at` is where it stands, `chosen` the moments that are picked, and
/// `lit` the stretch of the day a window holds for, as the parts of the day it runs between.
fn arc(
    at: RwSignal<f64>,
    chosen: Vec<&'static str>,
    lit: Signal<Option<(f64, f64)>>,
    pick: impl Fn(&'static str) + Clone + 'static,
) -> impl IntoView {
    // The day's path over the horizon, and the night's under it, as drawn lines.
    let path = |from: f64, to: f64| -> String {
        (0..=48)
            .map(|i| {
                let (x, y) = sun_at(from + (to - from) * f64::from(i) / 48.0);
                format!("{}{x:.1} {y:.1}", if i == 0 { "M" } else { "L" })
            })
            .collect()
    };
    view! {
        <svg class="sun-arc" viewBox="0 0 280 150" role="img" aria-label="The sun's day">
            // The sky warms as the sun climbs and goes dark as it sinks.
            <rect class="sun-sky" x="0" y="0" width="280" height="100" rx="8"
                style=move || format!("opacity: {:.3}", height(at.get()) * 0.9) />
            <rect class="sun-night" x="0" y="0" width="280" height="150" rx="8"
                style=move || format!("opacity: {:.3}", (1.0 - height(at.get()) * 4.0).clamp(0.0, 1.0) * 0.55) />
            <path class="sun-path day" d=path(0.25, 0.75) />
            <path class="sun-path night" d=path(0.75, 1.25) />
            // The window: along the sun's own path from where it opens to where it closes,
            // on through the night if that is the way round.
            {move || lit.get().map(|(from, to)| {
                let (from, to) = (from.rem_euclid(1.0), to.rem_euclid(1.0));
                let to = if to > from { to } else { to + 1.0 };
                view! { <path class="sun-span" d=path(from, to) /> }
            })}
            <line class="sun-horizon" x1="12" y1="100" x2="268" y2="100" />
            {SUN
                .iter()
                .map(|(name, label, part)| {
                    let (x, y) = sun_at(*part);
                    let on = chosen.contains(name);
                    let pick = pick.clone();
                    // Labels stand clear of the line: above it by day, beside it at the ends.
                    let (lx, ly, anchor) = match *name {
                        "noon" => (x, y - 12.0, "middle"),
                        "midnight" => (x, y + 13.0, "middle"),
                        "dawn" | "sunrise" => (x - 9.0, if *name == "dawn" { y + 11.0 } else { y - 7.0 }, "end"),
                        _ => (x + 9.0, if *name == "dusk" { y + 11.0 } else { y - 7.0 }, "start"),
                    };
                    view! {
                        <g class="sun-stop" class:chosen=on on:click=move |_| pick(name)>
                            <circle class="sun-stop-hit" cx=x cy=y r="12" />
                            <circle class="sun-stop-dot" cx=x cy=y r="3.5" />
                            <text x=lx y=ly text-anchor=anchor>{*label}</text>
                        </g>
                    }
                })
                .collect_view()}
            <g class="sun-disc" style=move || {
                let (x, y) = sun_at(at.get());
                format!("translate: {x:.2}px {y:.2}px")
            }>
                <circle class="sun-glow" r="15" />
                <circle class="sun-body" r="8" />
            </g>
        </svg>
    }
}

/// The sun, coming from wherever it last stood to `part`.
fn travelling(part: f64) -> RwSignal<f64> {
    let from = LAST_SUN.with(Cell::get).unwrap_or(part);
    LAST_SUN.with(|last| last.set(Some(part)));
    let at = RwSignal::new(from);
    // The short way along the day: sunset to dusk goes on, not back through the morning.
    glide(at, (part - from + 0.5).rem_euclid(1.0) - 0.5);
    at
}

/// An offset as a slider, from two hours before to two hours after, with what it reads as.
/// `said` puts the minutes into words; `moved` follows the thumb while it's dragged, and
/// `commit` is handed the minutes once it's let go.
fn offset_field(
    offset: i32,
    said: impl Fn(i32) -> String + Send + Sync + 'static,
    moved: impl Fn(i32) + 'static,
    commit: impl Fn(i32) + 'static,
) -> impl IntoView {
    let live = RwSignal::new(offset);
    view! {
        <label>{move || said(live.get())}</label>
        <input type="range" class="sun-offset" min="-120" max="120" step="5"
            prop:value=offset.to_string()
            style=move || format!("--fill: {:.1}%", f64::from(live.get() + 120) / 240.0 * 100.0)
            aria-label="How long before or after"
            // While it's dragged the sun goes with it; letting go is what writes it down.
            on:input=move |input| {
                if let Ok(minutes) = event_target_value(&input).parse::<i32>() {
                    live.set(minutes);
                    moved(minutes);
                }
            }
            on:change=move |input| {
                if let Ok(minutes) = event_target_value(&input).parse::<i32>() {
                    commit(minutes);
                }
            } />
        <div class="sun-offset-ends muted"><span>"2 h before"</span><span>"2 h after"</span></div>
    }
}

/// An offset the slider can't hold (seconds, or half a day), typed as it's written.
fn offset_typed(
    label: &'static str,
    written: String,
    commit: impl Fn(Option<String>) + 'static,
) -> impl IntoView {
    view! {
        <label>{label}</label>
        <input type="text" class="mono" prop:value=written
            on:change=move |input| {
                let text = event_target_value(&input).trim().to_owned();
                commit((!text.is_empty()).then_some(text));
            } />
    }
}

/// Today's time for each of the sun's moments, as chips to pick from.
fn sun_chips(
    clock: RwSignal<Option<Clock>>,
    chosen: Signal<Option<String>>,
    pick: impl Fn(&'static str) + Clone + 'static,
) -> impl IntoView {
    view! {
        <div class="sun-chips" role="group" aria-label="Which moment">
            {SUN
                .iter()
                .map(|(name, label, _)| {
                    let pick = pick.clone();
                    let today = move || {
                        clock.get().and_then(|clock| clock.sun.get(*name).cloned())
                    };
                    view! {
                        <button type="button" class="sun-chip"
                            aria-pressed=move || (chosen.get().as_deref() == Some(*name)).to_string()
                            on:click=move |_| pick(name)>
                            <span>{*label}</span>
                            <span class="sun-chip-time">
                                {move || today().unwrap_or_else(|| "–".to_owned())}
                            </span>
                        </button>
                    }
                })
                .collect_view()}
        </div>
    }
}

/// Writes `text` at `key`, or takes `key` away when there is nothing to write.
fn write_or_remove(value: &mut Value, key: &str, text: Option<String>) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    match text {
        Some(text) => {
            object.insert(key.to_owned(), json!(text));
        }
        None => {
            object.remove(key);
        }
    }
}

/// The sun as a trigger: which moment, and how long before or after it.
pub fn sun_form(trigger: &Value, edit: impl Fn(Edit) + Clone + Send + Sync + 'static) -> AnyView {
    let clock = ask(trigger.clone());
    let event = trigger["event"].as_str().unwrap_or("sunset").to_owned();
    let offset_as_written = trigger["offset"].as_str().map(str::to_owned);
    let offset = offset_as_written.as_deref().map(offset_minutes);
    let minutes = offset.flatten().unwrap_or(0).clamp(-120, 120);
    let at = travelling(part_of(&event, minutes));
    let pick = {
        let edit = edit.clone();
        move |name: &'static str| edit(Box::new(move |t: &mut Value| t["event"] = json!(name)))
    };
    // An offset the slider can't hold (seconds, or half a day) is shown as it's written.
    let beyond =
        matches!(offset, Some(None)) || offset.flatten().is_some_and(|minutes| minutes.abs() > 120);
    let chosen = Signal::stored(Some(event.clone()));
    view! {
        {arc(
            at,
            SUN.iter().map(|(name, _, _)| *name).filter(|name| *name == event).collect(),
            Signal::stored(None),
            pick.clone(),
        )}
        {sun_chips(clock, chosen, pick)}
        {if beyond {
            offset_typed(
                "Before or after it (like -45m or 1h30m)",
                offset_as_written.unwrap_or_default(),
                move |text| edit(Box::new(move |t: &mut Value| write_or_remove(t, "offset", text))),
            )
            .into_any()
        } else {
            let (said, moved) = (event.clone(), event.clone());
            offset_field(
                minutes,
                move |minutes| sun_words(&said, minutes),
                {
                    let event = moved.clone();
                    move |minutes| at.set(part_of(&event, minutes))
                },
                move |minutes| {
                    LAST_SUN.with(|last| last.set(Some(part_of(&moved, minutes))));
                    edit(Box::new(move |t: &mut Value| {
                        write_or_remove(t, "offset", offset_text(minutes));
                    }));
                },
            )
            .into_any()
        }}
        {next_line(clock, true)}
    }
    .into_any()
}

/// The two ends of a sun window: how each is written, and what it's called.
const ENDS: [(&str, &str, &str); 2] = [
    ("after", "after_offset", "From"),
    ("before", "before_offset", "Until"),
];

/// An offset that moves both ends, written out as each end's own: the form sets them apart.
fn own_offsets(window: &mut Value) {
    let Some(shared) = window
        .as_object_mut()
        .and_then(|object| object.remove("offset"))
    else {
        return;
    };
    for (end, own, _) in ENDS {
        if !window[end].is_null() && window[own].is_null() {
            window[own] = shared.clone();
        }
    }
}

/// The sun as a window: from one moment, until another, or both, each with its own offset
/// ("from 30 min before sunset until sunrise"). The stretch of the day it holds for is lit
/// along the sun's path, and a press on the arc sets whichever end is being set.
pub fn sun_window_form(
    condition: &Value,
    edit: impl Fn(Edit) + Clone + Send + Sync + 'static,
) -> AnyView {
    let clock = ask(json!({ "type": "sun", "event": "sunset" }));
    // Each end: its moment, its offset as written, and that offset in minutes if a slider
    // can hold it.
    let ends: Vec<(Option<String>, Option<String>, Option<i32>)> = ENDS
        .iter()
        .map(|(end, own, _)| {
            let event = condition[*end].as_str().map(str::to_owned);
            let written = end_offset(condition, own).map(str::to_owned);
            let minutes = match written.as_deref() {
                None => Some(0),
                Some(text) => offset_minutes(text).filter(|minutes| minutes.abs() <= 120),
            };
            (event, written, minutes)
        })
        .collect();
    // An end with no moment can't be the one being set by the arc's stops alone, but it can
    // be given one: so whichever was last being set still is.
    let setting = RwSignal::new(SUN_END.with(Cell::get).min(1));
    let part = |i: usize| {
        let (event, _, minutes) = &ends[i];
        event
            .as_deref()
            .map(|event| part_of(event, minutes.unwrap_or(0)))
    };
    // Where each end stands on the arc, following its slider while that is dragged.
    let parts = [RwSignal::new(part(0)), RwSignal::new(part(1))];
    // The disc stands at the end being set: the one that last moved.
    let here = setting.get_untracked();
    let at = travelling(part(here).or(part(1 - here)).unwrap_or(0.75));
    let lit = Signal::derive(move || match (parts[0].get(), parts[1].get()) {
        (Some(from), Some(to)) => Some((from, to)),
        // Open at one end: to where the day ends, or from where it starts.
        (Some(from), None) => Some((from, 1.0)),
        (None, Some(to)) => Some((0.0, to)),
        (None, None) => None,
    });
    let pick = {
        let edit = edit.clone();
        move |name: &'static str| {
            let (end, _, _) = ENDS[setting.get_untracked()];
            edit(Box::new(move |c: &mut Value| c[end] = json!(name)));
        }
    };
    let chosen: Vec<&'static str> = SUN
        .iter()
        .map(|(name, _, _)| *name)
        .filter(|name| {
            ends.iter()
                .any(|(event, _, _)| event.as_deref() == Some(name))
        })
        .collect();
    let events: [Option<String>; 2] = [ends[0].0.clone(), ends[1].0.clone()];
    let chips_chosen = Signal::derive(move || events[setting.get()].clone());

    let rows = ENDS
        .iter()
        .enumerate()
        .map(|(i, (end, own, label))| {
            let (end, own, label) = (*end, *own, *label);
            let (event, written, minutes) = ends[i].clone();
            let other_is_set = ends[1 - i].0.is_some();
            let open = {
                let edit = edit.clone();
                move |_| {
                    edit(Box::new(move |c: &mut Value| {
                        own_offsets(c);
                        if let Some(object) = c.as_object_mut() {
                            object.remove(end);
                            object.remove(own);
                        }
                    }));
                }
            };
            // The moment itself; how far it's moved is said over its slider.
            let said = match &event {
                Some(event) => SUN
                    .iter()
                    .find(|(name, _, _)| name == event)
                    .map_or("The sun", |(_, label, _)| *label),
                None if i == 0 => "The start of the day",
                None => "The end of the day",
            };
            let today = {
                let event = event.clone();
                move || {
                    let event = event.clone()?;
                    clock.get()?.sun.get(event.as_str()).cloned()
                }
            };
            let offset = event.clone().map(|event| match minutes {
                Some(minutes) => {
                    let (said, moved, kept) = (event.clone(), event.clone(), event);
                    let edit = edit.clone();
                    offset_field(
                        minutes,
                        move |minutes| match minutes {
                            0 => format!("Right at {}", sun_word(&said)),
                            minutes => sun_words(&said, minutes),
                        },
                        move |minutes| {
                            let part = part_of(&moved, minutes);
                            parts[i].set(Some(part));
                            setting.set(i);
                            at.set(part);
                        },
                        move |minutes| {
                            SUN_END.with(|last| last.set(i));
                            LAST_SUN.with(|last| last.set(Some(part_of(&kept, minutes))));
                            edit(Box::new(move |c: &mut Value| {
                                own_offsets(c);
                                write_or_remove(c, own, offset_text(minutes));
                            }));
                        },
                    )
                    .into_any()
                }
                None => {
                    let edit = edit.clone();
                    offset_typed(
                        "Before or after it (like -45m or 1h30m)",
                        written.clone().unwrap_or_default(),
                        move |text| {
                            edit(Box::new(move |c: &mut Value| {
                                own_offsets(c);
                                write_or_remove(c, own, text);
                            }));
                        },
                    )
                    .into_any()
                }
            });
            view! {
                <div class="sun-end" class:setting=move || setting.get() == i>
                    <button type="button" class="sun-end-head"
                        aria-pressed=move || (setting.get() == i).to_string()
                        title="Set this end from the arc and the moments below"
                        on:click=move |_| {
                            SUN_END.with(|last| last.set(i));
                            setting.set(i);
                            if let Some(part) = parts[i].get_untracked() {
                                let from = at.get_untracked();
                                glide(at, (part - from + 0.5).rem_euclid(1.0) - 0.5);
                                LAST_SUN.with(|last| last.set(Some(part)));
                            }
                        }>
                        <span class="sun-end-label">{label}</span>
                        <span class="sun-end-said">{said}</span>
                        <span class="sun-end-time">
                            {move || today().map(|at| format!("{at} today")).unwrap_or_default()}
                        </span>
                    </button>
                    {offset}
                    // One end may be left open, never both.
                    {(event.is_some() && other_is_set).then(|| view! {
                        <button type="button" class="link clock-switch" on:click=open>
                            {if i == 0 { "Leave the start open" } else { "Leave the end open" }}
                        </button>
                    })}
                </div>
            }
        })
        .collect_view();
    view! {
        {arc(at, chosen, lit, pick.clone())}
        <p class="muted clock-hint sun-setting">
            {move || if setting.get() == 0 {
                "Pick the moment it starts:"
            } else {
                "Pick the moment it ends:"
            }}
        </p>
        {sun_chips(clock, chips_chosen, pick)}
        <div class="sun-ends">{rows}</div>
        {holds_line(condition.clone(), true)}
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn days(list: &[&str]) -> Vec<String> {
        list.iter().map(|day| (*day).to_owned()).collect()
    }

    #[test]
    fn a_time_reads_both_ways() {
        assert_eq!(minutes_of("07:00"), Some(420));
        assert_eq!(minutes_of("23:59:30"), Some(1439));
        assert_eq!(minutes_of(" 7:5 "), Some(425));
        assert_eq!(minutes_of("24:00"), None);
        assert_eq!(minutes_of("seven"), None);
        assert_eq!(hhmm(425), "07:05");
        assert_eq!(hhmm(1440), "00:00");
    }

    #[test]
    fn a_time_is_read_however_it_was_typed() {
        assert_eq!(minutes_of("14"), Some(840));
        assert_eq!(minutes_of("2pm"), Some(840));
        assert_eq!(minutes_of("2 PM"), Some(840));
        assert_eq!(minutes_of("2:30 pm"), Some(870));
        assert_eq!(minutes_of("2.30pm"), Some(870));
        assert_eq!(minutes_of("2am"), Some(120));
        // Twelve starts its half of the day.
        assert_eq!(minutes_of("12am"), Some(0));
        assert_eq!(minutes_of("12pm"), Some(720));
        assert_eq!(minutes_of("13pm"), None);
        assert_eq!(minutes_of("0am"), None);
        assert_eq!(minutes_of(""), None);
        assert_eq!(minutes_of("pm"), None);
    }

    #[test]
    fn a_window_is_said_in_a_few_words() {
        let said = |window: Value| window_words(&window);
        assert_eq!(
            said(json!({ "type": "time", "after": "14:00", "before": "15:00" })),
            "between 14:00 and 15:00"
        );
        assert_eq!(
            said(json!({ "type": "time", "after": "14:00:00" })),
            "after 14:00"
        );
        assert_eq!(
            said(json!({ "type": "time", "before": "02:00", "weekday": ["sat", "sun"] })),
            "before 02:00 at the weekend"
        );
        assert_eq!(
            said(json!({ "type": "time", "weekday": ["mon", "tue", "wed", "thu", "fri"] })),
            "on weekdays"
        );
        assert_eq!(
            said(json!({ "type": "sun", "after": "sunset", "before": "sunrise" })),
            "from sunset until sunrise"
        );
        assert_eq!(
            said(
                json!({ "type": "sun", "after": "sunset", "before": "sunrise",
                "after_offset": "-30m", "before_offset": "15m" })
            ),
            "from 30 min before sunset until 15 min after sunrise"
        );
        // One offset written the old way moves both ends.
        assert_eq!(
            said(json!({ "type": "sun", "after": "sunset", "offset": "1h" })),
            "from 1 h after sunset on"
        );
        assert_eq!(
            said(json!({ "type": "sun", "before": "dusk" })),
            "until dusk"
        );
        assert_eq!(
            said(json!({ "type": "sun", "after": "dawn", "after_offset": "90s" })),
            "from dawn (90s) on"
        );
    }

    #[test]
    fn an_offset_for_both_ends_becomes_each_end_s_own() {
        let mut window = json!({ "type": "sun", "after": "sunset", "before": "sunrise",
            "offset": "-30m" });
        own_offsets(&mut window);
        assert_eq!(
            window,
            json!({ "type": "sun", "after": "sunset", "before": "sunrise",
                "after_offset": "-30m", "before_offset": "-30m" })
        );
        // An end that isn't there has nothing to move.
        let mut open = json!({ "type": "sun", "after": "sunset", "offset": "1h" });
        own_offsets(&mut open);
        assert_eq!(
            open,
            json!({ "type": "sun", "after": "sunset", "after_offset": "1h" })
        );
    }

    #[test]
    fn noon_is_up_and_midnight_is_down() {
        assert_eq!(hand_angle(720.0), 0.0);
        assert_eq!(hand_angle(0.0), -180.0);
        assert_eq!(hand_angle(360.0), -90.0);
        // And a point on the dial is the time the hand would be at.
        assert_eq!(minutes_at(0.0, -50.0, 5), 720);
        assert_eq!(minutes_at(0.0, 50.0, 5), 0);
        assert_eq!(minutes_at(-50.0, 0.0, 5), 360);
        assert_eq!(minutes_at(50.0, 0.0, 5), 1080);
        // To the nearest five minutes.
        assert_eq!(minutes_at(1.0, -50.0, 5) % 5, 0);
    }

    #[test]
    fn the_hand_turns_the_short_way_round() {
        assert_eq!(short_way(60.0, 120.0), 60.0);
        // From eleven at night to one in the morning is two hours on, not twenty-two back.
        assert_eq!(short_way(1380.0, 60.0), 120.0);
        assert_eq!(short_way(60.0, 1380.0), -120.0);
    }

    #[test]
    fn days_are_said_in_a_few_words() {
        assert_eq!(days_words(&[]), "every day");
        assert_eq!(
            days_words(&days(&["mon", "tue", "wed", "thu", "fri"])),
            "on weekdays"
        );
        assert_eq!(days_words(&days(&["sun", "sat"])), "at the weekend");
        assert_eq!(
            days_words(&days(&["wed", "mon"])),
            "on Monday and Wednesday"
        );
        assert_eq!(
            time_words("07:00", &days(&["mon", "tue", "wed", "thu", "fri"])),
            "at 07:00 on weekdays"
        );
    }

    #[test]
    fn days_are_written_in_the_week_s_order_and_every_day_is_nothing() {
        let mut trigger = json!({ "type": "time", "at": "07:00" });
        write_days(&mut trigger, &days(&["fri", "mon"]));
        assert_eq!(trigger["weekday"], json!(["mon", "fri"]));
        write_days(
            &mut trigger,
            &days(&["mon", "tue", "wed", "thu", "fri", "sat", "sun"]),
        );
        assert!(trigger.get("weekday").is_none());
    }

    #[test]
    fn an_offset_reads_both_ways() {
        assert_eq!(offset_minutes("-30m"), Some(-30));
        assert_eq!(offset_minutes("1h30m"), Some(90));
        assert_eq!(offset_minutes("+45m"), Some(45));
        // Seconds and milliseconds aren't a slider's to hold.
        assert_eq!(offset_minutes("90s"), None);
        assert_eq!(offset_minutes("500ms"), None);
        assert_eq!(offset_text(-30).as_deref(), Some("-30m"));
        assert_eq!(offset_text(90).as_deref(), Some("1h30m"));
        assert_eq!(offset_text(120).as_deref(), Some("2h"));
        assert_eq!(offset_text(0), None);
    }

    #[test]
    fn the_sun_is_said_the_way_a_person_would() {
        assert_eq!(sun_words("sunset", 0), "at sunset");
        assert_eq!(sun_words("sunset", -30), "30 min before sunset");
        assert_eq!(sun_words("dawn", 90), "1 h 30 min after dawn");
    }

    #[test]
    fn a_cron_is_read_out() {
        assert_eq!(
            cron_words("0 7 * * *").as_deref(),
            Ok("at 07:00, every day")
        );
        assert_eq!(
            cron_words("*/30 8 * * MON-FRI").as_deref(),
            Ok("at :00 and 30 past 08, on Mon, Tue, Wed, Thu and Fri")
        );
        assert_eq!(
            cron_words("0 0 1 JAN *").as_deref(),
            Ok("at 00:00, on day 1 of the month, in Jan")
        );
        assert!(cron_words("0 25 * * *").is_err());
    }

    #[test]
    fn the_sun_rises_on_the_left_and_is_highest_at_noon() {
        let (rise, noon, set, midnight) = (sun_at(0.25), sun_at(0.5), sun_at(0.75), sun_at(0.0));
        assert!((rise.1 - 100.0).abs() < 1e-9 && (set.1 - 100.0).abs() < 1e-9);
        assert!(rise.0 < noon.0 && noon.0 < set.0);
        assert!(noon.1 < rise.1 && midnight.1 > rise.1);
        assert_eq!(height(0.5), 1.0);
        assert_eq!(height(0.0), 0.0);
        assert!(height(0.3) > 0.0 && height(0.2) == 0.0);
    }

    #[test]
    fn an_offset_nudges_the_sun_along_its_arc() {
        assert!(part_of("sunset", -30) < part_of("sunset", 0));
        assert!((part_of("sunset", 60) - (0.75 + 1.0 / 24.0)).abs() < 1e-9);
    }

    #[test]
    fn the_window_on_the_dial_is_a_closed_shape() {
        let path = arc_path(1320.0, 360.0, 92.0, 70.0);
        assert!(path.starts_with('M') && path.ends_with('Z'), "{path}");
        // Eight hours is the short way round; sixteen is the long way.
        assert!(arc_path(0.0, 480.0, 92.0, 70.0).contains(" 0 0 1 "));
        assert!(arc_path(0.0, 960.0, 92.0, 70.0).contains(" 0 1 1 "));
    }
}
