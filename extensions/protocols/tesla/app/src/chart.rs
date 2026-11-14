//! Each day's spend, stacked by what Tesla bills for (reading, commands, wakes), for this month
//! or the last. Columns are HTML, not SVG, so the text stays crisp at any width; each column is
//! its own hit target, with the day's figures on hover and focus, and the same figures are in
//! the table view.

use leptos::prelude::*;

use crate::api::{Day, Money, Month};
use crate::{Usage, time};

/// The three things Tesla bills for, bottom of the stack first. Colours follow the category
/// on every chart on the page.
const CATEGORIES: [(&str, &str); 3] = [
    ("data", "Reading"),
    ("command", "Commands"),
    ("wake", "Wakes"),
];

fn part(day: &Day, category: &str) -> u64 {
    match category {
        "data" => day.data,
        "command" => day.command,
        _ => day.wake,
    }
}

fn count(day: &Day, category: &str) -> u32 {
    match category {
        "data" => day.counts.data,
        "command" => day.counts.command,
        _ => day.counts.wake,
    }
}

/// How the extension writes money, so axis ticks read the same: the symbol before or the code
/// after, taken from an amount it formatted.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Format {
    before: String,
    after: String,
}

impl Format {
    pub fn from(sample: &Money) -> Self {
        let text = sample.text.as_str();
        let first = text.find(|c: char| c.is_ascii_digit()).unwrap_or(0);
        let last = text
            .rfind(|c: char| c.is_ascii_digit())
            .map_or(text.len(), |at| at + 1);
        Self {
            before: text[..first].to_owned(),
            after: text[last..].to_owned(),
        }
    }

    pub fn tick(&self, micros: u64) -> String {
        let units = micros as f64 / 1_000_000.0;
        let digits = if micros > 0 && micros < 10_000 { 3 } else { 2 };
        format!("{}{units:.digits$}{}", self.before, self.after)
    }
}

/// A round top for the axis and the step between its lines: at most five steps, each 1, 2 or
/// 5 of a power of ten.
pub fn axis(highest: u64) -> (u64, u64) {
    let highest = highest.max(1_000);
    let mut step = 1_000_u64;
    loop {
        for factor in [1, 2, 5] {
            let candidate = step * factor;
            if highest.div_ceil(candidate) <= 5 {
                return (highest.div_ceil(candidate) * candidate, candidate);
            }
        }
        step *= 10;
    }
}

#[component]
pub fn Daily() -> impl IntoView {
    let usage = expect_context::<Usage>();
    let last = RwSignal::new(false);
    let table = RwSignal::new(false);
    let hovered = RwSignal::new(None::<u8>);

    let month = move || {
        usage.days.with(|days| {
            days.as_ref().and_then(|days| {
                if last.get() {
                    days.last.clone()
                } else {
                    Some(days.this.clone())
                }
            })
        })
    };
    let last_name = move || {
        usage.days.with(|days| {
            days.as_ref()
                .and_then(|days| days.last.as_ref())
                .map(|month| time::month_only(&month.month))
        })
    };
    let format = move || {
        usage
            .summary
            .with(|summary| summary.as_ref().map(|s| Format::from(&s.allowance)))
            .unwrap_or_default()
    };
    // Today's column, in this month only (days are UTC, as Tesla bills).
    let today = move || {
        if last.get() {
            return None;
        }
        usage.summary.with(|summary| {
            summary.as_ref().map(|summary| {
                u8::try_from((summary.now.saturating_sub(summary.month_start)) / 86_400 + 1)
                    .unwrap_or(1)
            })
        })
    };

    view! {
        <section class="card" aria-labelledby="daily-title">
            <div class="card-head">
                <h2 id="daily-title">"Each day"</h2>
                <div class="seg" role="group" aria-label="Which month">
                    <button class:on=move || !last.get() on:click=move |_| last.set(false)
                        aria-pressed=move || (!last.get()).to_string()>"This month"</button>
                    {move || last_name().map(|name| view! {
                        <button class:on=move || last.get() on:click=move |_| last.set(true)
                            aria-pressed=move || last.get().to_string()>{name}</button>
                    })}
                </div>
                <button class="btn small" on:click=move |_| table.update(|shown| *shown = !*shown)
                    aria-pressed=move || table.get().to_string()>
                    {move || if table.get() { "Chart" } else { "Table" }}
                </button>
            </div>
            <ul class="legend">
                {CATEGORIES.iter().map(|(key, name)| view! {
                    <li><span class=format!("key {key}")></span>{*name}</li>
                }).collect_view()}
            </ul>
            {move || match month() {
                None => view! { <p class="muted">"Nothing counted yet."</p> }.into_any(),
                Some(month) if table.get() => days_table(&month).into_any(),
                Some(month) => columns(month, format(), today(), hovered).into_any(),
            }}
            <p class="muted small">"Days are UTC, as Tesla bills them. Each call is costed at the prices in the extension's settings."</p>
        </section>
    }
}

fn columns(
    month: Month,
    format: Format,
    today: Option<u8>,
    hovered: RwSignal<Option<u8>>,
) -> impl IntoView {
    let highest = month
        .days
        .iter()
        .map(|day| day.data + day.command + day.wake)
        .max()
        .unwrap_or(0);
    let (top, step) = axis(highest);
    let lines: Vec<u64> = (0..=top / step).map(|n| n * step).collect();
    let name = month.month.clone();
    let tip_month = month.month.clone();
    let tip_days = month.days.clone();
    let tip_format = format.clone();

    view! {
        <div class="chart" role="group"
            aria-label=format!("Each day's spend in {}; the table view lists the figures.", time::month_name(&name))>
            <div class="grid" aria-hidden="true">
                {lines.iter().rev().map(|value| view! {
                    <div class="gridline" style=format!("bottom:{:.3}%", *value as f64 / top as f64 * 100.0)>
                        <span>{format.tick(*value)}</span>
                    </div>
                }).collect_view()}
            </div>
            <div class="cols">
                {month.days.into_iter().map(|day| {
                    let number = day.day;
                    let total = day.data + day.command + day.wake;
                    let is_today = today == Some(number);
                    let later = today.is_some_and(|today| number > today);
                    let segments = CATEGORIES.iter().filter_map(|(key, _)| {
                        let value = part(&day, key);
                        (value > 0).then(|| view! {
                            <span class=format!("bar {key}")
                                style=format!("height:{:.3}%", value as f64 / top as f64 * 100.0)></span>
                        })
                    }).collect_view();
                    let label = (number == 1 || number % 5 == 0).then(|| number.to_string());
                    view! {
                        <div class="slot" tabindex=if later { "-1" } else { "0" }
                            class:today=is_today class:later=later
                            class:hot=move || hovered.get() == Some(number)
                            aria-label=format!("{}: {}", time::day_label(&name, number), day.cost.text)
                            on:pointerenter=move |_| hovered.set(Some(number))
                            on:pointerleave=move |_| hovered.set(None)
                            on:focus=move |_| hovered.set(Some(number))
                            on:blur=move |_| hovered.set(None)>
                            <div class="stack" class:empty=total == 0>{segments}</div>
                            <span class="day" aria-hidden="true">{label}</span>
                        </div>
                    }
                }).collect_view()}
            </div>
            {move || hovered.get().and_then(|number| {
                let day = tip_days.iter().find(|day| day.day == number)?.clone();
                let slots = tip_days.len().max(1) as f64;
                // Beside the column, on whichever side has the room, so it hides neither the
                // column nor the legend.
                let at = (f64::from(number) - 0.5) / slots;
                let side = if at > 0.5 { "calc(-100% - .9rem) 0" } else { ".9rem 0" };
                Some(view! {
                    <div class="tip" role="tooltip"
                        style=format!("left:calc(3.6rem + (100% - 3.6rem) * {at:.4});translate:{side}")>
                        <p class="tip-value">{day.cost.text.clone()}</p>
                        <p class="tip-day muted">{time::day_label(&tip_month, number)}</p>
                        {CATEGORIES.iter().map(|(key, name)| view! {
                            <p class="tip-row">
                                <span class=format!("line-key {key}")></span>
                                <b>{tip_format.tick(part(&day, key))}</b>
                                <span class="muted">{format!("{name}, {}", count(&day, key))}</span>
                            </p>
                        }).collect_view()}
                    </div>
                })
            })}
        </div>
    }
}

fn days_table(month: &Month) -> impl IntoView + use<> {
    let rows: Vec<Day> = month
        .days
        .iter()
        .filter(|day| day.counts.data + day.counts.command + day.counts.wake > 0)
        .cloned()
        .collect();
    let name = month.month.clone();
    let total = month.cost.text.clone();
    view! {
        <div class="table-wrap">
            <table class="nums">
                <thead><tr>
                    <th scope="col">"Day"</th>
                    <th scope="col">"Reads"</th>
                    <th scope="col">"Commands"</th>
                    <th scope="col">"Wakes"</th>
                    <th scope="col">"Cost"</th>
                </tr></thead>
                <tbody>
                    {rows.into_iter().map(|day| view! {
                        <tr>
                            <th scope="row">{time::day_label(&name, day.day)}</th>
                            <td>{day.counts.data}</td>
                            <td>{day.counts.command}</td>
                            <td>{day.counts.wake}</td>
                            <td>{day.cost.text}</td>
                        </tr>
                    }).collect_view()}
                </tbody>
                <tfoot><tr>
                    <th scope="row" colspan="4">"The month"</th>
                    <td>{total}</td>
                </tr></tfoot>
            </table>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_axis_tops_out_round_in_five_steps_or_fewer() {
        assert_eq!(axis(0), (1_000, 1_000));
        assert_eq!(axis(23_000), (25_000, 5_000));
        assert_eq!(axis(410_000), (500_000, 100_000));
        assert_eq!(axis(1_000_000), (1_000_000, 200_000));
    }

    #[test]
    fn ticks_read_like_the_extensions_money() {
        let dollars = Format::from(&Money {
            micros: 10_000_000,
            text: "$10.00".into(),
        });
        assert_eq!(dollars.tick(250_000), "$0.25");
        assert_eq!(dollars.tick(2_000), "$0.002");
        let kronor = Format::from(&Money {
            micros: 10_000_000,
            text: "10.00 SEK".into(),
        });
        assert_eq!(kronor.tick(1_000_000), "1.00 SEK");
    }
}
