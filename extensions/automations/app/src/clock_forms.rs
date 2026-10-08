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
}

// ---- plain arithmetic, tested on the host ---------------------------------------------------

/// `HH:MM` (or `HH:MM:SS`) as minutes of the day.
pub fn minutes_of(text: &str) -> Option<i32> {
    let mut parts = text.trim().split(':');
    let hour: i32 = parts.next()?.trim().parse().ok()?;
    let minute: i32 = parts.next()?.trim().parse().ok()?;
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

/// The 24-hour dial. `hands` are the times on it (one for a trigger, two for a window);
/// dragging the dial moves the one nearest the pointer. `commit` is handed which hand and its
/// new time once it is let go.
fn dial(
    hands: Vec<(&'static str, i32)>,
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
        window
            .then(|| shown.with_value(|shown| arc_path(shown[0].get(), shown[1].get(), 92.0, 70.0)))
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
            {dial(vec![("The time", minutes)], clock, move |_, minutes| {
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

/// A window of time: from one time to another, on some days. The same dial, with two hands
/// and the window lit between them.
pub fn time_window_form(
    condition: &Value,
    edit: impl Fn(Edit) + Clone + Send + Sync + 'static,
) -> AnyView {
    // The dial's night comes from the sun at home; any trigger asks for it.
    let clock = ask(json!({ "type": "time", "at": "12:00" }));
    let after = condition["after"]
        .as_str()
        .and_then(minutes_of)
        .unwrap_or(22 * 60);
    let before = condition["before"]
        .as_str()
        .and_then(minutes_of)
        .unwrap_or(6 * 60);
    let turned = edit.clone();
    let field = |label: &'static str, key: &'static str, minutes: i32| {
        let edit = edit.clone();
        view! {
            <div>
                <label>{label}</label>
                <input type="text" class="clock-typed" inputmode="numeric" prop:value=hhmm(minutes)
                    on:change=move |event| {
                        if let Some(minutes) = minutes_of(&event_target_value(&event)) {
                            let text = hhmm(minutes);
                            edit(Box::new(move |c: &mut Value| c[key] = json!(text)));
                        }
                    } />
            </div>
        }
    };
    view! {
        <div class="clock-form">
            {dial(vec![("From", after), ("Until", before)], clock, move |hand, minutes| {
                let (key, text) = (if hand == 0 { "after" } else { "before" }, hhmm(minutes));
                turned(Box::new(move |c: &mut Value| c[key] = json!(text)));
            })}
            <div class="clock-side">
                {field("From", "after", after)}
                {field("Until", "before", before)}
                <p class="muted clock-hint">
                    {if after > before {
                        "Through the night: it holds from the first time, past midnight, until \
                         the second."
                    } else {
                        "It holds from the first time until the second."
                    }}
                </p>
            </div>
        </div>
        {days_field(condition, edit.clone())}
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

/// The sun on its arc. `at` is where it stands, `stops` the moments that can be picked and
/// whether each is the one chosen.
fn arc(
    at: RwSignal<f64>,
    chosen: Vec<&'static str>,
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

/// The offset as a slider, from two hours before to two hours after, with what it reads as.
fn offset_field(
    event: String,
    offset: i32,
    at: RwSignal<f64>,
    edit: impl Fn(Edit) + Clone + Send + Sync + 'static,
) -> impl IntoView {
    let live = RwSignal::new(offset);
    let named = event.clone();
    view! {
        <label>{move || sun_words(&named, live.get())}</label>
        <input type="range" class="sun-offset" min="-120" max="120" step="5"
            prop:value=offset.to_string()
            style=move || format!("--fill: {:.1}%", f64::from(live.get() + 120) / 240.0 * 100.0)
            aria-label="How long before or after"
            // While it's dragged the sun goes with it; letting go is what writes it down.
            on:input={
                let event = event.clone();
                move |input| {
                    if let Ok(minutes) = event_target_value(&input).parse::<i32>() {
                        live.set(minutes);
                        at.set(part_of(&event, minutes));
                    }
                }
            }
            on:change=move |input| {
                let Ok(minutes) = event_target_value(&input).parse::<i32>() else {
                    return;
                };
                LAST_SUN.with(|last| last.set(Some(part_of(&event, minutes))));
                edit(Box::new(move |t: &mut Value| match offset_text(minutes) {
                    Some(text) => t["offset"] = json!(text),
                    None => {
                        t.as_object_mut().map(|object| object.remove("offset"));
                    }
                }));
            } />
        <div class="sun-offset-ends muted"><span>"2 h before"</span><span>"2 h after"</span></div>
    }
}

/// Today's time for each of the sun's moments, as chips to pick from.
fn sun_chips(
    clock: RwSignal<Option<Clock>>,
    chosen: String,
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
                            aria-pressed=(chosen == *name).to_string()
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
    let typed = edit.clone();
    view! {
        {arc(at, SUN.iter().map(|(name, _, _)| *name).filter(|name| *name == event).collect(), pick.clone())}
        {sun_chips(clock, event.clone(), pick)}
        {if beyond {
            view! {
                <label>"Before or after it (like -45m or 1h30m)"</label>
                <input type="text" class="mono" prop:value=offset_as_written.unwrap_or_default()
                    on:change=move |input| {
                        let text = event_target_value(&input).trim().to_owned();
                        typed(Box::new(move |t: &mut Value| {
                            if text.is_empty() {
                                t.as_object_mut().map(|object| object.remove("offset"));
                            } else {
                                t["offset"] = json!(text);
                            }
                        }));
                    } />
            }
            .into_any()
        } else {
            offset_field(event, minutes, at, edit).into_any()
        }}
        {next_line(clock, true)}
    }
    .into_any()
}

/// The sun as a window: after one moment, before another, or both.
pub fn sun_window_form(
    condition: &Value,
    edit: impl Fn(Edit) + Clone + Send + Sync + 'static,
) -> AnyView {
    let clock = ask(json!({ "type": "sun", "event": "sunset" }));
    let after = condition["after"].as_str().map(str::to_owned);
    let before = condition["before"].as_str().map(str::to_owned);
    // The disc stands at the start of the window, which is the moment it opens.
    let at = travelling(part_of(
        after.as_deref().or(before.as_deref()).unwrap_or("sunset"),
        0,
    ));
    let row = |label: &'static str, key: &'static str, value: Option<String>, other: bool| {
        let edit = edit.clone();
        let current = value.clone().unwrap_or_default();
        view! {
            <label>{label}</label>
            <select on:change=move |input| {
                let picked = event_target_value(&input);
                edit(Box::new(move |c: &mut Value| {
                    if picked.is_empty() {
                        c.as_object_mut().map(|object| object.remove(key));
                    } else {
                        c[key] = json!(picked);
                    }
                }));
            }>
                // One end may be left open, never both.
                {other.then(|| view! {
                    <option value="" selected=current.is_empty()>"any time"</option>
                })}
                {SUN
                    .iter()
                    .map(|(name, label, _)| {
                        let today = clock
                            .get_untracked()
                            .and_then(|clock| clock.sun.get(*name).cloned());
                        view! {
                            <option value=*name selected={current == *name}>
                                {match today {
                                    Some(today) => format!("{label} ({today} today)"),
                                    None => (*label).to_owned(),
                                }}
                            </option>
                        }
                    })
                    .collect_view()}
            </select>
        }
    };
    let chosen: Vec<&'static str> = SUN
        .iter()
        .map(|(name, _, _)| *name)
        .filter(|name| after.as_deref() == Some(name) || before.as_deref() == Some(name))
        .collect();
    let said = match (&after, &before) {
        (Some(after), Some(before)) => {
            format!("From {} until {}.", sun_word(after), sun_word(before))
        }
        (Some(after), None) => format!("From {} until the day ends.", sun_word(after)),
        (None, Some(before)) => format!("From the start of the day until {}.", sun_word(before)),
        (None, None) => "Pick when it starts, when it ends, or both.".to_owned(),
    };
    view! {
        {arc(at, chosen, |_| {})}
        {row("From", "after", after.clone(), before.is_some())}
        {row("Until", "before", before.clone(), after.is_some())}
        <p class="muted clock-hint">{said}</p>
        {next_line(clock, true)}
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
