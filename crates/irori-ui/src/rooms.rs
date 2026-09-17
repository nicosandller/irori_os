//! The Rooms page: making the rooms of a home, and seeing what's in each one.
//!
//! A room is the first thing in Irori that a person creates rather than a device announcing
//! itself. It lives in `config/areas.toml`, which is a plain file anyone can open — so this page
//! is a convenience, not the only way in (`docs/specs/config.md`).

use irori_types::{Area, AreaId, Device, Name};
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;

use crate::api;

#[component]
pub fn Rooms() -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let trouble = RwSignal::new(None::<String>);
    let adding = RwSignal::new(String::new());
    // Which room's name is being edited, if any: only one at a time.
    let editing = RwSignal::new(None::<AreaId>);
    let draft = RwSignal::new(String::new());

    // Only the rooms and the devices in them, not what those devices are reporting. A page that
    // redrew every time a sensor spoke would throw away a half-typed room name with it.
    let shape = Memo::new(move |_| {
        let home = live.home.get();
        (
            home.areas.clone(),
            home.devices.clone(),
            home.floors.clone(),
        )
    });
    let floor_name = RwSignal::new(String::new());
    let floor_level = RwSignal::new("0".to_owned());

    let add = move || {
        let Some(name) = named(adding.get(), trouble) else {
            return;
        };
        adding.set(String::new());
        spawn_local(async move {
            match api::add_area(name).await {
                Ok(()) => {
                    trouble.set(None);
                    crate::refresh(live);
                }
                Err(why) => trouble.set(Some(why)),
            }
        });
    };

    view! {
        <div class="page-head">
            <h1>"Rooms"</h1>
        </div>
        <p class="lede">
            "Rooms are yours to name. Irori never invents one, even when a device says which room "
            "it thinks it's in — but a device that asks for a room you've made goes straight into "
            "it."
        </p>

        {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}

        <section class="card">
            <h2>"Add a room"</h2>
            <form
                class="inline-form"
                on:submit=move |ev| {
                    ev.prevent_default();
                    add();
                }
            >
                <input
                    type="text"
                    aria-label="Name of the new room"
                    placeholder="Kitchen"
                    prop:value=adding
                    on:input:target=move |ev| adding.set(ev.target().value())
                />
                <button type="submit" class="add" disabled=move || adding.get().trim().is_empty()>
                    "Add"
                </button>
            </form>
        </section>

        <Floors name=floor_name level=floor_level trouble=trouble />

        {move || {
            let (areas, devices, floors) = shape.get();
            if areas.is_empty() {
                return view! {
                    <p class="empty">
                        "No rooms yet. Add one above, and devices can go in it."
                    </p>
                }
                .into_any();
            }
            if floors.is_empty() {
                return areas
                    .iter()
                    .map(|area| room(area.clone(), &devices, &floors, editing, draft, trouble, live))
                    .collect_view()
                    .into_any();
            }
            // By floor, lowest first, then the rooms that aren't on one.
            let mut sections: Vec<(String, Vec<Area>)> = floors
                .iter()
                .map(|floor| {
                    let on_it = areas
                        .iter()
                        .filter(|area| area.floor_id.as_ref() == Some(&floor.id))
                        .cloned()
                        .collect();
                    (floor.name.to_string(), on_it)
                })
                .collect();
            let unfloored: Vec<Area> = areas
                .iter()
                .filter(|area| {
                    area.floor_id
                        .as_ref()
                        .is_none_or(|id| floors.iter().all(|floor| &floor.id != id))
                })
                .cloned()
                .collect();
            if !unfloored.is_empty() {
                sections.push(("On no floor".to_owned(), unfloored));
            }
            sections
                .into_iter()
                .map(|(heading, rooms)| {
                    let rooms = if rooms.is_empty() {
                        view! { <p class="muted small">"No rooms on this floor yet."</p> }.into_any()
                    } else {
                        rooms
                            .into_iter()
                            .map(|area| room(area, &devices, &floors, editing, draft, trouble, live))
                            .collect_view()
                            .into_any()
                    };
                    view! {
                        <h2 class="floor-heading">{heading}</h2>
                        {rooms}
                    }
                })
                .collect_view()
                .into_any()
        }}

        {move || {
            let (_, devices, _) = shape.get();
            let homeless: Vec<Device> = devices
                .iter()
                .filter(|device| device.area_id.is_none())
                .cloned()
                .collect();
            (!homeless.is_empty())
                .then(|| {
                    view! {
                        <section class="card room">
                            <div class="room-head">
                                <h2>"Not in a room"</h2>
                                <span class="muted">{count(homeless.len())}</span>
                            </div>
                            <ul class="room-devices">
                                {homeless.into_iter().map(in_room).collect_view()}
                            </ul>
                        </section>
                    }
                })
        }}
    }
}

/// One room: its name, what's in it, and the two things you can do to it.
#[allow(clippy::too_many_arguments)]
fn room(
    area: Area,
    all: &[Device],
    floors: &[irori_types::Floor],
    editing: RwSignal<Option<AreaId>>,
    draft: RwSignal<String>,
    trouble: RwSignal<Option<String>>,
    live: crate::Live,
) -> AnyView {
    let devices: Vec<Device> = all
        .iter()
        .filter(|device| device.area_id.as_ref() == Some(&area.id))
        .cloned()
        .collect();
    let id = area.id.clone();
    let being_edited = {
        let id = id.clone();
        move || editing.get().as_ref() == Some(&id)
    };

    let picker = floor_picker(&area, floors, trouble, live);
    let rename = {
        let id = id.clone();
        move || {
            let Some(name) = named(draft.get(), trouble) else {
                return;
            };
            let id = id.clone();
            editing.set(None);
            spawn_local(async move {
                match api::rename_area(&id, name).await {
                    Ok(()) => {
                        trouble.set(None);
                        crate::refresh(live);
                    }
                    Err(why) => trouble.set(Some(why)),
                }
            });
        }
    };

    let remove = {
        let id = id.clone();
        let what = area.name.to_string();
        let count = devices.len();
        move || {
            // Nothing is lost by removing a room — the devices stay, and what was said about
            // them is kept — so this asks only when it would visibly move things.
            if count > 0
                && !window()
                    .confirm_with_message(&format!(
                        "Remove {what}? The {} in it will have no room until you put them \
                         somewhere, and nothing else changes.",
                        count_of(count),
                    ))
                    .unwrap_or(false)
            {
                return;
            }
            let id = id.clone();
            spawn_local(async move {
                match api::remove_area(&id).await {
                    Ok(()) => {
                        trouble.set(None);
                        crate::refresh(live);
                    }
                    Err(why) => trouble.set(Some(why)),
                }
            });
        }
    };

    let start = {
        let id = id.clone();
        let name = area.name.to_string();
        move |_| {
            draft.set(name.clone());
            editing.set(Some(id.clone()));
        }
    };

    view! {
        <section class="card room">
            <div class="room-head">
                {move || {
                    if being_edited() {
                        let rename = rename.clone();
                        view! {
                            <form
                                class="inline-form"
                                on:submit=move |ev| {
                                    ev.prevent_default();
                                    rename();
                                }
                            >
                                <input
                                    type="text"
                                    aria-label="Name of this room"
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
                        view! { <h2>{area.name.to_string()}</h2> }.into_any()
                    }
                }}
                <span class="muted">{count(devices.len())}</span>
                <span class="room-actions">
                    {picker}
                    <button type="button" on:click=start>"Rename"</button>
                    <button
                        type="button"
                        class="danger"
                        on:click=move |_| {
                            let remove = remove.clone();
                            remove();
                        }
                    >
                        "Remove"
                    </button>
                </span>
            </div>
            {if devices.is_empty() {
                view! {
                    <p class="muted">
                        "Nothing in here yet. A device's own page is where you put it in a room."
                    </p>
                }
                .into_any()
            } else {
                view! {
                    <ul class="room-devices">
                        {devices.into_iter().map(in_room).collect_view()}
                    </ul>
                }
                .into_any()
            }}
        </section>
    }
    .into_any()
}

/// Which floor a room is on. Only offered once a floor exists.
fn floor_picker(
    area: &Area,
    floors: &[irori_types::Floor],
    trouble: RwSignal<Option<String>>,
    live: crate::Live,
) -> AnyView {
    if floors.is_empty() {
        return ().into_any();
    }
    let id = area.id.clone();
    let current = area.floor_id.clone();
    let chosen = move |value: String| {
        let id = id.clone();
        let floor = (!value.is_empty())
            .then(|| irori_types::FloorId::try_from(value.as_str()).ok())
            .flatten();
        spawn_local(async move {
            match api::set_area_floor(&id, floor.as_ref()).await {
                Ok(()) => crate::refresh(live),
                Err(why) => trouble.set(Some(why)),
            }
        });
    };
    view! {
        <select
            class="floor-picker"
            aria-label="Floor"
            on:change:target=move |ev| chosen(ev.target().value())
        >
            <option value="" selected=current.is_none()>"No floor"</option>
            {floors
                .iter()
                .map(|floor| {
                    let value = floor.id.to_string();
                    let selected = current.as_ref() == Some(&floor.id);
                    view! { <option value=value selected=selected>{floor.name.to_string()}</option> }
                })
                .collect_view()}
        </select>
    }
    .into_any()
}

/// Making and removing floors. Folded away until someone wants them: most flats have one.
#[component]
fn Floors(
    name: RwSignal<String>,
    level: RwSignal<String>,
    trouble: RwSignal<Option<String>>,
) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let floors = Memo::new(move |_| live.home.get().floors);
    let add = move || {
        let Some(named) = named(name.get(), trouble) else {
            return;
        };
        let Ok(at) = level.get().trim().parse::<i8>() else {
            trouble.set(Some(
                "A floor's level is a whole number: 0 for the entrance, 1 above it, -1 below."
                    .to_owned(),
            ));
            return;
        };
        name.set(String::new());
        spawn_local(async move {
            match api::add_floor(named, at).await {
                Ok(()) => {
                    trouble.set(None);
                    crate::refresh(live);
                }
                Err(why) => trouble.set(Some(why)),
            }
        });
    };
    view! {
        <details class="card floors">
            <summary>
                {move || match floors.get().len() {
                    0 => "Floors".to_owned(),
                    1 => "Floors (1)".to_owned(),
                    n => format!("Floors ({n})"),
                }}
            </summary>
            <p class="muted small">
                "Floors group rooms, lowest first. The level is a whole number: 0 for the entrance "
                "floor, 1 above it, -1 for a cellar."
            </p>
            <ul class="room-devices">
                {move || {
                    floors
                        .get()
                        .into_iter()
                        .map(|floor| {
                            let id = floor.id.clone();
                            let remove = move |_| {
                                let id = id.clone();
                                spawn_local(async move {
                                    match api::remove_floor(&id).await {
                                        Ok(()) => crate::refresh(live),
                                        Err(why) => trouble.set(Some(why)),
                                    }
                                });
                            };
                            view! {
                                <li>
                                    <span class="name">{floor.name.to_string()}</span>
                                    <span class="muted small">{format!("level {}", floor.level)}</span>
                                    <button type="button" class="link" on:click=remove>"Remove"</button>
                                </li>
                            }
                        })
                        .collect_view()
                }}
            </ul>
            <form
                class="inline-form"
                on:submit=move |ev| {
                    ev.prevent_default();
                    add();
                }
            >
                <input
                    type="text"
                    aria-label="Name of the new floor"
                    placeholder="Upstairs"
                    prop:value=name
                    on:input:target=move |ev| name.set(ev.target().value())
                />
                <input
                    type="number"
                    class="level"
                    aria-label="Level"
                    min="-128"
                    max="127"
                    prop:value=level
                    on:input:target=move |ev| level.set(ev.target().value())
                />
                <button type="submit" class="add" disabled=move || name.get().trim().is_empty()>
                    "Add floor"
                </button>
            </form>
        </details>
    }
}

fn in_room(device: Device) -> AnyView {
    let id = device.id.to_string();
    let name = device.name.to_string();
    let through = device.integration.to_string();
    view! {
        <li>
            <A href=format!("/devices/{id}")>{name}</A>
            <span class="muted small">{through}</span>
        </li>
    }
    .into_any()
}

fn count(devices: usize) -> String {
    match devices {
        0 => "no devices".to_owned(),
        n => count_of(n),
    }
}

fn count_of(devices: usize) -> String {
    format!("{devices} device{}", if devices == 1 { "" } else { "s" })
}

/// Reads what was typed as a name, saying what's wrong with it rather than failing silently.
///
/// The same rule the core applies, applied here so the answer is immediate: a name is 1–100
/// characters with nothing hanging off either end.
pub fn named(typed: String, trouble: RwSignal<Option<String>>) -> Option<Name> {
    match Name::try_from(typed.trim()) {
        Ok(name) => Some(name),
        Err(_) if typed.trim().is_empty() => {
            trouble.set(Some("A name can't be empty.".to_owned()));
            None
        }
        Err(e) => {
            trouble.set(Some(e.to_string()));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_counts_read_as_a_person_would_say_them() {
        assert_eq!(count(0), "no devices");
        assert_eq!(count(1), "1 device");
        assert_eq!(count(4), "4 devices");
    }
}
