//! The month at a glance: what's spent against the allowance, split between Irori and the other
//! apps on the account, where it's heading, and when it runs out.

use leptos::prelude::*;

use crate::api::Summary;
use crate::{Usage, time};

fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        (part as f64 / whole as f64 * 100.0).clamp(0.0, 100.0)
    }
}

#[component]
pub fn Month() -> impl IntoView {
    let usage = expect_context::<Usage>();
    move || usage.summary.get().map(|summary| glance(&summary))
}

fn glance(summary: &Summary) -> impl IntoView + use<> {
    let others = summary.others.clone();
    let known = others.is_some();
    let spent = summary.account.clone();
    let others_micros = others.as_ref().map_or(0, |others| others.micros);
    // The bar's whole: the allowance, or the month's end when that's further.
    let scale = summary
        .allowance
        .micros
        .max(summary.projected.micros)
        .max(spent.micros)
        .max(1);
    let irori_width = percent(summary.irori.micros, scale);
    let others_width = percent(others_micros, scale);
    let ahead_width = percent(summary.projected.micros.saturating_sub(spent.micros), scale);
    let allowance_at = percent(summary.allowance.micros, scale);
    let label = if known {
        "Spent this month, all apps"
    } else {
        "Spent by Irori this month"
    };
    let meter_name = format!(
        "{} spent of a {} allowance: Irori {}{}; heading for {} by the month's end.",
        spent.text,
        summary.allowance.text,
        summary.irori.text,
        others
            .as_ref()
            .map(|others| format!(", other apps about {}", others.text))
            .unwrap_or_default(),
        summary.projected.text
    );
    let runs_out = match summary.runs_out_at {
        Some(at) if at <= summary.now => "Already".to_owned(),
        Some(at) => time::date_label(at),
        None => "Not this month".to_owned(),
    };
    let others_rate = if known {
        format!("{} a day", summary.others_per_day.text)
    } else {
        "Not known yet".to_owned()
    };
    let others_note = if known {
        "Estimated from Tesla's dashboard"
    } else {
        "Enter Tesla's dashboard figure below"
    };
    let over = summary.over;
    let warning = summary.warning.clone();

    view! {
        <section class="card glance" aria-labelledby="spent-label">
            {warning.map(|warning| view! {
                <p class="status warning" role="status">
                    <span class="status-icon" aria-hidden="true">"!"</span>
                    <span>{warning}</span>
                </p>
            })}
            <p class="label" id="spent-label">{label}</p>
            <p class="hero">
                <span class="hero-figure">{spent.text.clone()}</span>
                <span class="hero-of muted">" of " {summary.allowance.text.clone()}</span>
            </p>
            <div class="meter" role="img" aria-label=meter_name>
                <div class="meter-track">
                    {[("irori", irori_width), ("others", others_width), ("ahead", ahead_width)]
                        .into_iter()
                        // A part at nothing draws nothing, not even its gap.
                        .filter(|(_, width)| *width > 0.0)
                        .map(|(part, width)| view! {
                            <span class=format!("part {part}") style=format!("width:{width:.3}%")></span>
                        })
                        .collect_view()}
                </div>
                <span class="allowance-mark" style=format!("left:{allowance_at:.3}%")></span>
            </div>
            <ul class="legend">
                <li><span class="key irori"></span>"Irori " <b>{summary.irori.text.clone()}</b></li>
                {others.map(|others| view! {
                    <li><span class="key others"></span>"Other apps " <b>{others.text}</b>
                        <span class="muted">" (estimated)"</span></li>
                })}
                <li><span class="key ahead"></span>"Heading for " <b>{summary.projected.text.clone()}</b></li>
                <li><span class="key mark"></span>"Allowance " <b>{summary.allowance.text.clone()}</b></li>
            </ul>
            <div class="tiles">
                <div class="tile">
                    <p class="label">"Projected for the month"</p>
                    <p class="value">{summary.projected.text.clone()}</p>
                    <p class=move || if over { "foot over" } else { "foot muted" }>
                        {if over { "Over the allowance: Irori reads less often" } else { "Inside the allowance" }}
                    </p>
                </div>
                <div class="tile">
                    <p class="label">"Left of the allowance"</p>
                    <p class="value">{summary.left.text.clone()}</p>
                    <p class="foot muted">{format!("Tesla's credit is {} a month", summary.credit.text)}</p>
                </div>
                <div class="tile">
                    <p class="label">"Runs out"</p>
                    <p class="value">{runs_out}</p>
                    <p class="foot muted">"At this month's rate so far"</p>
                </div>
                <div class="tile">
                    <p class="label">"Other apps"</p>
                    <p class="value">{others_rate}</p>
                    <p class="foot muted">{others_note}</p>
                </div>
            </div>
        </section>
    }
}
