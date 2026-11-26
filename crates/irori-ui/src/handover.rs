//! Handing a device over to another way of reaching it, on the device's page: a device someone
//! tried through a bridge (Home Assistant) moves to the extension that reaches it directly, and
//! can be handed back (`docs/specs/api.md` §3).
//!
//! The card asks Irori what handing over would do, and only offers it when there is somewhere to
//! hand it to. The window says what moves, what follows it, and what can't, before anything is
//! done: closing it is cancelling, since nothing was sent.

use irori_types::DeviceId;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{self, HandoverPlan};

/// The card on a device's page that offers to hand it over, or back. Draws nothing when there is
/// nothing to hand it over to.
#[component]
pub fn HandoverCard(device: DeviceId) -> impl IntoView {
    let plan = LocalResource::new(move || {
        let device = device.clone();
        async move { api::fetch_handover(&device).await }
    });
    let asking = RwSignal::new(false);
    move || {
        let Some(Ok(Some(plan))) = plan.get() else {
            return ().into_any();
        };
        let (heading, said, button) = if plan.back {
            (
                "Handed over".to_owned(),
                format!(
                    "{} used to be reached through {}. You can hand it back: it keeps its name, \
                     room, spot on the floorplan and history, and the automations that use it \
                     follow it.",
                    plan.name, plan.to.through
                ),
                format!("Hand back to {}…", plan.to.through),
            )
        } else {
            (
                format!("Use {} instead", plan.to.through),
                format!(
                    "{} reaches this device directly. Hand it over and the device keeps its \
                     name, room, spot on the floorplan and history, and the automations that use \
                     it follow it. Nothing is changed in {}, and you can hand it back later.",
                    plan.to.through, plan.from.through
                ),
                format!("Hand over to {}…", plan.to.through),
            )
        };
        let shown = plan.clone();
        view! {
            <section class="card handover-card">
                <h2>{heading}</h2>
                <p class="muted">{said}</p>
                <button type="button" on:click=move |_| asking.set(true)>{button}</button>
                {move || asking.get().then(|| view! {
                    <HandOver plan=shown.clone() on_close=move || asking.set(false) />
                })}
            </section>
        }
        .into_any()
    }
}

/// The window that says what handing over does, and does it.
#[component]
fn HandOver(plan: HandoverPlan, #[prop(into)] on_close: Callback<()>) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let navigate = leptos_router::hooks::use_navigate();
    let busy = RwSignal::new(false);
    let trouble = RwSignal::new(None::<String>);
    let room = plan.area.as_ref().and_then(|area| {
        live.home
            .with_untracked(|home| home.area(area).map(|area| area.name.to_string()))
    });
    let (verb, doing) = if plan.back {
        ("Hand back", "Handing back…")
    } else {
        ("Hand over", "Handing over…")
    };
    let title = format!(
        "{} {} {} {}?",
        verb,
        plan.name,
        if plan.back { "to" } else { "to" },
        plan.to.through
    );
    // Where nothing is written: the bridge's side, whichever way this goes.
    let bridge = if plan.back {
        plan.to.through.clone()
    } else {
        plan.from.through.clone()
    };
    let blocked = plan.blocked.clone();
    let (from, to) = (plan.from.id.clone(), plan.to.id.clone());
    let go = move |_| {
        let (from, to) = (from.clone(), to.clone());
        let navigate = navigate.clone();
        busy.set(true);
        trouble.set(None);
        spawn_local(async move {
            match api::hand_over(&from, &to).await {
                Ok(()) => {
                    crate::refresh(live);
                    navigate(&format!("/devices/{to}"), Default::default());
                }
                Err(why) => {
                    busy.set(false);
                    trouble.set(Some(why));
                }
            }
        });
    };
    let uses = |list: &[api::HandoverUses]| {
        list.iter()
            .map(|uses| format!("“{}”", uses.name))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let automations = uses(&plan.automations);
    let helpers = uses(&plan.helpers);
    let left: Vec<String> = plan
        .left_behind
        .iter()
        .map(|entity| entity.name.to_string())
        .collect();
    let to_through = plan.to.through.clone();
    let from_through = plan.from.through.clone();

    view! {
        <crate::modal::Modal title=title on_close=move || on_close.run(())>
            <div class="confirm handover">
                <p>
                    {format!("{} goes through {to_through} from now on. It keeps:", plan.name)}
                </p>
                <ul class="handover-keeps">
                    <li>{format!("its name, {}", plan.name)}</li>
                    <li>{match room {
                        Some(room) => format!("its room, {room}"),
                        None => "no room, as now".to_owned(),
                    }}</li>
                    <li>"its spot on the floorplan"</li>
                    <li>{format!("its history: what came through {from_through} stays, \
                                  shown before what {to_through} records")}</li>
                </ul>
                {(!plan.entities.is_empty()).then(|| view! {
                    <p class="muted small">
                        {format!(
                            "{} of its entities are matched with {to_through}'s: {}.",
                            plan.entities.len(),
                            plan.entities
                                .iter()
                                .map(|pair| pair.name.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )}
                    </p>
                })}
                {(!automations.is_empty()).then(|| view! {
                    <p>{format!("These automations are changed to follow it: {automations}.")}</p>
                })}
                {(!helpers.is_empty()).then(|| view! {
                    <p>{format!("So are these helpers: {helpers}.")}</p>
                })}
                {(!left.is_empty()).then(|| view! {
                    <p class="muted small">
                        {format!(
                            "{to_through} has nothing for {}, so {} stay{} behind with the \
                             old connection, history included.",
                            left.join(", "),
                            if left.len() == 1 { "it" } else { "they" },
                            if left.len() == 1 { "s" } else { "" },
                        )}
                    </p>
                })}
                <p class="muted small">
                    {format!("Nothing is changed in {bridge}. You can hand it back from this \
                              device's page.")}
                </p>
                {blocked.clone().map(|why| view! { <p class="why" role="alert">{why}</p> })}
                {move || trouble.get().map(|why| view! { <p class="why" role="alert">{why}</p> })}
                <div class="confirm-actions">
                    <button type="button" on:click=move |_| on_close.run(())>"Cancel"</button>
                    <button
                        type="button"
                        class="primary"
                        disabled=move || busy.get() || blocked.is_some()
                        on:click=go
                    >
                        {move || if busy.get() { doing } else { verb }}
                    </button>
                </div>
            </div>
        </crate::modal::Modal>
    }
}
