//! One device: everything Irori knows about it, everything it provides, and the two things
//! that are yours to decide — what to call it and which area it's in.
//!
//! The list pages answer "what's going on"; this one answers "what is this thing" — which
//! protocol brought it in, what it calls itself, what firmware it's running, and which
//! entities belong to it.

use irori_types::{
    Area, AreaId, Availability, Capabilities, Device, Entity, EntityCategory, EntityId,
    EntityState, Name, SensorValue, State,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;

use crate::api::{self, DeviceEdit, Home};
use crate::chart;

/// The two picker choices that aren't an area. Neither can collide with an area id: one is empty
/// and the other has a space in it, and a slug can have neither.
const NOWHERE: &str = "";
const LET_THE_DEVICE_SAY: &str = "let the device say";
use crate::devices::{self, Controls};
use crate::settings::named;

/// What this page shows that isn't a live reading: the device itself, the areas it could be in,
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
    /// What it has that Irori has no entity kind for yet, as its protocol says.
    unmodeled: Vec<irori_types::Unmodeled>,
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
    let description_draft = RwSignal::new(String::new());
    let entity_draft = RwSignal::new(String::new());
    let editing = RwSignal::new(None::<EntityId>);

    // Once a device is ignored it has no page any more: go back to the list, which says where it went.
    let navigate = leptos_router::hooks::use_navigate();
    let leave = Callback::new(move |()| navigate("/devices", Default::default()));

    // This device's name is the one that travels back to its row, whichever way it was reached.
    let travelling = expect_context::<crate::transition::Travelling>().0;
    Effect::new(move |_| travelling.set(params.read().get("id")));

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
                leave,
                controls,
                trouble,
                Drafts {
                    editing: renaming,
                    name: draft,
                    description: description_draft,
                },
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
    let unmodeled = device
        .as_ref()
        .map(|device| {
            home.extensions
                .iter()
                .filter(|(id, _)| id.as_str() == device.protocol.as_str())
                .flat_map(|(_, extension)| &extension.unmodeled)
                .filter(|entry| entry.device_unique_id.as_ref() == Some(&device.unique_id))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    Shape {
        device,
        areas: home.areas.clone(),
        entities,
        unmodeled,
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

/// The device's name and description while they're being edited. Made once, above what redraws,
/// so live readings can't throw away what's being typed (ROADMAP D33).
#[derive(Debug, Clone, Copy)]
struct Drafts {
    editing: RwSignal<bool>,
    name: RwSignal<String>,
    description: RwSignal<String>,
}

#[allow(clippy::too_many_arguments)]
fn page(
    device: Device,
    shape: Shape,
    leave: Callback<()>,
    controls: Controls,
    trouble: RwSignal<Option<String>>,
    drafts: Drafts,
    editing: RwSignal<Option<EntityId>>,
    entity_draft: RwSignal<String>,
) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let areas = shape.areas.clone();
    let entities = shape.entities.clone();
    let also_has = also_has(&shape.unmodeled);
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

    let Drafts {
        editing: renaming,
        name: draft,
        description: description_draft,
    } = drafts;
    // One name and one description, saved together: there is no second name anywhere to fall
    // back to or keep in step (ROADMAP D36).
    let save = {
        let edit = edit.clone();
        move || {
            let Some(name) = named(draft.get(), trouble) else {
                return;
            };
            let typed = description_draft.get();
            let description = match typed.trim() {
                "" => None,
                text => match irori_types::Description::try_from(text) {
                    Ok(description) => Some(description),
                    Err(e) => {
                        trouble.set(Some(e.to_string()));
                        return;
                    }
                },
            };
            renaming.set(false);
            edit(DeviceEdit {
                name: Some(Some(name)),
                description: Some(description),
                ..DeviceEdit::default()
            });
        }
    };
    let move_to = {
        let edit = edit.clone();
        move |chosen: String| {
            // The three the picker offers. "Nowhere" is a decision, not the absence of one:
            // without it, a device whose firmware names an area would be put straight back.
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
                area: Some(area),
                ..DeviceEdit::default()
            });
        }
    };

    // Removing asks first, in a window of Irori's own rather than the browser's: it says what
    // goes and how to get the device back, which is the part worth reading before pressing it.
    let confirming = RwSignal::new(false);
    let removing = RwSignal::new(false);
    let remove = {
        let id = device.id.clone();
        move || {
            let id = id.clone();
            removing.set(true);
            spawn_local(async move {
                match api::remove_device(&id).await {
                    Ok(()) => {
                        crate::refresh(live);
                        leave.run(());
                    }
                    Err(why) => {
                        removing.set(false);
                        confirming.set(false);
                        trouble.set(Some(why));
                    }
                }
            });
        }
    };
    let what = device.name.to_string();
    let start = {
        let name = device.name.to_string();
        let description = device
            .description
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        move |_| {
            draft.set(name.clone());
            description_draft.set(description.clone());
            renaming.set(true);
        }
    };
    let in_room = device.area_id.clone();
    let suggested = device.suggested_area.clone();
    // The area to say as plain text, until "Edit" turns it into the picker.
    let room_name = in_room.as_ref().and_then(|id| {
        areas
            .iter()
            .find(|area| area.id.as_str() == id.as_str())
            .map(|area| area.name.to_string())
    });
    // What the device's own suggestion amounts to right now. Irori doesn't send *why* a device
    // is where it is, and doesn't need to: comparing the suggestion with the areas that exist
    // and the area it's in tells all three stories apart.
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
            "The device says it's in \"{suggests}\". Irori doesn't make areas on its own, but an \
             area called that would collect it."
        )),
        (Some(suggests), Some(room)) if in_room.as_ref() == Some(room) => Some(format!(
            "The device says it's in \"{suggests}\", and it is."
        )),
        // The area exists and the device isn't in it: somebody decided otherwise.
        (Some(suggests), Some(_)) => Some(format!(
            "The device says it's in \"{suggests}\", but it's been put elsewhere. Choose \
             \"Wherever the device says\" to let it decide again."
        )),
    };

    view! {
        <p class="crumb"><A href="/devices">"← All devices"</A></p>
        <div class="page-head">
            // The same name the list's link had, so the name travels up from the row it was.
            <h1 style=crate::transition::device_name(device.id.as_ref())>
                {device.name.to_string()}
            </h1>
            <div class="page-actions">
                // The two things a person decides about a device, kept as buttons rather than a
                // section: Edit opens the fields below; Remove takes it out of the home.
                <button type="button" on:click=start>"Edit"</button>
                <button type="button" class="danger-button" on:click=move |_| confirming.set(true)>
                    "Remove"
                </button>
            </div>
        </div>
        {move || confirming.get().then(|| {
            let remove = remove.clone();
            view! {
                <crate::modal::Modal
                    title=format!("Remove {what}?")
                    on_close=move || confirming.set(false)
                >
                    <div class="confirm">
                        <p>
                            "Irori deletes everything it keeps about it — its name, room, "
                            "entities, spot on the floorplan and history — and nothing can use it "
                            "any more."
                        </p>
                        <p class="muted small">
                            "The device itself isn't touched. It'll be listed under "
                            <strong>"+ Add device"</strong>
                            " if you want it back."
                        </p>
                        <div class="confirm-actions">
                            <button type="button" on:click=move |_| confirming.set(false)>
                                "Cancel"
                            </button>
                            <button
                                type="button"
                                class="danger-solid"
                                disabled=move || removing.get()
                                on:click=move |_| remove()
                            >
                                {move || if removing.get() { "Removing…" } else { "Remove" }}
                            </button>
                        </div>
                    </div>
                </crate::modal::Modal>
            }
        })}
        {device
            .description
            .as_ref()
            .map(|description| view! { <p class="description-lede">{description.to_string()}</p> })}
        <p class="lede">{subtitle}</p>

        {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}

        // One card for the device itself: what it is, where it is, and — while "Edit" is open —
        // the name, description and area that are a person's to decide.
        <section class="card">
            <h2>"Device information"</h2>
            {move || {
                renaming.get().then(|| {
                    let save = save.clone();
                    view! {
                        <form
                            class="about-form"
                            on:submit=move |ev| {
                                ev.prevent_default();
                                save();
                            }
                        >
                            <label>
                                <span>"Name"</span>
                                <input
                                    type="text"
                                    prop:value=draft
                                    on:input:target=move |ev| draft.set(ev.target().value())
                                />
                            </label>
                            <label>
                                <span>"Description"</span>
                                <textarea
                                    rows="2"
                                    placeholder="What it's for, or where exactly it is"
                                    prop:value=description_draft
                                    on:input:target=move |ev| description_draft.set(ev.target().value())
                                ></textarea>
                            </label>
                            <p class="muted small">
                                "This is the device's only name and description: everywhere Irori "
                                "shows it, and in the config files, it's this."
                            </p>
                            <div class="inline-form">
                                <button type="submit" class="add">"Save"</button>
                                <button type="button" on:click=move |_| renaming.set(false)>
                                    "Cancel"
                                </button>
                            </div>
                        </form>
                    }
                })
            }}
            <dl>
                // One id, the same one as in this page's address and in the config files. It's
                // made from the protocol and its permanent handle, so it never changes.
                <dt>"ID"</dt>
                <dd>{device.id.to_string()}</dd>
                <dt>"Through"</dt>
                <dd>{device.protocol.to_string()}</dd>
                <dt>"Area"</dt>
                // The area is a reading until "Edit" is open, when it becomes the picker: the
                // page says where a device is without offering to move it by mistake.
                <dd>
                    {{
                        let areas = areas.clone();
                        move || {
                            if renaming.get() {
                                let move_to = move_to.clone();
                                view! {
                                <label class="room-picker">
                                    <select
                                        on:change:target=move |ev| move_to(ev.target().value())
                                        prop:value=in_room
                                            .as_ref()
                                            .map(AreaId::to_string)
                                            .unwrap_or_else(|| NOWHERE.to_owned())
                                    >
                                        <option value=NOWHERE selected=in_room.is_none()>
                                            "Unassigned"
                                        </option>
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
                                            <option value=LET_THE_DEVICE_SAY>
                                                "Wherever the device says"
                                            </option>
                                        })}
                                    </select>
                                </label>
                            }
                            .into_any()
                        } else {
                            view! {
                                {room_name.clone().unwrap_or_else(|| "Unassigned".to_owned())}
                            }
                            .into_any()
                        }
                        }
                    }}
                </dd>
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
            {if areas.is_empty() {
                view! {
                    <p class="muted small">
                        "No areas yet. " <A href="/settings">"Make one"</A>
                        " and this device can go in it."
                    </p>
                }
                .into_any()
            } else {
                ().into_any()
            }}
            {suggestion.map(|note| view! { <p class="muted small">{note}</p> })}
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
                        "Nothing Irori can model yet."
                    </p>
                }
                .into_any()
            } else {
                // What the device is for, then its settings and diagnostics under their own
                // headings, as the protocol marked them.
                let row = move |entity: Entity| {
                    view! {
                        <EntityRow
                            entity=entity
                            controls=controls
                            trouble=trouble
                            editing=editing
                            draft=entity_draft
                        />
                    }
                };
                let (main, rest): (Vec<_>, Vec<_>) =
                    entities.into_iter().partition(|e| e.entity_category.is_none());
                let (settings, diagnostics): (Vec<_>, Vec<_>) = rest
                    .into_iter()
                    .partition(|e| e.entity_category == Some(EntityCategory::Config));
                let section = move |title: &'static str, group: Vec<Entity>| {
                    (!group.is_empty()).then(|| {
                        view! {
                            <h3 class="entity-group">{title}</h3>
                            {group.into_iter().map(row).collect_view()}
                        }
                    })
                };
                view! {
                    {main.into_iter().map(row).collect_view()}
                    {section("Settings", settings)}
                    {section("Diagnostics", diagnostics)}
                }
                .into_any()
            }}
            {also_has.map(|text| view! { <p class="muted small">{text}</p> })}
        </section>
    }
}

/// What a device has that Irori has no entity kind for yet, in a sentence: "Also has a ceiling
/// fan (fan) and 2 settings (number), which Irori doesn't support yet." Then, one by one, those
/// of a kind it has that it couldn't use, and why. `None` when there's nothing.
fn also_has(unmodeled: &[irori_types::Unmodeled]) -> Option<String> {
    if unmodeled.is_empty() {
        return None;
    }
    let mut by_platform: std::collections::BTreeMap<&str, Vec<&str>> = Default::default();
    let mut refused = Vec::new();
    for entry in unmodeled {
        let platform = entry.platform.as_str();
        let name = entry.name.as_ref().map(irori_types::Name::as_str);
        match &entry.reason {
            Some(why) => refused.push(match name {
                Some(name) => format!("{name} ({}): {why}", platform.replace('_', " ")),
                None => format!("a {}: {why}", platform.replace('_', " ")),
            }),
            None => by_platform.entry(platform).or_default().extend(name),
        }
    }
    let items: Vec<String> = by_platform
        .into_iter()
        .map(|(platform, names)| match names.as_slice() {
            [] => platform.replace('_', " "),
            names => format!("{} ({})", names.join(", "), platform.replace('_', " ")),
        })
        .collect();
    let mut said = Vec::new();
    if !items.is_empty() {
        said.push(format!(
            "Also has {}, which Irori doesn't support yet.",
            items.join("; ")
        ));
    }
    if !refused.is_empty() {
        said.push(format!("Couldn't use {}.", refused.join("; ")));
    }
    Some(said.join(" "))
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
    let row = entity.clone();

    // The rolled-up "last 24 hours", opened from the row's own reading. Fetched once, the
    // first time it's opened, and kept: reopening shows the same day it loaded, which is honest
    // about what was on file then.
    let open = RwSignal::new(false);
    let history = RwSignal::new(None::<Result<Vec<EntityState>, String>>);
    let fetching = RwSignal::new(false);
    let toggle = {
        let id = id.clone();
        move |()| {
            let id = id.clone();
            if !open.get_untracked()
                && !fetching.get_untracked()
                && history.get_untracked().is_none()
            {
                fetching.set(true);
                spawn_local(async move {
                    let result = api::entity_history(&id).await;
                    fetching.set(false);
                    history.set(Some(result));
                });
            }
            open.update(|open| *open = !*open);
        }
    };
    let unroll = devices::Unroll {
        open,
        toggle: Callback::new(toggle),
    };

    view! {
        <div class="entity-row">
            {move || devices::row(row.clone(), state.get(), controls, Some(unroll))}
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
            </div>
            // Always there, rolled up or down, so it can roll both ways; `inert` while rolled
            // up, so nobody tabs into what they can't see.
            <div class="drawer" class:open=move || open.get() inert=move || (!open.get()).then_some("")>
                <div class="drawer-inner">{history_panel(entity, history, state)}</div>
            </div>
        </div>
    }
}

/// The unrolled "last 24 hours": the day of changes the server has recorded for this entity.
/// A number's day is drawn as a chart, with the table a click away; anything else — on and off,
/// words — is the table, newest first, in its own scroll so a sensor that changed a hundred
/// times doesn't stretch the page. "As much as available" is what it says: the server keeps
/// what happened while it's been running, and the long view is the recorder's job (M1.3).
fn history_panel(
    entity: Entity,
    history: RwSignal<Option<Result<Vec<EntityState>, String>>>,
    // The row's reading as it is now, so a chart can grow with it.
    live: Memo<Option<EntityState>>,
) -> AnyView {
    let numeric = entity.capabilities.primary_shape() == Some(irori_types::ValueShape::Number);
    view! {
        <div class="history">
            {move || match history.get() {
                // A number's chart will need the room: keep it, so the drawer doesn't jump
                // when the day arrives.
                None if numeric => view! {
                    <div class="chart-waiting" aria-label="Looking for the last 24 hours…"></div>
                }
                .into_any(),
                None => view! {
                    <p class="muted small history-note">"Looking for the last 24 hours…"</p>
                }
                .into_any(),
                Some(Err(why)) => view! {
                    <p class="why">{why}</p>
                    <p class="muted small history-note">"Nothing to show until it can be asked again."</p>
                }
                .into_any(),
                Some(Ok(states)) if states.is_empty() => view! {
                    <p class="muted small history-note">
                        "No changes in the last 24 hours. Irori records a change each time one "
                        "happens, and keeps the last day while it's running."
                    </p>
                }
                .into_any(),
                Some(Ok(states)) => match readings(&states) {
                    Some(numbers) => charted(&entity, numbers, states, live),
                    None => table(&entity, states),
                },
            }}
        </div>
    }
    .into_any()
}

/// A day of readings as numbers, if every reading is a number or a gap. A gap is the sensor
/// not saying anything — unreachable, or registered but not yet reporting — which the chart
/// shows as a break in the line rather than a value it didn't have. Gaps before the first number
/// are dropped: the day starts when the readings do. Any words in there, and it's the table.
fn readings(states: &[EntityState]) -> Option<Vec<chart::Reading>> {
    let mut numbers = Vec::new();
    for state in states {
        let reading = as_reading(state)?;
        if reading.value.is_nan() && numbers.is_empty() {
            continue;
        }
        numbers.push(reading);
    }
    (!numbers.is_empty()).then_some(numbers)
}

/// One state as a point on the chart: its number, or a gap. Nothing if it's words or not a
/// sensor at all.
fn as_reading(state: &EntityState) -> Option<chart::Reading> {
    let value = match (state.availability, state.state.as_ref()) {
        (Availability::Unavailable, _) | (_, None) => f64::NAN,
        // Whatever its kind, a number is a point on the chart.
        (_, Some(state)) => match state.primary() {
            irori_types::Typed::Number(value) => value,
            _ => return None,
        },
    };
    Some(chart::Reading {
        at_ms: state.last_changed.as_jiff().as_millisecond() as f64,
        at: clock_time(state.last_changed),
        value,
    })
}

/// The chart, with the same day as a table behind a switch: the table is where every number is
/// readable without pointing at it.
fn charted(
    entity: &Entity,
    numbers: Vec<chart::Reading>,
    states: Vec<EntityState>,
    live: Memo<Option<EntityState>>,
) -> AnyView {
    let as_table = RwSignal::new(false);
    let unit = devices::unit_of(&entity.capabilities);
    let name = entity.name.to_string();
    let chart = view! {
        <chart::StepChart
            readings=numbers
            live=Signal::derive(move || live.get().as_ref().and_then(as_reading))
            unit=unit
            name=name
        />
    }
    .into_any();
    let table = table(entity, states);
    view! {
        <div class="history-head">
            <div class="switcher" role="group" aria-label="Show as">
                <button
                    type="button"
                    class:chosen=move || !as_table.get()
                    aria-pressed=move || (!as_table.get()).to_string()
                    on:click=move |_| as_table.set(false)
                >
                    "Chart"
                </button>
                <button
                    type="button"
                    class:chosen=move || as_table.get()
                    aria-pressed=move || as_table.get().to_string()
                    on:click=move |_| as_table.set(true)
                >
                    "Table"
                </button>
            </div>
        </div>
        // Both drawn once and kept, so switching is instant and loses nothing. Coming back to
        // the chart draws its line in again — the same day, arriving again.
        <div hidden=move || as_table.get()>{chart}</div>
        <div hidden=move || !as_table.get()>{table}</div>
    }
    .into_any()
}

fn table(entity: &Entity, states: Vec<EntityState>) -> AnyView {
    view! {
        <div class="history-scroll">
            <table class="history">
                <thead>
                    <tr>
                        <th scope="col">"Time"</th>
                        <th scope="col">"Reading"</th>
                    </tr>
                </thead>
                <tbody>
                    {states
                        .into_iter()
                        .rev()
                        .map(|state| {
                            let at = state.last_changed;
                            view! {
                                <tr>
                                    <th scope="row">
                                        <time title=at.to_string()>{clock_time(at)}</time>
                                    </th>
                                    <td>{reading_of(entity, &state)}</td>
                                </tr>
                            }
                        })
                        .collect_view()}
                </tbody>
            </table>
        </div>
    }
    .into_any()
}

/// What a change meant, in the same words as its row uses (`devices.rs`). The table's cells are
/// plain text, so the reading is a string rather than the row's spans.
fn reading_of(entity: &Entity, state: &EntityState) -> String {
    let value = state.state.as_ref();
    match (&entity.capabilities, value) {
        (Capabilities::Sensor(capabilities), Some(State::Sensor(sensor))) => match &sensor.value {
            SensorValue::Number(n) => {
                let unit = capabilities
                    .unit
                    .as_ref()
                    .map(|u| format!(" {u}"))
                    .unwrap_or_default();
                format!("{}{}", devices::number(*n), unit)
            }
            SensorValue::Text(text) => text.clone(),
        },
        (Capabilities::BinarySensor(capabilities), Some(State::BinarySensor(sensor))) => {
            devices::wording(capabilities.device_class, sensor.on).to_owned()
        }
        (Capabilities::Light(_), Some(State::Light(light))) => {
            let on = if light.on { "On" } else { "Off" };
            light
                .brightness
                .map(|level| format!("{on} · {}%", (u16::from(level) * 100).div_ceil(255)))
                .unwrap_or_else(|| on.to_owned())
        }
        (Capabilities::Switch(_), Some(State::Switch(switch))) => {
            if switch.on { "On" } else { "Off" }.to_owned()
        }
        (Capabilities::Button(_), _) => "—".to_owned(),
        (Capabilities::Event(_), Some(State::Event(event))) => event.event_type.clone(),
        (Capabilities::Cover(_), Some(State::Cover(cover))) => devices::cover_words(cover),
        (Capabilities::Lock(_), Some(State::Lock(lock))) => {
            devices::lock_words(lock.state).to_owned()
        }
        (Capabilities::Fan(_), Some(State::Fan(fan))) => devices::fan_words(fan),
        (Capabilities::Siren(_), Some(State::Siren(siren))) => {
            if siren.on { "Sounding" } else { "Quiet" }.to_owned()
        }
        (Capabilities::Valve(_), Some(State::Valve(valve))) => {
            devices::cover_words(&irori_types::CoverState {
                state: valve.state,
                position: valve.position,
                tilt: None,
            })
        }
        (Capabilities::Select(_), Some(State::Select(select))) => select.option.clone(),
        (Capabilities::Text(text), Some(State::Text(state))) => {
            if text.mode == irori_types::TextMode::Password {
                "Hidden".to_owned()
            } else {
                state.value.clone()
            }
        }
        (Capabilities::Number(capabilities), Some(State::Number(number))) => {
            let unit = capabilities
                .unit
                .as_ref()
                .map(|u| format!(" {u}"))
                .unwrap_or_default();
            format!("{}{unit}", devices::number(number.value))
        }
        _ => "unknown".to_owned(),
    }
}

/// The clock part of a timestamp, for the table's Time column. Server timestamps serialize as
/// fixed RFC 3339 UTC (`2026-09-16T10:00:01.123Z`), so the time is the characters 11..19; the
/// full string sits in the `<time>`'s title, and anything unexpected shows whole rather than
/// guessed at.
fn clock_time(at: irori_types::Timestamp) -> String {
    let text = at.to_string();
    text.get(11..19).map(str::to_owned).unwrap_or(text)
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
        EntityState, Origin, SensorCapabilities, SensorState, SensorValue, SensorValueType, State,
        Timestamp,
    };

    use super::*;

    #[test]
    fn what_a_device_has_that_irori_cant_model_is_said_once() {
        let entry = |platform: &str, name: Option<&str>| irori_types::Unmodeled {
            device_unique_id: None,
            platform: platform.parse().expect("slug"),
            name: name.map(|n| n.parse().expect("name")),
            reason: None,
        };
        assert_eq!(also_has(&[]), None);
        let refused = irori_types::Unmodeled {
            reason: Some("its command_template needs Jinja".into()),
            ..entry("number", Some("Level"))
        };
        assert_eq!(
            also_has(&[refused, entry("fan", None)]).as_deref(),
            Some(
                "Also has fan, which Irori doesn't support yet. \
                 Couldn't use Level (number): its command_template needs Jinja."
            )
        );
        assert_eq!(
            also_has(&[
                entry("number", Some("Timeout")),
                entry("fan", Some("Ceiling fan")),
                entry("number", Some("Sensitivity")),
                entry("water_heater", None),
            ])
            .as_deref(),
            Some(
                "Also has Ceiling fan (fan); Timeout, Sensitivity (number); water heater, \
                 which Irori doesn't support yet."
            )
        );
    }

    fn device() -> Device {
        Device {
            id: "radar".parse().expect("a valid device id"),
            protocol: "esphome".parse().expect("a valid protocol id"),
            unique_id: "00:11:22:33:44:55".parse().expect("a valid unique id"),
            name: "Radar".parse().expect("a valid name"),
            description: None,
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
            protocol: "esphome".parse().expect("a valid protocol id"),
            unique_id: "00:11:22:33:44:55-moving"
                .parse()
                .expect("a valid unique id"),
            name: "Moving".parse().expect("a valid name"),
            device_id: Some("radar".parse().expect("a valid device id")),
            area_id: None,
            capabilities: Capabilities::BinarySensor(BinarySensorCapabilities {
                device_class: None,
            }),
            entity_category: None,
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

    /// And so does putting it in an area, which is the other thing this page can do.
    #[test]
    fn moving_a_device_changes_the_shape_of_the_page() {
        let reading = reading("2026-09-16T10:00:00Z", false);
        let before = shape_of(&home(reading.clone()), "radar");
        let mut moved = home(reading);
        moved.devices[0].area_id = Some("hall".parse().expect("a valid area id"));
        assert_ne!(before, shape_of(&moved, "radar"));
    }

    /// The table's Time column shows the clock part of the server's UTC timestamps, not the
    /// date-long string that would crowd the column.
    #[test]
    fn clock_time_shows_the_clock_part() {
        let at: Timestamp = "2026-09-16T10:00:01.123Z"
            .parse()
            .expect("a valid timestamp");
        assert_eq!(clock_time(at), "10:00:01");
    }

    /// The table's Reading column uses the same words as the row above it: a sensor keeps its
    /// unit, and a binary sensor says what its `true` means.
    #[test]
    fn history_readings_use_the_rows_words() {
        let sensor = Entity {
            id: "sensor.water_temp".parse().expect("a valid entity id"),
            protocol: "radar".parse().expect("a valid protocol id"),
            unique_id: "00:11:22:33:44:55-temp".parse().expect("a valid unique id"),
            name: "Water temperature".parse().expect("a valid name"),
            device_id: Some("radar".parse().expect("a valid device id")),
            area_id: None,
            capabilities: Capabilities::Sensor(SensorCapabilities {
                value_type: SensorValueType::Number,
                device_class: None,
                unit: Some("°C".to_owned()),
                state_class: None,
                options: Vec::new(),
            }),
            entity_category: None,
        };
        let mut state = reading("2026-09-16T10:00:00Z", true);
        state.state = Some(State::Sensor(SensorState {
            value: SensorValue::Number(22.5),
        }));
        assert_eq!(reading_of(&sensor, &state), "22.5 °C");
        assert_eq!(
            reading_of(&entity(), &reading("2026-09-16T10:00:00Z", true)),
            "On"
        );
    }
}
