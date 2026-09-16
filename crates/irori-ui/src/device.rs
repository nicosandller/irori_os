//! One device: everything Irori knows about it, everything it provides, and the two things
//! that are yours to decide — what to call it and which room it's in.
//!
//! The list pages answer "what's going on"; this one answers "what is this thing" — which
//! integration brought it in, what it calls itself, what firmware it's running, and which
//! entities belong to it.

use irori_types::{Area, AreaId, Device, Entity, EntityId, Name};
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;

use crate::api::{self, DeviceEdit, Home};

/// The two picker choices that aren't a room. Neither can collide with an area id: one is empty
/// and the other has a space in it, and a slug can have neither.
const NOWHERE: &str = "";
const LET_THE_DEVICE_SAY: &str = "let the device say";
use crate::devices::{self, Controls};
use crate::rooms::named;

/// What this page shows that isn't a live reading: the device itself, the rooms it could be in,
/// and which entities it has.
///
/// Kept apart from the readings on purpose. A chatty device — a presence sensor reports every
/// second — would otherwise rebuild the whole page that often, and a rebuilt page throws away
/// whatever half-typed name is in an open rename box. Everything a person can edit hangs off
/// this, so it only redraws when one of these things actually changes.
#[derive(Debug, Clone, PartialEq)]
struct Shape {
    device: Option<Device>,
    areas: Vec<Area>,
    entities: Vec<Entity>,
    /// Whether any device is known at all, for telling "no such device" from "nothing yet".
    any_devices: bool,
}

#[component]
pub fn DevicePage() -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let controls = expect_context::<Controls>();
    let params = use_params_map();
    let trouble = RwSignal::new(None::<String>);
    // Created here, once, rather than inside what redraws: an open rename box has to survive a
    // reading arriving underneath it.
    let renaming = RwSignal::new(false);
    let draft = RwSignal::new(String::new());
    let entity_draft = RwSignal::new(String::new());
    let editing = RwSignal::new(None::<EntityId>);

    let shape = Memo::new(move |_| {
        shape_of(
            &live.home.get(),
            &params.read().get("id").unwrap_or_default(),
        )
    });

    move || {
        let shape = shape.get();
        match shape.device.clone() {
            None => missing(
                &params.read().get("id").unwrap_or_default(),
                shape.any_devices,
            )
            .into_any(),
            Some(device) => page(
                device,
                shape,
                controls,
                trouble,
                renaming,
                draft,
                editing,
                entity_draft,
            )
            .into_any(),
        }
    }
}

/// Everything this page draws from, except the readings.
///
/// A `Memo` over this is what keeps the page still: two homes that differ only in what the
/// sensors are saying produce the same `Shape`, so the page isn't rebuilt and an open rename box
/// keeps its text and its focus.
fn shape_of(home: &Home, id: &str) -> Shape {
    let device = home
        .devices
        .iter()
        .find(|device| device.id.as_str() == id)
        .cloned();
    let entities = device
        .as_ref()
        .map(|device| of_device(home, device))
        .unwrap_or_default();
    Shape {
        device,
        areas: home.areas.clone(),
        entities,
        any_devices: !home.devices.is_empty(),
    }
}

/// The device's entities, in the order the list pages use them.
fn of_device(home: &Home, device: &Device) -> Vec<Entity> {
    devices::groups(home, "")
        .into_iter()
        .find(|group| group.device.as_ref().is_some_and(|d| d.id == device.id))
        .map(|group| {
            group
                .entities
                .into_iter()
                .map(|(entity, _)| entity)
                .collect()
        })
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn page(
    device: Device,
    shape: Shape,
    controls: Controls,
    trouble: RwSignal<Option<String>>,
    renaming: RwSignal<bool>,
    draft: RwSignal<String>,
    editing: RwSignal<Option<EntityId>>,
    entity_draft: RwSignal<String>,
) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let areas = shape.areas.clone();
    let entities = shape.entities.clone();
    let battery = battery_of(live, &entities);
    let subtitle = [device.manufacturer.clone(), device.model.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");

    let edit = {
        let id = device.id.clone();
        move |edit: DeviceEdit| {
            let id = id.clone();
            spawn_local(async move {
                match api::edit_device(&id, &edit).await {
                    Ok(()) => {
                        trouble.set(None);
                        crate::refresh(live);
                    }
                    Err(why) => trouble.set(Some(why)),
                }
            });
        }
    };

    let save_name = {
        let edit = edit.clone();
        move || {
            let Some(name) = named(draft.get(), trouble) else {
                return;
            };
            renaming.set(false);
            edit(DeviceEdit {
                name: Some(Some(name)),
                area: None,
            });
        }
    };
    let use_reported = {
        let edit = edit.clone();
        move |_| {
            edit(DeviceEdit {
                name: Some(None),
                area: None,
            })
        }
    };
    let move_to = {
        let edit = edit.clone();
        move |chosen: String| {
            // The three the picker offers. "Nowhere" is a decision, not the absence of one:
            // without it, a device whose firmware names a room would be put straight back.
            let area = match chosen.as_str() {
                NOWHERE => Some(api::WhereTo::nowhere()),
                LET_THE_DEVICE_SAY => None,
                id => match AreaId::try_from(id) {
                    Ok(id) => Some(api::WhereTo::In(id)),
                    Err(e) => {
                        trouble.set(Some(e.to_string()));
                        return;
                    }
                },
            };
            edit(DeviceEdit {
                name: None,
                area: Some(area),
            });
        }
    };

    let start = {
        let name = device.name.to_string();
        move |_| {
            draft.set(name.clone());
            renaming.set(true);
        }
    };
    let renamed_from = device.renamed_from.clone();
    let in_room = device.area_id.clone();
    let suggested = device.suggested_area.clone();
    // What the device's own suggestion amounts to right now. Irori doesn't send *why* a device
    // is where it is, and doesn't need to: comparing the suggestion with the rooms that exist
    // and the room it's in tells all three stories apart.
    let asked_for = suggested.as_ref().and_then(|suggests| {
        areas
            .iter()
            .find(|area| {
                area.name
                    .as_str()
                    .trim()
                    .eq_ignore_ascii_case(suggests.as_str().trim())
            })
            .map(|area| area.id.clone())
    });
    let suggestion = match (&suggested, &asked_for) {
        (None, _) => None,
        (Some(suggests), None) => Some(format!(
            "The device says it's in \"{suggests}\". Irori doesn't make rooms on its own, but a \
             room called that would collect it."
        )),
        (Some(suggests), Some(room)) if in_room.as_ref() == Some(room) => Some(format!(
            "The device says it's in \"{suggests}\", and it is."
        )),
        // The room exists and the device isn't in it: somebody decided otherwise.
        (Some(suggests), Some(_)) => Some(format!(
            "The device says it's in \"{suggests}\", but it's been put elsewhere. Choose \
             \"Wherever the device says\" to let it decide again."
        )),
    };

    view! {
        <p class="crumb"><A href="/devices">"← All devices"</A></p>
        <div class="page-head">
            {move || {
                if renaming.get() {
                    let save_name = save_name.clone();
                    view! {
                        <form
                            class="inline-form"
                            on:submit=move |ev| {
                                ev.prevent_default();
                                save_name();
                            }
                        >
                            <input
                                type="text"
                                aria-label="What to call this device"
                                prop:value=draft
                                on:input:target=move |ev| draft.set(ev.target().value())
                            />
                            <button type="submit" class="add">"Save"</button>
                            <button type="button" on:click=move |_| renaming.set(false)>
                                "Cancel"
                            </button>
                        </form>
                    }
                    .into_any()
                } else {
                    view! { <h1>{device.name.to_string()}</h1> }.into_any()
                }
            }}
            <button type="button" on:click=start>"Rename"</button>
        </div>
        <p class="lede">{subtitle}</p>

        {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}

        <section class="card">
            <h2>"Where it is"</h2>
            <label class="room-picker">
                <span>"Room"</span>
                <select
                    on:change:target=move |ev| move_to(ev.target().value())
                    prop:value=in_room
                        .as_ref()
                        .map(AreaId::to_string)
                        .unwrap_or_else(|| NOWHERE.to_owned())
                >
                    <option value=NOWHERE selected=in_room.is_none()>"Not in a room"</option>
                    {areas
                        .iter()
                        .map(|area| {
                            let id = area.id.to_string();
                            view! {
                                <option value=id.clone() selected=in_room
                                    .as_ref()
                                    .is_some_and(|chosen| chosen.as_str() == id)
                                >
                                    {area.name.to_string()}
                                </option>
                            }
                        })
                        .collect_view()}
                    // Only worth offering when there's something to go back to.
                    {suggested.is_some().then(|| view! {
                        <option value=LET_THE_DEVICE_SAY>"Wherever the device says"</option>
                    })}
                </select>
            </label>
            {if areas.is_empty() {
                view! {
                    <p class="muted small">
                        "No rooms yet. " <A href="/rooms">"Make one"</A>
                        " and this device can go in it."
                    </p>
                }
                .into_any()
            } else {
                ().into_any()
            }}
            {suggestion.map(|note| view! { <p class="muted small">{note}</p> })}
        </section>

        <section class="card">
            <h2>"What it is"</h2>
            <dl>
                {renamed_from.map(|was| view! {
                    <dt>"Called by the device"</dt>
                    <dd>
                        {was.to_string()}
                        <button type="button" class="link" on:click=use_reported>
                            "Use this name"
                        </button>
                    </dd>
                })}
                <dt>"Through"</dt>
                <dd>{device.integration.to_string()}</dd>
                // For an ESPHome device this is its MAC address; every integration picks
                // something of its own that survives a rename.
                <dt>"Known to it as"</dt>
                <dd>{device.unique_id.to_string()}</dd>
                <dt>"Irori's id"</dt>
                <dd>{device.id.to_string()}</dd>
                {device.manufacturer.clone().map(|make| view! {
                    <dt>"Make"</dt>
                    <dd>{make}</dd>
                })}
                {device.model.clone().map(|model| view! {
                    <dt>"Model"</dt>
                    <dd>{model}</dd>
                })}
                {device.sw_version.clone().map(|version| view! {
                    <dt>"Firmware"</dt>
                    <dd>{version}</dd>
                })}
                {device.hw_version.clone().map(|version| view! {
                    <dt>"Hardware"</dt>
                    <dd>{version}</dd>
                })}
                {move || battery.get().map(|level| view! {
                    <dt>"Battery"</dt>
                    <dd>{level}</dd>
                })}
                {device.via_device_id.clone().map(|via| view! {
                    <dt>"Reached through"</dt>
                    <dd><A href=format!("/devices/{via}")>{via.to_string()}</A></dd>
                })}
            </dl>
            {device.sw_version.as_ref().map(|_| view! {
                <p class="muted small">
                    "Irori can read the firmware version but can't install updates yet; see the "
                    "roadmap (M1.8)."
                </p>
            })}
        </section>

        <section class="card">
            <h2>
                {format!(
                    "{} entit{}",
                    entities.len(),
                    if entities.len() == 1 { "y" } else { "ies" },
                )}
            </h2>
            {if entities.is_empty() {
                view! {
                    <p class="muted">
                        "Nothing Irori can model yet. The device may provide kinds it doesn't "
                        "know about, which are left out rather than guessed at."
                    </p>
                }
                .into_any()
            } else {
                entities
                    .into_iter()
                    .map(|entity| {
                        view! {
                            <EntityRow
                                entity=entity
                                controls=controls
                                trouble=trouble
                                editing=editing
                                draft=entity_draft
                            />
                        }
                    })
                    .collect_view()
                    .into_any()
            }}
        </section>
    }
}

/// One entity: its reading, and the name it can be given.
///
/// The reading is the only reactive part. Keeping the rename box out of it is what lets you
/// finish typing a name while a presence sensor reports every second underneath.
#[component]
fn EntityRow(
    entity: Entity,
    controls: Controls,
    trouble: RwSignal<Option<String>>,
    editing: RwSignal<Option<EntityId>>,
    draft: RwSignal<String>,
) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let id = entity.id.clone();
    let state = {
        let id = id.clone();
        Memo::new(move |_| {
            live.home
                .get()
                .states
                .iter()
                .find(|state| state.entity_id == id)
                .cloned()
        })
    };

    let send = {
        let id = id.clone();
        move |name: Option<Name>| {
            let id = id.clone();
            editing.set(None);
            spawn_local(async move {
                match api::rename_entity(&id, name).await {
                    Ok(()) => {
                        trouble.set(None);
                        crate::refresh(live);
                    }
                    Err(why) => trouble.set(Some(why)),
                }
            });
        }
    };
    let save = {
        let send = send.clone();
        move || {
            if let Some(name) = named(draft.get(), trouble) {
                send(Some(name));
            }
        }
    };
    let reset = {
        let send = send.clone();
        move |_| send(None)
    };
    let start = {
        let id = id.clone();
        let name = entity.name.to_string();
        move |_| {
            draft.set(name.clone());
            editing.set(Some(id.clone()));
        }
    };
    let being_edited = {
        let id = id.clone();
        move || editing.get().as_ref() == Some(&id)
    };
    let was = entity.renamed_from.clone();
    let row = entity.clone();

    view! {
        <div class="entity-row">
            {move || devices::row(row.clone(), state.get(), controls)}
            <div class="entity-name">
                {move || {
                    if being_edited() {
                        let save = save.clone();
                        view! {
                            <form
                                class="inline-form"
                                on:submit=move |ev| {
                                    ev.prevent_default();
                                    save();
                                }
                            >
                                <input
                                    type="text"
                                    aria-label="What to call this entity"
                                    prop:value=draft
                                    on:input:target=move |ev| draft.set(ev.target().value())
                                />
                                <button type="submit" class="add">"Save"</button>
                                <button type="button" on:click=move |_| editing.set(None)>
                                    "Cancel"
                                </button>
                            </form>
                        }
                        .into_any()
                    } else {
                        view! {
                            <button type="button" class="link" on:click=start.clone()>
                                "Rename"
                            </button>
                        }
                        .into_any()
                    }
                }}
                {was.map(|was| view! {
                    <span class="muted small">
                        {format!("was \"{was}\"")}
                        <button type="button" class="link" on:click=reset>"undo"</button>
                    </span>
                })}
            </div>
        </div>
    }
}

/// A device's battery, from whichever of its entities reports one.
///
/// A reading like any other, so it follows the readings rather than the page's shape: a battery
/// level that arrives after the page is drawn — or drops overnight — has to show up.
fn battery_of(live: crate::Live, entities: &[Entity]) -> Memo<Option<String>> {
    let entities = entities.to_vec();
    Memo::new(move |_| {
        let home = live.home.get();
        let with_state: Vec<_> = entities
            .iter()
            .map(|entity| {
                let state = home
                    .states
                    .iter()
                    .find(|state| state.entity_id == entity.id)
                    .cloned();
                (entity.clone(), state)
            })
            .collect();
        devices::battery(&with_state)
    })
}

/// A device id that isn't here: either mistyped, or one that has gone away since the link was
/// made. Both are worth saying plainly.
fn missing(id: &str, known: bool) -> impl IntoView {
    view! {
        <section class="card">
            <h1>"No such device"</h1>
            <p class="muted">
                {if known {
                    format!("Irori has no device called `{id}`. It may have been removed.")
                } else {
                    "Irori hasn't heard from any devices yet.".to_owned()
                }}
            </p>
            <p><A href="/devices">"All devices"</A></p>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use irori_types::{
        Availability, BinarySensorCapabilities, BinarySensorState, Capabilities, Context,
        EntityState, Origin, State, Timestamp,
    };

    use super::*;

    fn device() -> Device {
        Device {
            id: "radar".parse().expect("a valid device id"),
            integration: "esphome".parse().expect("a valid integration id"),
            unique_id: "30:83:98:CA:6A:08".parse().expect("a valid unique id"),
            name: "Radar".parse().expect("a valid name"),
            renamed_from: None,
            manufacturer: None,
            model: None,
            sw_version: None,
            hw_version: None,
            area_id: None,
            suggested_area: None,
            via_device_id: None,
        }
    }

    fn entity() -> Entity {
        Entity {
            id: "binary_sensor.radar_moving"
                .parse()
                .expect("a valid entity id"),
            integration: "esphome".parse().expect("a valid integration id"),
            unique_id: "30:83:98:CA:6A:08-moving"
                .parse()
                .expect("a valid unique id"),
            name: "Moving".parse().expect("a valid name"),
            renamed_from: None,
            device_id: Some("radar".parse().expect("a valid device id")),
            area_id: None,
            capabilities: Capabilities::BinarySensor(BinarySensorCapabilities {
                device_class: None,
            }),
        }
    }

    fn reading(at: &str, on: bool) -> EntityState {
        let at: Timestamp = at.parse().expect("a valid timestamp");
        EntityState {
            entity_id: "binary_sensor.radar_moving"
                .parse()
                .expect("a valid entity id"),
            availability: Availability::Available,
            state: Some(State::BinarySensor(BinarySensorState { on })),
            attributes: Default::default(),
            last_changed: at,
            last_updated: at,
            last_reported: at,
            context: Context {
                id: "01K5B2Q9A1B2C3D4E5F6G7H8J9"
                    .parse()
                    .expect("a valid context id"),
                parent_id: None,
                origin: Origin::System,
            },
        }
    }

    fn home(state: EntityState) -> Home {
        Home {
            devices: vec![device()],
            entities: vec![entity()],
            states: vec![state],
            ..Home::default()
        }
    }

    /// The rule that keeps a half-typed name on the page. A presence sensor reports every
    /// second; if a new reading changed the page's shape, the page would be rebuilt that often
    /// and the rename box would be thrown away mid-word — which is exactly what it used to do.
    #[test]
    fn a_new_reading_doesnt_change_the_shape_of_the_page() {
        let before = shape_of(&home(reading("2026-09-16T10:00:00Z", false)), "radar");
        let after = shape_of(&home(reading("2026-09-16T10:00:01Z", true)), "radar");
        assert_eq!(before, after);
    }

    /// A rename does change it, or the new name would never appear.
    #[test]
    fn a_rename_does_change_the_shape_of_the_page() {
        let reading = reading("2026-09-16T10:00:00Z", false);
        let before = shape_of(&home(reading.clone()), "radar");
        let mut renamed = home(reading);
        renamed.devices[0].name = "Hallway radar".parse().expect("a valid name");
        assert_ne!(before, shape_of(&renamed, "radar"));
    }

    /// And so does putting it in a room, which is the other thing this page can do.
    #[test]
    fn moving_a_device_changes_the_shape_of_the_page() {
        let reading = reading("2026-09-16T10:00:00Z", false);
        let before = shape_of(&home(reading.clone()), "radar");
        let mut moved = home(reading);
        moved.devices[0].area_id = Some("hall".parse().expect("a valid area id"));
        assert_ne!(before, shape_of(&moved, "radar"));
    }
}
