//! The Devices and Entities lists: compact rows in groups that fold, with a switch for what
//! they're grouped by.
//!
//! Both are drawn from what things *are* (`grouping.rs`) and nothing else, so they're only
//! drawn again when a device joins, a filter is typed or the grouping changes. What a thing is
//! *reporting* — a battery level, a reading, a switch's position — is watched by its own row.
//! That's what lets a history drawer stay open, and a fold finish folding, while a presence
//! sensor reports every second.

use std::collections::BTreeSet;

use irori_types::{Availability, Capabilities, EntityId};
use leptos::prelude::*;
use leptos_router::components::A;

use super::grouping::{self, DeviceLine, DevicesBy, EntitiesBy, EntityLine, Section};
use super::{Controls, remember, stored};
use crate::fold::{Head, fold};
use crate::history::Histories;
use crate::segmented::segmented;

/// What each list is grouped by, and which of its groups are folded away. Remembered per
/// browser, like the view: preferences about reading, nothing Irori needs to know.
const DEVICES_BY_KEY: &str = "irori.devices.group";
const DEVICES_FOLDED_KEY: &str = "irori.devices.folded";
const ENTITIES_BY_KEY: &str = "irori.entities.group";
const ENTITIES_FOLDED_KEY: &str = "irori.entities.folded";

/// Regrouping moves each row to its new place rather than swapping one list for another. The
/// browser pictures every row that moves, so past this many the list just changes.
const ROWS_THAT_TRAVEL: usize = 80;

/// One row per device: what it is and where it is.
///
/// Built from the devices rather than from their entities, so a device Irori is connected to
/// still appears when it provides nothing Irori can model — a Bluetooth proxy, say. Those are
/// invisible in the entity view by their nature, and being unable to find them would be worse.
#[component]
pub fn DeviceTable(filter: RwSignal<String>) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let by = RwSignal::new(
        stored(DEVICES_BY_KEY)
            .and_then(|saved| DevicesBy::from_key(&saved))
            .unwrap_or(DevicesBy::Protocol),
    );
    let folded = remembered_folds(DEVICES_FOLDED_KEY);
    Effect::new(move |_| remember(DEVICES_BY_KEY, by.get().key()));

    let drawn = Memo::new(move |_| {
        let (needle, by) = (filter.get(), by.get());
        let sections = live.home.with(|home| grouping::devices(home, &needle, by));
        (by, !needle.trim().is_empty(), sections)
    });
    let any = Memo::new(move |_| live.home.with(|home| !home.devices.is_empty()));

    view! {
        <div class="list-bar">
            <span class="list-bar-label">"Group by"</span>
            {segmented(
                "Group devices by",
                DevicesBy::ALL.into_iter().map(|by| (by, by.label())).collect(),
                by.into(),
                move |picked| regroup(move || by.set(picked)),
            )}
        </div>
        {move || {
            let (by, filtering, sections) = drawn.get();
            if sections.is_empty() {
                let message = if any.get() {
                    "Nothing matches that."
                } else {
                    "No devices yet. Extensions bring them in; \"Add device\" says how."
                };
                return view! { <p class="empty">{message}</p> }.into_any();
            }
            let travels = grouping::count(&sections) <= ROWS_THAT_TRAVEL;
            view! {
                <div class=format!("table-scroll grouped device-list by-{}", by.key())>
                    <div class="grouped-inner">
                        // The headings, once, for the eye. Each group says them again for a
                        // screen reader, which reads a group as its own table.
                        <div class="line columns" aria-hidden="true">{device_columns(by)}</div>
                        {sections
                            .into_iter()
                            .map(|section| device_group(section, by, folded, filtering, travels))
                            .collect_view()}
                    </div>
                </div>
            }
            .into_any()
        }}
    }
}

/// The device table's column headings, without the one the groups already say.
fn device_columns(by: DevicesBy) -> AnyView {
    let heading = |words: &'static str, class: &'static str| {
        view! { <span class=format!("cell {class}") role="columnheader">{words}</span> }
    };
    view! {
        <span class="cell icon-col" role="columnheader">
            <span class="visually-hidden">"Protocol"</span>
        </span>
        {heading("Device", "")}
        {(by != DevicesBy::Area).then(|| heading("Area", ""))}
        {(by != DevicesBy::Make).then(|| heading("Make", ""))}
        {heading("Model", "")}
        {heading("Battery", "battery")}
        {heading("Entities", "number")}
    }
    .into_any()
}

/// One group of devices: a heading that folds them away, and a row each.
fn device_group(
    section: Section<DeviceLine>,
    by: DevicesBy,
    folded: RwSignal<BTreeSet<String>>,
    filtering: bool,
    travels: bool,
) -> AnyView {
    let count = section.rows.len();
    let label = section.label.clone();
    let rows = section
        .rows
        .into_iter()
        .map(|line| device_row(line, by, travels))
        .collect_view();
    let body = view! {
        <div class="lines" role="table" aria-label=label>
            <div class="line visually-hidden" role="row">{device_columns(by)}</div>
            {rows}
        </div>
    }
    .into_any();
    group(
        section.key,
        section.label,
        section.note,
        section.icon,
        format!("{count} device{}", if count == 1 { "" } else { "s" }),
        folded,
        filtering,
        body,
    )
}

fn device_row(line: DeviceLine, by: DevicesBy, travels: bool) -> AnyView {
    let live = expect_context::<crate::Live>();
    let DeviceLine {
        device,
        area,
        icon,
        entities,
    } = line;
    let id = device.id.to_string();
    // The one thing on the row that is a reading, watched on its own.
    let battery = {
        let id = device.id.clone();
        Memo::new(move |_| live.home.with(|home| super::battery_of(home, &id)))
    };
    view! {
        <div class="line device-line" role="row" style=travelling_row(&id, travels)>
            <span class="cell icon-col" role="cell">
                {super::icon(&icon.protocol, icon.has_icon)}
            </span>
            <span class="cell name-col" role="rowheader">
                <A
                    href=format!("/devices/{id}")
                    attr:style=crate::transition::list_name(&id)
                    on:click={
                        let (id, travelling) = (
                            id.clone(),
                            expect_context::<crate::transition::Travelling>().0,
                        );
                        move |_| travelling.set(Some(id.clone()))
                    }
                >
                    {device.name.to_string()}
                </A>
                {device.description.as_ref().map(|description| view! {
                    <span class="description">{description.to_string()}</span>
                })}
            </span>
            {(by != DevicesBy::Area).then(|| view! {
                <span class="cell room" role="cell">{area.unwrap_or_else(|| "—".to_owned())}</span>
            })}
            {(by != DevicesBy::Make).then(|| view! {
                <span class="cell" role="cell">{device.manufacturer.clone().unwrap_or_default()}</span>
            })}
            <span class="cell" role="cell">{device.model.clone().unwrap_or_default()}</span>
            <span class="cell battery" role="cell">
                {move || battery.get().unwrap_or_else(|| "—".to_owned())}
            </span>
            <span class="cell number" role="cell">{entities}</span>
        </div>
    }
    .into_any()
}

/// Every entity with its reading and its control, a compact row each, with its last 24 hours
/// a click away.
#[component]
pub fn EntityTable(filter: RwSignal<String>) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let by = RwSignal::new(
        stored(ENTITIES_BY_KEY)
            .and_then(|saved| EntitiesBy::from_key(&saved))
            .unwrap_or(EntitiesBy::Device),
    );
    let folded = remembered_folds(ENTITIES_FOLDED_KEY);
    Effect::new(move |_| remember(ENTITIES_BY_KEY, by.get().key()));
    // Here rather than in the rows: regrouping draws every row again, and an open drawer and
    // the day it loaded should both come through that.
    let histories = Histories::new();

    let drawn = Memo::new(move |_| {
        let (needle, by) = (filter.get(), by.get());
        let sections = live.home.with(|home| grouping::entities(home, &needle, by));
        (by, !needle.trim().is_empty(), sections)
    });
    let any = Memo::new(move |_| live.home.with(|home| !home.entities.is_empty()));

    view! {
        <div class="list-bar">
            <span class="list-bar-label">"Group by"</span>
            {segmented(
                "Group entities by",
                EntitiesBy::ALL.into_iter().map(|by| (by, by.label())).collect(),
                by.into(),
                move |picked| regroup(move || by.set(picked)),
            )}
        </div>
        {move || {
            let (by, filtering, sections) = drawn.get();
            if sections.is_empty() {
                let message = if any.get() {
                    "Nothing matches that."
                } else {
                    "No devices yet. Extensions bring them in; the demo extension provides a few."
                };
                return view! { <p class="empty">{message}</p> }.into_any();
            }
            let travels = grouping::count(&sections) <= ROWS_THAT_TRAVEL;
            view! {
                <div class=format!("table-scroll grouped entity-list by-{}", by.key())>
                    <div class="grouped-inner">
                        {sections
                            .into_iter()
                            .map(|section| {
                                entity_group(section, by, folded, filtering, travels, histories)
                            })
                            .collect_view()}
                    </div>
                </div>
            }
            .into_any()
        }}
    }
}

fn entity_group(
    section: Section<EntityLine>,
    by: EntitiesBy,
    folded: RwSignal<BTreeSet<String>>,
    filtering: bool,
    travels: bool,
    histories: Histories,
) -> AnyView {
    let count = section.rows.len();
    let label = section.label.clone();
    let rows = section
        .rows
        .into_iter()
        .map(|line| entity_row(line, by, travels, histories))
        .collect_view();
    let body = view! {
        <div class="lines" role="table" aria-label=label>
            <div class="line visually-hidden" role="row">
                <span role="columnheader">"Entity"</span>
                {(by != EntitiesBy::Device).then(|| view! {
                    <span role="columnheader">"Device"</span>
                })}
                <span role="columnheader">"State"</span>
            </div>
            {rows}
        </div>
    }
    .into_any();
    group(
        section.key,
        section.label,
        section.note,
        section.icon,
        format!("{count} entit{}", if count == 1 { "y" } else { "ies" }),
        folded,
        filtering,
        body,
    )
}

fn entity_row(line: EntityLine, by: EntitiesBy, travels: bool, histories: Histories) -> AnyView {
    let live = expect_context::<crate::Live>();
    let controls = expect_context::<Controls>();
    let EntityLine { entity, device } = line;
    let id: EntityId = entity.id.clone();
    let shown = id.to_string();
    let state = {
        let id = id.clone();
        Memo::new(move |_| {
            live.home.with(|home| {
                home.states
                    .iter()
                    .find(|state| state.entity_id == id)
                    .cloned()
            })
        })
    };
    let offline = Memo::new(move |_| {
        state.with(|state| {
            state
                .as_ref()
                .is_some_and(|state| state.availability == Availability::Unavailable)
        })
    });
    let failure = {
        let id = id.clone();
        move || {
            controls
                .failures
                .with(|failures| failures.get(&id).cloned())
        }
    };
    // A button has nothing to remember from one day to the next.
    let has_history = !matches!(entity.capabilities, Capabilities::Button(_));
    let unroll = has_history.then(|| histories.unroll(&id));
    // A player, a lock or a cover is drawn once and follows its state; everything else is
    // drawn again from each reading.
    let control = match super::kept(&entity, state.into(), offline.into(), controls) {
        Some(kept) => super::unrolling(&entity, kept, unroll),
        None => {
            let entity = entity.clone();
            (move || {
                let state = state.get();
                let offline = offline.get();
                super::unrolling(
                    &entity,
                    super::control(&entity, state.as_ref(), offline, controls),
                    unroll,
                )
            })
            .into_any()
        }
    };
    let drawer = unroll.map(|unroll| {
        let open = unroll.open;
        let panel = crate::device::history_panel(entity.clone(), histories.day(&id), state.into());
        view! {
            <div class="drawer" class:open=move || open.get() inert=move || (!open.get()).then_some("")>
                <div class="drawer-inner">{panel}</div>
            </div>
        }
    });
    view! {
        <div class="entity-block" role="rowgroup" style=travelling_row(&shown, travels)>
            <div class="line entity-line" role="row" class:offline=move || offline.get()>
                <span class="cell names iconed" role="rowheader">
                    {crate::icons::entity(&entity.capabilities)}
                    <span class="names-text">
                        <span class="name">{entity.name.to_string()}</span>
                        <span class="id" title=shown.clone()>{shown.clone()}</span>
                    </span>
                </span>
                {(by != EntitiesBy::Device).then(|| view! {
                    <span class="cell of-device" role="cell">
                        {device.map(|(id, name)| view! {
                            <A href=format!("/devices/{id}") attr:class="row-link">{name}</A>
                        })}
                    </span>
                })}
                <span class="cell control" role="cell">
                    {move || offline.get().then(|| view! { <span class="badge">"offline"</span> })}
                    {control}
                    {move || failure().map(|why| view! { <p class="why">{why}</p> })}
                </span>
            </div>
            {drawer}
        </div>
    }
    .into_any()
}

/// A group of either list: the fold both are made of.
#[allow(clippy::too_many_arguments)]
fn group(
    key: String,
    label: String,
    note: Option<String>,
    icon: Option<grouping::ProtocolIcon>,
    count: String,
    folded: RwSignal<BTreeSet<String>>,
    // While filtering, every group with a match is open: a folded group hiding the one result
    // would look like no result at all.
    filtering: bool,
    body: AnyView,
) -> AnyView {
    let open = {
        let key = key.clone();
        Signal::derive(move || filtering || !folded.with(|folded| folded.contains(&key)))
    };
    let toggle = move || {
        folded.update(|folded| {
            if !folded.remove(&key) {
                folded.insert(key.clone());
            }
        })
    };
    let state = match note {
        Some(note) => format!("{note} · {count}"),
        None => count,
    };
    fold(
        None,
        Head {
            icon: icon.map(|icon| super::icon(&icon.protocol, icon.has_icon)),
            title: label.into_any(),
            state: Some(state.into_any()),
            actions: None,
        },
        open,
        toggle,
        body,
    )
}

/// The folded groups of a list, kept in the browser as they change.
fn remembered_folds(key: &'static str) -> RwSignal<BTreeSet<String>> {
    let folded = RwSignal::new(
        stored(key)
            .map(|saved| grouping::folded_from(&saved))
            .unwrap_or_default(),
    );
    Effect::new(move |_| remember(key, &folded.get().into_iter().collect::<Vec<_>>().join(",")));
    folded
}

/// Changes what a list is grouped by as one movement: each row travels from where it was to
/// where it now belongs (`transition.rs`, and `html[data-nav="regroup"]` in the stylesheet).
fn regroup(change: impl FnOnce() + 'static) {
    crate::transition::around("regroup", change);
}

/// What lets a row travel when the list is regrouped: a name of its own, which the stylesheet
/// only turns into a `view-transition-name` while a regrouping is under way.
fn travelling_row(id: &str, travels: bool) -> Option<String> {
    travels.then(|| format!("--vt: row-{}", crate::transition::ident(id)))
}
