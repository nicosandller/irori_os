//! What the person decides: how often Irori reads, how much the month may cost, and what
//! Tesla's dashboard says the whole account spent (which is how other apps' share is known).

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{self, Pace, Summary};
use crate::{Usage, time};

/// How often reading happens now, in sentences.
fn pace_lines(summary: &Summary) -> Vec<String> {
    let Pace {
        states_every,
        car_every,
        site_every,
        starved,
    } = summary.pace;
    if starved {
        return vec![
            "Nothing is left of the allowance for reading: cars and sites are read only on \
             Refresh. Commands still go out."
                .to_owned(),
        ];
    }
    if summary.mode == "Off" {
        return vec![
            "Nothing is read on its own: only Refresh and commands read a car.".to_owned(),
        ];
    }
    let mut lines = Vec::new();
    if let Some(gap) = car_every {
        lines.push(format!(
            "A car that's charging, driving or warming up is read {}.",
            time::every(gap)
        ));
    }
    if let Some(gap) = states_every {
        lines.push(format!(
            "Which cars are awake is checked {}. A sleeping car is never woken to be read.",
            time::every(gap)
        ));
    }
    if let Some(gap) = site_every {
        lines.push(format!("Energy sites are read {}.", time::every(gap)));
    }
    lines
}

#[component]
pub fn Reading() -> impl IntoView {
    let usage = expect_context::<Usage>();
    let allowance = RwSignal::new(String::new());
    let saving = RwSignal::new(false);
    let problem = RwSignal::new(None::<String>);

    let summary = move || usage.summary.get();
    let can_change = move || {
        usage
            .summary
            .with(|s| s.as_ref().is_some_and(|s| s.can_change))
    };

    let change = move |mode: Option<String>, amount: Option<String>| {
        saving.set(true);
        problem.set(None);
        spawn_local(async move {
            match api::set(mode.as_deref(), amount.as_deref()).await {
                Ok(summary) => {
                    allowance.set(String::new());
                    usage.changed(summary);
                }
                Err(why) => problem.set(Some(why)),
            }
            saving.set(false);
        });
    };

    view! {
        <section class="card" aria-labelledby="reading-title">
            <h2 id="reading-title">"Reading"</h2>
            {move || summary().map(|summary| {
                let current = summary.mode.clone();
                let about = summary
                    .modes
                    .iter()
                    .find(|mode| mode.label == current)
                    .map(|mode| mode.about.clone())
                    .unwrap_or_default();
                view! {
                    <div class="seg wide" role="radiogroup" aria-label="Polling">
                        {summary.modes.iter().map(|mode| {
                            let label = mode.label.clone();
                            let chosen = label == current;
                            let pick = label.clone();
                            view! {
                                <button role="radio" aria-checked=chosen.to_string() class:on=chosen
                                    disabled=move || !can_change() || saving.get()
                                    on:click=move |_| if !chosen { change(Some(pick.clone()), None) }>
                                    {label}
                                </button>
                            }
                        }).collect_view()}
                    </div>
                    <p class="about">{about}</p>
                    <ul class="pace">
                        {pace_lines(&summary).into_iter().map(|line| view! { <li>{line}</li> }).collect_view()}
                    </ul>
                    <p class="muted small">{format!(
                        "Tesla charges {} a read, {} a command and {} a wake, and bills a call even when the car is asleep or the call is refused.",
                        summary.prices.data.text, summary.prices.command.text, summary.prices.wake.text
                    )}</p>
                }
            })}
            <form class="field" on:submit=move |ev| {
                ev.prevent_default();
                let amount = allowance.get_untracked();
                if !amount.trim().is_empty() {
                    change(None, Some(amount));
                }
            }>
                <label for="allowance">"Monthly allowance, all apps"</label>
                <div class="row">
                    <input id="allowance" type="text" inputmode="decimal" autocomplete="off"
                        placeholder=move || usage.summary.with(|s| s.as_ref().map(|s| s.allowance.text.clone()).unwrap_or_default())
                        prop:value=move || allowance.get()
                        on:input=move |ev| allowance.set(event_target_value(&ev))
                        disabled=move || !can_change()/>
                    <button class="btn" type="submit"
                        disabled=move || !can_change() || saving.get() || allowance.with(|a| a.trim().is_empty())>
                        "Save"
                    </button>
                </div>
                <p class="muted small">"Irori paces its reading so the whole account ends the month inside this. Commands and wakes you ask for always go out."</p>
            </form>
            {move || problem.get().map(|why| view! { <p class="note error" role="alert">{why}</p> })}
            {move || (!can_change()).then(|| view! {
                <p class="muted small">"Only the home's owner can change these."</p>
            })}
        </section>
    }
}

#[component]
pub fn Dashboard() -> impl IntoView {
    let usage = expect_context::<Usage>();
    let spent = RwSignal::new(String::new());
    let saving = RwSignal::new(false);
    let problem = RwSignal::new(None::<String>);
    let can_change = move || {
        usage
            .summary
            .with(|s| s.as_ref().is_some_and(|s| s.can_change))
    };

    let checked = move || {
        usage.summary.with(|summary| {
            let summary = summary.as_ref()?;
            Some(match summary.checked_at {
                Some(at) => format!(
                    "Last checked {}. With the other apps' spend carried on at their rate since, \
                     the account is at about {} now.",
                    time::ago(at, summary.now),
                    summary.account.text
                ),
                None => "Not checked this month: other apps' spend isn't known, so it isn't left room for.".to_owned(),
            })
        })
    };
    let dashboard = move || {
        usage
            .summary
            .with(|s| s.as_ref().map(|s| s.dashboard.clone()).unwrap_or_default())
    };

    view! {
        <section class="card" aria-labelledby="dashboard-title">
            <h2 id="dashboard-title">"Other apps on the account"</h2>
            <p>"Tesla doesn't tell an app what the account has spent, and every app on it (Home Assistant's Tesla, say) spends from the same credit. Now and then, read "
               <b>"Spent so far this month"</b>
               " off Tesla's developer dashboard and enter it here: Irori works out what the other apps spend a day and leaves room for them."</p>
            <p class="muted small">{checked}</p>
            <p class="muted small">"Tesla's dashboard: " <span class="mono select">{dashboard}</span></p>
            <form class="field" on:submit=move |ev| {
                ev.prevent_default();
                let figure = spent.get_untracked();
                if figure.trim().is_empty() {
                    return;
                }
                saving.set(true);
                problem.set(None);
                spawn_local(async move {
                    match api::calibrate(&figure).await {
                        Ok(summary) => {
                            spent.set(String::new());
                            usage.changed(summary);
                        }
                        Err(why) => problem.set(Some(why)),
                    }
                    saving.set(false);
                });
            }>
                <label for="spent">"Spent so far this month, from Tesla's dashboard"</label>
                <div class="row">
                    <input id="spent" type="text" inputmode="decimal" autocomplete="off" placeholder="3.42"
                        prop:value=move || spent.get()
                        on:input=move |ev| spent.set(event_target_value(&ev))
                        disabled=move || !can_change()/>
                    <button class="btn primary" type="submit"
                        disabled=move || !can_change() || saving.get() || spent.with(|s| s.trim().is_empty())>
                        "Update"
                    </button>
                </div>
            </form>
            {move || problem.get().map(|why| view! { <p class="note error" role="alert">{why}</p> })}
        </section>
    }
}
