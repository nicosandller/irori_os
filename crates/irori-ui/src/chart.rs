//! A number's last 24 hours, as a line.
//!
//! A step line, not a slope: Irori records a reading each time it changes, so a value holds
//! until the next one arrives — drawing a slope between two readings would invent every value in
//! between. The line runs on to now at the last value, which is still what the sensor says.
//!
//! Drawn in SVG stretched to the width it's given, with everything that must keep its shape —
//! the dots, the labels, the tooltip — in HTML over it, placed by percentage. One series, so no
//! legend: the row above names it. The table is still a click away for anyone who wants the
//! numbers (the device page's Chart/Table switch).

use leptos::ev;
use leptos::prelude::*;
use web_sys::wasm_bindgen::JsCast;

/// One reading: when it arrived, and what it said. A value that isn't a number (`NaN`) is a gap
/// — the sensor wasn't there, or hadn't said — and the line breaks there rather than pretending
/// the last value held through it.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub at_ms: f64,
    /// For the labels, in the same words the table uses.
    pub at: String,
    pub value: f64,
}

/// The drawing's own units: 1000 wide, 100 tall, with a margin top and bottom so a line along
/// the extremes isn't cut in half by the edge.
const WIDTH: f64 = 1000.0;
const TOP: f64 = 8.0;
const BOTTOM: f64 = 92.0;

/// A round step for the value axis, from a rough one: 1, 2 or 5 times a power of ten.
fn nice_step(rough: f64) -> f64 {
    if rough.is_nan() || rough <= 0.0 || rough.is_infinite() {
        return 1.0;
    }
    // Found by stepping rather than with logarithms, which would pull a maths library into the
    // download for one number.
    let mut magnitude = 1.0;
    while magnitude * 10.0 <= rough {
        magnitude *= 10.0;
    }
    while magnitude > rough {
        magnitude /= 10.0;
    }
    let fraction = rough / magnitude;
    let nice = if fraction <= 1.0 {
        1.0
    } else if fraction <= 2.0 {
        2.0
    } else if fraction <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * magnitude
}

/// The value axis: from a round number at or under the lowest reading to one at or over the
/// highest, so the gridlines fall on numbers worth reading. A flat line gets room either side.
pub fn value_range(min: f64, max: f64) -> (f64, f64) {
    let (min, max) = if max > min {
        (min, max)
    } else {
        let pad = if min == 0.0 { 1.0 } else { min.abs() * 0.1 };
        (min - pad, max + pad)
    };
    // About four steps across the readings: close enough to the data that the line uses the
    // height, loose enough that the ends are round.
    let step = nice_step((max - min) / 4.0);
    ((min / step).floor() * step, (max / step).ceil() * step)
}

/// Where each reading goes across, from 0 to [`WIDTH`], over the time from the first reading to
/// now.
fn across(readings: &[Reading], now_ms: f64) -> Vec<f64> {
    let start = readings.first().map_or(now_ms, |first| first.at_ms);
    let span = (now_ms - start).max(1.0);
    readings
        .iter()
        .map(|reading| ((reading.at_ms - start) / span * WIDTH).clamp(0.0, WIDTH))
        .collect()
}

fn down(value: f64, (lo, hi): (f64, f64)) -> f64 {
    BOTTOM - (value - lo) / (hi - lo) * (BOTTOM - TOP)
}

/// The line and the area under it: along at each value until the next reading, straight up or
/// down to it, and on to the right edge — now — at the last one. A gap ends a run; the next
/// value starts a new one.
pub fn step_paths(xs: &[f64], ys: &[f64]) -> (String, String) {
    let (mut line, mut area, mut run) = (String::new(), String::new(), String::new());
    let mut run_start = 0.0;
    for i in 0..xs.len() {
        let (x, y) = (xs[i], ys[i]);
        if y.is_nan() {
            continue;
        }
        let until = xs.get(i + 1).copied().unwrap_or(WIDTH);
        if i > 0 && !ys[i - 1].is_nan() {
            run.push_str(&format!("V{y:.1}H{until:.1}"));
        } else {
            run = format!("M{x:.1} {y:.1}H{until:.1}");
            run_start = x;
        }
        if ys.get(i + 1).is_none_or(|next| next.is_nan()) {
            line.push_str(&run);
            area.push_str(&format!("{run}V100H{run_start:.1}Z"));
        }
    }
    (line, area)
}

/// Which reading was in effect at `x` across: the last one to arrive at or before it.
pub fn in_effect(xs: &[f64], x: f64) -> usize {
    xs.iter().rposition(|&at| at <= x).unwrap_or(0)
}

#[component]
pub fn StepChart(
    readings: Vec<Reading>,
    now_ms: f64,
    /// What the numbers are in, if anything: "°C", "lx".
    unit: String,
    /// What the chart is of, for a screen reader.
    name: String,
) -> impl IntoView {
    let values = readings.iter().map(|reading| reading.value);
    let (min, max) = values.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), value| {
        (lo.min(value), hi.max(value))
    });
    let range = value_range(min, max);
    let xs = across(&readings, now_ms);
    let ys: Vec<f64> = readings
        .iter()
        .map(|reading| down(reading.value, range))
        .collect();
    let (line, area) = step_paths(&xs, &ys);
    let with_unit = {
        let unit = unit.clone();
        move |value: f64| {
            if value.is_nan() {
                return "No reading".to_owned();
            }
            let number = crate::devices::number(value);
            if unit.is_empty() {
                number
            } else {
                format!("{number} {unit}")
            }
        }
    };
    let last = readings.len().saturating_sub(1);
    let last_y = ys.last().copied().unwrap_or(BOTTOM);
    let latest = readings
        .last()
        .map(|reading| with_unit(reading.value))
        .unwrap_or_default();
    let since = readings
        .first()
        .map(|reading| reading.at.clone())
        .unwrap_or_default();
    let summary = format!(
        "{name}, last 24 hours: between {} and {}, now {latest}. Arrow keys step through the \
         readings.",
        with_unit(min),
        with_unit(max),
    );

    // What the pointer or the arrow keys are on: the reading, and where across the rule is.
    let pointed = RwSignal::new(None::<(usize, f64)>);
    let point_at = {
        let xs = xs.clone();
        move |ev: ev::PointerEvent| {
            let Some(plot) = ev.current_target() else {
                return;
            };
            let rect = plot
                .unchecked_into::<web_sys::Element>()
                .get_bounding_client_rect();
            let fraction =
                ((f64::from(ev.client_x()) - rect.left()) / rect.width()).clamp(0.0, 1.0);
            let x = fraction * WIDTH;
            pointed.set(Some((in_effect(&xs, x), x)));
        }
    };
    let step = {
        let xs = xs.clone();
        move |ev: ev::KeyboardEvent| {
            let now = pointed.get_untracked().map_or(last, |(index, _)| index);
            let next = match ev.key().as_str() {
                "ArrowLeft" => now.saturating_sub(1),
                "ArrowRight" => (now + 1).min(last),
                "Home" => 0,
                "End" => last,
                _ => return,
            };
            ev.prevent_default();
            pointed.set(Some((next, xs[next])));
        }
    };
    let on_focus = {
        let xs = xs.clone();
        move |_| {
            if pointed.get_untracked().is_none() {
                pointed.set(Some((last, xs[last])));
            }
        }
    };
    let tip = {
        let readings = readings.clone();
        let with_unit = with_unit.clone();
        move || {
            pointed.get().map(|(index, x)| {
                let reading = &readings[index];
                let percent = x / WIDTH * 100.0;
                // Kept inside the plot at either end, rather than hanging off it.
                let anchor = if percent > 75.0 {
                    "end"
                } else if percent < 25.0 {
                    "start"
                } else {
                    "mid"
                };
                (
                    percent,
                    ys[index],
                    with_unit(reading.value),
                    reading.at.clone(),
                    anchor,
                )
            })
        }
    };
    let tip = Memo::new(move |_| tip());

    view! {
        <figure class="chart">
            <div
                class="chart-plot"
                tabindex="0"
                role="group"
                aria-label=summary
                on:pointermove=point_at
                on:pointerleave=move |_| pointed.set(None)
                on:keydown=step
                on:focus=on_focus
                on:blur=move |_| pointed.set(None)
            >
                <svg viewBox="0 0 1000 100" preserveAspectRatio="none" aria-hidden="true">
                    <g class="chart-grid">
                        <line x1="0" x2="1000" y1=TOP y2=TOP />
                        <line x1="0" x2="1000" y1="50" y2="50" />
                        <line x1="0" x2="1000" y1=BOTTOM y2=BOTTOM />
                    </g>
                    <path class="chart-area" d=area />
                    <path class="chart-line" d=line vector-effect="non-scaling-stroke" />
                </svg>
                <span class="chart-tick top">{with_unit(range.1)}</span>
                <span class="chart-tick bottom">{with_unit(range.0)}</span>
                // Now: the reading the row above shows, marked where the line ends — unless the
                // sensor isn't saying anything now.
                {(!last_y.is_nan()).then(|| view! {
                    <span class="chart-end" style=format!("top:{last_y:.1}%")></span>
                    <span class="chart-latest" style=format!("top:{last_y:.1}%")>{latest}</span>
                })}
                {move || tip.get().map(|(percent, y, value, at, anchor)| view! {
                    <span class="chart-rule" style=format!("left:{percent:.2}%")></span>
                    <span
                        class="chart-dot"
                        hidden=y.is_nan()
                        style=format!("left:{percent:.2}%;top:{y:.1}%")
                    ></span>
                    <span class=format!("chart-tip {anchor}") style=format!("left:{percent:.2}%")>
                        <strong>{value}</strong>
                        <span>"since " {at}</span>
                    </span>
                })}
            </div>
            <figcaption class="chart-axis">
                <span>{since}</span>
                <span>"now"</span>
            </figcaption>
            // What the keys are on, said out loud as it changes.
            <p class="visually-hidden" aria-live="polite">
                {move || tip.get().map(|(_, _, value, at, _)| format!("{value}, since {at}"))}
            </p>
        </figure>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(at_ms: f64, value: f64) -> Reading {
        Reading {
            at_ms,
            at: String::new(),
            value,
        }
    }

    /// The axis lands on round numbers around the readings, and a flat line still has a range.
    #[test]
    fn the_value_axis_is_round_and_never_empty() {
        assert_eq!(value_range(18.4, 22.7), (18.0, 24.0));
        assert_eq!(value_range(0.0, 830.0), (0.0, 1000.0));
        assert_eq!(value_range(21.0, 21.0), (18.0, 24.0));
        assert_eq!(value_range(0.0, 0.0), (-1.0, 1.0));
        let (lo, hi) = value_range(-3.2, 4.1);
        assert!(lo <= -3.2 && hi >= 4.1);
    }

    /// Steps, not slopes: each value holds until the next reading, and the last runs on to now.
    #[test]
    fn the_line_holds_each_value_until_the_next() {
        let readings = [reading(0.0, 1.0), reading(500.0, 2.0)];
        let xs = across(&readings, 1000.0);
        assert_eq!(xs, vec![0.0, 500.0]);
        let (line, area) = step_paths(&xs, &[80.0, 20.0]);
        assert_eq!(line, "M0.0 80.0H500.0V20.0H1000.0");
        assert_eq!(area, "M0.0 80.0H500.0V20.0H1000.0V100H0.0Z");
    }

    /// A gap breaks the line: the value before it stops where the gap starts, and the next one
    /// starts a new run — nothing is drawn through the time nothing was said.
    #[test]
    fn a_gap_breaks_the_line() {
        let xs = [0.0, 300.0, 600.0];
        let (line, area) = step_paths(&xs, &[80.0, f64::NAN, 20.0]);
        assert_eq!(line, "M0.0 80.0H300.0M600.0 20.0H1000.0");
        assert_eq!(
            area,
            "M0.0 80.0H300.0V100H0.0ZM600.0 20.0H1000.0V100H600.0Z"
        );
    }

    /// Pointing anywhere between two readings is pointing at the earlier one — it was in effect.
    #[test]
    fn a_moment_belongs_to_the_reading_before_it() {
        let xs = [0.0, 400.0, 900.0];
        assert_eq!(in_effect(&xs, 0.0), 0);
        assert_eq!(in_effect(&xs, 399.0), 0);
        assert_eq!(in_effect(&xs, 400.0), 1);
        assert_eq!(in_effect(&xs, 1000.0), 2);
    }
}
