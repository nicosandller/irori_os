//! Where the money went: each car and site's share of the month, and the billed calls one by
//! one, with why each was made.

use leptos::prelude::*;

use crate::api::Target;
use crate::{Usage, time};

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "car" => "Car",
        "site" => "Energy site",
        "account" => "Checking which cars are awake, listing, setting up",
        _ => "No longer on the account",
    }
}

fn counted(target: &Target) -> String {
    let mut parts = Vec::new();
    let mut say = |n: u32, one: &str, many: &str| {
        if n > 0 {
            parts.push(format!("{n} {}", if n == 1 { one } else { many }));
        }
    };
    say(target.counts.data, "read", "reads");
    say(target.counts.command, "command", "commands");
    say(target.counts.wake, "wake", "wakes");
    parts.join(" · ")
}

#[component]
pub fn Targets() -> impl IntoView {
    let usage = expect_context::<Usage>();
    view! {
        <section class="card" aria-labelledby="targets-title">
            <h2 id="targets-title">"By car and site"</h2>
            {move || usage.days.with(|days| {
                let Some(days) = days else {
                    return view! { <p class="muted">"…"</p> }.into_any();
                };
                let month = &days.this;
                if month.targets.is_empty() {
                    return view! { <p class="muted">"Nothing billed yet this month."</p> }.into_any();
                }
                let whole = month.cost.micros.max(1) as f64;
                view! {
                    <ul class="targets">
                        {month.targets.iter().map(|target| {
                            let total = target.cost.micros as f64;
                            let width = total / whole * 100.0;
                            view! {
                                <li class="target">
                                    <div class="target-head">
                                        <span class="target-name">{target.name.clone()}</span>
                                        <b class="target-cost">{target.cost.text.clone()}</b>
                                    </div>
                                    <div class="thin" style=format!("width:{width:.3}%") aria-hidden="true"></div>
                                    <p class="muted small">
                                        {kind_label(&target.kind)} " · " {counted(target)}
                                    </p>
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                }.into_any()
            })}
        </section>
    }
}

/// How many calls show before "Show all".
const FIRST: usize = 20;

#[component]
pub fn Calls() -> impl IntoView {
    let usage = expect_context::<Usage>();
    let all = RwSignal::new(false);
    let now = move || usage.summary.with(|s| s.as_ref().map_or(0, |s| s.now));
    view! {
        <section class="card" aria-labelledby="calls-title">
            <div class="card-head">
                <h2 id="calls-title">"Where it went"</h2>
                <span class="muted small">"The latest billed calls, newest first"</span>
            </div>
            {move || {
                let calls = usage.calls.get();
                if calls.is_empty() {
                    return view! { <p class="muted">"No billed calls yet."</p> }.into_any();
                }
                let total = calls.len();
                let shown = if all.get() { total } else { total.min(FIRST) };
                let now = now();
                view! {
                    <div class="table-wrap">
                        <table class="calls">
                            <thead><tr>
                                <th scope="col">"When"</th>
                                <th scope="col">"Car or site"</th>
                                <th scope="col">"What"</th>
                                <th scope="col">"Why"</th>
                                <th scope="col" class="num">"Cost"</th>
                            </tr></thead>
                            <tbody>
                                {calls.into_iter().take(shown).map(|call| view! {
                                    <tr>
                                        <td class="when">{time::when(call.at, now)}</td>
                                        <td>{call.target}</td>
                                        <td>
                                            {call.what}
                                            {call.flag.map(|flag| view! {
                                                <span class="chip">{flag}</span>
                                            })}
                                        </td>
                                        <td class="muted">{call.why}</td>
                                        <td class="num">{call.cost.text}</td>
                                    </tr>
                                }).collect_view()}
                            </tbody>
                        </table>
                    </div>
                    {(total > FIRST).then(|| view! {
                        <button class="btn small more" on:click=move |_| all.update(|all| *all = !*all)>
                            {move || if all.get() { "Show fewer".to_owned() } else { format!("Show all {total}") }}
                        </button>
                    })}
                }.into_any()
            }}
        </section>
    }
}
