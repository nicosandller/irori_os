//! Every update in the home in one place, whatever protocol it came through: a line above the
//! tables saying how many there are, the list behind it, and one button to install them all.
//! "All" is still one at a time on each protocol; the core keeps the line (`installs.rs`).

use std::collections::{HashMap, HashSet};

use irori_types::{Availability, Capabilities, Entity, EntityId, EntityState, State, UpdateState};
use leptos::prelude::*;

use crate::api::Home;
use crate::devices::Controls;

/// Every update entity with how it stands: its state, when it has said, and whether it waits
/// its turn. One pass over the home, however large it is.
fn updates(home: &Home) -> Vec<(&Entity, Option<&EntityState>, bool)> {
    let states: HashMap<&EntityId, &EntityState> = home
        .states
        .iter()
        .map(|state| (&state.entity_id, state))
        .collect();
    let waiting: HashSet<&EntityId> = home.installs.iter().map(|q| &q.entity_id).collect();
    home.entities
        .iter()
        .filter(|entity| matches!(entity.capabilities, Capabilities::Update(_)))
        .map(|entity| {
            (
                entity,
                states.get(&entity.id).copied(),
                waiting.contains(&entity.id),
            )
        })
        .collect()
}

fn update_of(state: Option<&EntityState>) -> Option<&UpdateState> {
    match &state?.state {
        Some(State::Update(update)) => Some(update),
        _ => None,
    }
}

/// The updates worth a person's attention: available, installing, or waiting their turn.
/// Skipped ones and up-to-date ones stay in the tables. Sorted by name, so the list holds still.
pub(crate) fn noteworthy(home: &Home) -> Vec<Entity> {
    let mut entities: Vec<Entity> = updates(home)
        .into_iter()
        .filter(|(_, state, waiting)| {
            *waiting
                || update_of(*state).is_some_and(|update| update.available || update.in_progress)
        })
        .map(|(entity, _, _)| entity.clone())
        .collect();
    entities.sort_by(|a, b| title(home, a).cmp(&title(home, b)).then(a.id.cmp(&b.id)));
    entities
}

/// The ones "Update all" would install: available, able to install, reachable, and neither
/// installing nor already waiting.
pub(crate) fn installable(home: &Home) -> Vec<EntityId> {
    let mut ready: Vec<EntityId> = updates(home)
        .into_iter()
        .filter(|(entity, state, waiting)| {
            matches!(&entity.capabilities, Capabilities::Update(caps) if caps.install)
                && !waiting
                && state.is_some_and(|state| state.availability != Availability::Unavailable)
                && update_of(*state).is_some_and(|update| update.available && !update.in_progress)
        })
        .map(|(entity, _, _)| entity.id.clone())
        .collect();
    ready.sort();
    ready
}

/// What an update is for: its device. Irori's own are all on one device, so those go by their
/// own names ("Irori", "MQTT extension"), as does one with no device.
fn title(home: &Home, entity: &Entity) -> String {
    entity
        .device_id
        .as_ref()
        .filter(|_| entity.protocol.as_str() != "irori")
        .and_then(|id| home.devices.iter().find(|device| device.id == *id))
        .map_or_else(|| entity.name.to_string(), |device| device.name.to_string())
}

/// The line above the tables, and the list it opens. Nothing at all while nothing is pending.
#[component]
pub(crate) fn Updates() -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let controls = expect_context::<Controls>();
    // Only what the list is made of, so a reading elsewhere in the home redraws nothing here.
    let listed = Memo::new(move |_| {
        live.home.with(|home| {
            noteworthy(home)
                .into_iter()
                .map(|entity| (title(home, &entity), entity))
                .collect::<Vec<_>>()
        })
    });
    let ready = Memo::new(move |_| live.home.with(installable));
    // Whether there's a list at all, apart from what's in it: a row coming or going mustn't
    // redraw the others, one of which may have its window open.
    let any = Memo::new(move |_| listed.with(|listed| !listed.is_empty()));
    let open = RwSignal::new(false);
    let asking = RwSignal::new(false);
    let install_all = move || {
        for id in ready.get_untracked() {
            controls.act.run((id, "install", None));
        }
        asking.set(false);
    };

    view! {
        {move || {
            let count = listed.with(Vec::len);
            (count > 0).then(|| view! {
                <p class="nudge updates-nudge">
                    {format!("{count} update{} ", if count == 1 { "" } else { "s" })}
                    <button
                        type="button"
                        class="quiet-button"
                        aria-expanded=move || open.get().to_string()
                        on:click=move |_| open.update(|open| *open = !*open)
                    >
                        {move || if open.get() { "Hide" } else { "Show" }}
                    </button>
                    {move || {
                        let ready = ready.with(Vec::len);
                        (ready > 1).then(|| view! {
                            <button
                                type="button"
                                class="quiet-button"
                                on:click=move |_| asking.set(true)
                            >
                                "Update all"
                            </button>
                        })
                    }}
                </p>
            })
        }}
        {move || (open.get() && any.get()).then(|| view! {
            <section class="card updates-list">
                <ul class="room-devices">
                    <For
                        each=move || listed.get()
                        key=|(title, entity)| (entity.id.clone(), title.clone())
                        children=move |(title, entity)| {
                            let id = entity.id.clone();
                            let state = Memo::new(move |_| {
                                live.home.with(|home| {
                                    home.states.iter().find(|s| s.entity_id == id).cloned()
                                })
                            });
                            let offline = Memo::new(move |_| {
                                state.with(|state| {
                                    state.as_ref().is_some_and(|state| {
                                        state.availability == Availability::Unavailable
                                    })
                                })
                            });
                            let failure = {
                                let id = entity.id.clone();
                                move || controls.failures.with(|failures| failures.get(&id).cloned())
                            };
                            let control =
                                super::kept(&entity, state.into(), offline.into(), controls);
                            view! {
                                <li>
                                    <span class="name">{title}</span>
                                    <span class="room-actions">{control}</span>
                                    {move || failure().map(|why| view! {
                                        <span class="update-error small" role="status">{why}</span>
                                    })}
                                </li>
                            }
                        }
                    />
                </ul>
            </section>
        })}
        {move || asking.get().then(|| {
            let count = ready.with_untracked(Vec::len);
            view! {
                <crate::modal::Modal
                    title=format!("Install {count} updates?")
                    on_close=Callback::new(move |()| asking.set(false))
                >
                    <p>
                        "Each device can't be used while it installs, and restarts when it's "
                        "done. Devices on the same network are updated one after another, so "
                        "this can take a while."
                    </p>
                    <div class="ext-actions">
                        <button type="button" class="danger" on:click=move |_| asking.set(false)>
                            "Cancel"
                        </button>
                        <button type="button" class="add solid" on:click=move |_| install_all()>
                            "Update all"
                        </button>
                    </div>
                </crate::modal::Modal>
            }
        })}
    }
}

#[cfg(test)]
mod tests {
    use irori_types::UpdateCapabilities;

    use super::*;
    use crate::api::QueuedInstall;

    fn update(id: &str, install: bool) -> Entity {
        serde_json::from_value(serde_json::json!({
            "id": id, "protocol": "zigbee", "unique_id": id, "name": id,
            "capabilities": Capabilities::Update(UpdateCapabilities {
                device_class: None,
                install,
                check: false,
            }),
        }))
        .expect("a valid entity")
    }

    fn state(id: &str, update: UpdateState) -> EntityState {
        serde_json::from_value(serde_json::json!({
            "entity_id": id, "availability": "available",
            "state": State::Update(update),
            "last_changed": "2026-09-15T22:04:12Z", "last_updated": "2026-09-15T22:04:12Z",
            "last_reported": "2026-09-15T22:04:12Z",
            "context": {"id": "01K5B2Q7M0N1P2Q3R4S5T6V7W8", "origin": {"type": "system"}},
        }))
        .expect("a valid state")
    }

    fn at(installed: &str, latest: &str, in_progress: bool) -> UpdateState {
        UpdateState {
            available: installed != latest,
            installed_version: Some(installed.into()),
            latest_version: Some(latest.into()),
            in_progress,
            ..UpdateState::default()
        }
    }

    #[test]
    fn the_list_is_what_needs_attention_and_update_all_is_what_can_start() {
        let ids = ["update.a", "update.b", "update.c", "update.d", "update.e"];
        let home = Home {
            devices: Vec::new(),
            entities: vec![
                update(ids[0], true),
                update(ids[1], true),
                update(ids[2], true),
                update(ids[3], true),
                // Says there's an update, can't be told to install it.
                update(ids[4], false),
            ],
            states: vec![
                state(ids[0], at("1", "2", false)),
                state(ids[1], at("1", "2", true)),
                state(ids[2], at("2", "2", false)),
                state(ids[3], at("1", "2", false)),
                state(ids[4], at("1", "2", false)),
            ],
            extensions: Default::default(),
            areas: Vec::new(),
            held: Vec::new(),
            floors: Vec::new(),
            floorplan: Default::default(),
            installs: vec![QueuedInstall {
                entity_id: ids[3].parse().expect("an entity id"),
                ahead: 1,
            }],
        };
        let listed: Vec<String> = noteworthy(&home)
            .into_iter()
            .map(|entity| entity.id.to_string())
            .collect();
        assert_eq!(
            listed,
            [ids[0], ids[1], ids[3], ids[4]],
            "not the one up to date"
        );
        let ready: Vec<String> = installable(&home).iter().map(ToString::to_string).collect();
        assert_eq!(ready, [ids[0]], "not installing, not waiting, and able to");
    }
}
