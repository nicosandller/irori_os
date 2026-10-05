//! Floors and areas: the levels of the home, the places on them, and which devices are where.
//!
//! One shape for all of it. A floor is a row that folds; its areas are rows inside it; an
//! area's devices are chips inside that. Everything is made and renamed with the same small
//! composer, which opens where the thing will appear, and everything is rearranged by dragging:
//! a device onto an area, an area onto a floor.
//!
//! Irori never invents an area, even when a device says where it thinks it is.

use std::collections::BTreeSet;

use irori_types::{Area, AreaId, Device, DeviceId, Floor, FloorId, Name};
use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use web_sys::wasm_bindgen::JsCast;

use crate::api;
use crate::fold::{Head, fold};
use crate::icons::{Icon, icon};

/// How long something dragged has to linger over a folded floor before it opens.
const LINGER: std::time::Duration = std::time::Duration::from_millis(600);

/// Past this many chips and rows, a drop just lands instead of every one being pictured so
/// the dropped one can travel.
const THINGS_THAT_TRAVEL: usize = 80;

/// Reads what was typed as a name, in the words to show when it isn't one. The same rule the
/// core applies, applied here so the answer is immediate.
pub fn name_of(typed: &str) -> Result<Name, String> {
    match Name::try_from(typed.trim()) {
        Ok(name) => Ok(name),
        Err(_) if typed.trim().is_empty() => Err("A name can't be empty.".to_owned()),
        Err(e) => Err(e.to_string()),
    }
}

/// A floor as typed: its name, and its level as text. Wherever a floor is made, this is what
/// says no, in the same words.
pub fn floor_draft(name: &str, level: &str) -> Result<(Name, i8), String> {
    let name = name_of(name).map_err(|why| {
        if name.trim().is_empty() {
            "A floor needs a name.".to_owned()
        } else {
            why
        }
    })?;
    let level = level.trim().parse::<i8>().map_err(|_| {
        "A floor's level is a whole number: 0 for the entrance, 1 above it, -1 below.".to_owned()
    })?;
    Ok((name, level))
}

/// The level a new floor starts at: above the highest, because that's the one people add.
pub fn next_level(floors: &[Floor]) -> i8 {
    floors
        .iter()
        .map(|floor| floor.level)
        .max()
        .map_or(0, |highest| highest.saturating_add(1))
}

/// How the home is arranged, in a few words, for the heading of the row in Settings.
pub fn summary(floors: usize, areas: usize, unassigned: usize) -> String {
    let counted = |n: usize, one: &str, many: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    };
    let mut said = vec![
        counted(floors, "floor", "floors"),
        counted(areas, "area", "areas"),
    ];
    if unassigned > 0 {
        said.push(format!("{unassigned} unassigned"));
    }
    said.join(" · ")
}

/// The one thing being typed, if anything is: there's one composer open at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Composing {
    NewFloor,
    /// A new area, on this floor.
    NewArea(FloorId),
    Floor(FloorId),
    Area(AreaId),
}

/// What's being dragged.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Dragged {
    Device(DeviceId),
    Area(AreaId),
}

/// Where the dragged thing would land if let go now. `None` inside is "nowhere": a device in no
/// area, an area on no floor.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Area(Option<AreaId>),
    Floor(Option<FloorId>),
}

/// Everything the section's rows share. All of it is made once, above what's drawn again when
/// the home changes, so a name half typed and a floor folded away both come through that.
#[derive(Clone, Copy)]
struct Places {
    live: crate::Live,
    /// Why the last change was refused, said at the top of the section.
    trouble: RwSignal<Option<String>>,
    composing: RwSignal<Option<Composing>>,
    name: RwSignal<String>,
    level: RwSignal<i8>,
    /// What's wrong with what's typed, said under the field.
    mistake: RwSignal<Option<String>>,
    dragging: RwSignal<Option<Dragged>>,
    over: RwSignal<Option<Target>>,
    /// The folded floor something dragged is lingering over.
    lingering: RwSignal<Option<FloorId>>,
    folded: RwSignal<BTreeSet<FloorId>>,
    /// The floor or area whose Remove has been pressed once, and is asking to be pressed again.
    removing: RwSignal<Option<String>>,
}

impl Places {
    /// Opens the composer on `what`, starting from `name` and `level`.
    fn compose(self, what: Composing, name: &str, level: i8) {
        self.name.set(name.to_owned());
        self.level.set(level);
        self.mistake.set(None);
        self.composing.set(Some(what));
    }

    /// Sends a change, then shows the home as it now is, or says why it was refused.
    fn send(self, request: impl Future<Output = Result<(), String>> + 'static) {
        let Places { live, trouble, .. } = self;
        spawn_local(async move {
            match request.await {
                Ok(()) => trouble.set(None),
                Err(why) => trouble.set(Some(why)),
            }
            // Either way: a refused drop was already drawn where it was dropped.
            crate::refresh(live);
        });
    }

    /// What Enter does in the open composer.
    fn save(self) {
        let Some(what) = self.composing.get_untracked() else {
            return;
        };
        let name = match name_of(&self.name.get_untracked()) {
            Ok(name) => name,
            Err(why) => {
                self.mistake.set(Some(why));
                return;
            }
        };
        self.mistake.set(None);
        let level = self.level.get_untracked();
        match what {
            // Making one leaves the composer open for the next: floors and areas come in runs.
            Composing::NewFloor => {
                self.name.set(String::new());
                self.level.set(level.saturating_add(1));
                self.send(async move { api::add_floor(name, level).await });
            }
            Composing::NewArea(floor) => {
                self.name.set(String::new());
                self.send(async move { api::add_area(name, Some(&floor)).await });
            }
            Composing::Floor(id) => {
                self.composing.set(None);
                self.send(async move { api::edit_floor(&id, Some(name), Some(level)).await });
            }
            Composing::Area(id) => {
                self.composing.set(None);
                self.send(async move { api::rename_area(&id, name).await });
            }
        }
    }

    /// Lets go of what's being dragged, on `target`.
    fn drop_on(self, target: Target) {
        let dragged = self.dragging.get_untracked();
        self.dragging.set(None);
        self.over.set(None);
        self.lingering.set(None);
        let live = self.live;
        match (dragged, target) {
            (Some(Dragged::Device(device)), Target::Area(area)) => {
                let here = live.home.with_untracked(|home| {
                    home.devices
                        .iter()
                        .find(|known| known.id == device)
                        .map(|known| known.area_id.clone())
                });
                // Dropped where it already is, or gone while it was in the air.
                if here.is_none_or(|here| here == area) {
                    return;
                }
                // Drawn where it was dropped straight away, as one movement, and then asked
                // for. The same PATCH the device's own page uses, with `Nowhere` for "out" —
                // deliberately no area, so the protocol's suggestion can't put it straight back.
                let to = area.clone();
                let moved = device.clone();
                crate::transition::around("place", move || {
                    live.home.update(|home| {
                        if let Some(known) = home.devices.iter_mut().find(|d| d.id == moved) {
                            known.area_id = to;
                        }
                    });
                });
                let edit = api::DeviceEdit {
                    area: Some(Some(
                        area.map_or_else(api::WhereTo::nowhere, api::WhereTo::In),
                    )),
                    ..api::DeviceEdit::default()
                };
                self.send(async move { api::edit_device(&device, &edit).await });
            }
            (Some(Dragged::Area(area)), Target::Floor(floor)) => {
                let here = live.home.with_untracked(|home| {
                    home.areas
                        .iter()
                        .find(|known| known.id == area)
                        .map(|known| known.floor_id.clone())
                });
                if here.is_none_or(|here| here == floor) {
                    return;
                }
                let to = floor.clone();
                let moved = area.clone();
                crate::transition::around("place", move || {
                    live.home.update(|home| {
                        if let Some(known) = home.areas.iter_mut().find(|a| a.id == moved) {
                            known.floor_id = to;
                        }
                    });
                });
                self.send(async move { api::move_area(&area, floor.as_ref()).await });
            }
            // A device let go over a floor, an area over an area: not somewhere it can land.
            _ => {}
        }
    }

    /// Whether what's being dragged could land on `target`.
    fn takes(self, target: &Target) -> bool {
        matches!(
            (self.dragging.get_untracked(), target),
            (Some(Dragged::Device(_)), Target::Area(_))
                | (Some(Dragged::Area(_)), Target::Floor(_))
        )
    }
}

/// The handlers that make an element somewhere to drop: it says so while something that fits
/// is over it, and takes the drop.
fn landing(
    places: Places,
    target: Target,
) -> (
    impl Fn(ev::DragEvent) + Clone,
    impl Fn(ev::DragEvent) + Clone,
    impl Fn(ev::DragEvent) + Clone,
) {
    let over = {
        let target = target.clone();
        move |event: ev::DragEvent| {
            if !places.takes(&target) {
                return;
            }
            // Saying yes is what lets the browser drop here at all.
            event.prevent_default();
            if places
                .over
                .with_untracked(|over| over.as_ref() != Some(&target))
            {
                places.over.set(Some(target.clone()));
            }
        }
    };
    let leave = {
        let target = target.clone();
        move |event: ev::DragEvent| {
            // Crossing onto something inside it isn't leaving.
            let still_inside = event
                .current_target()
                .and_then(|here| here.dyn_into::<web_sys::Node>().ok())
                .zip(event.related_target())
                .is_some_and(|(here, to)| here.contains(to.dyn_ref::<web_sys::Node>()));
            if !still_inside
                && places
                    .over
                    .with_untracked(|over| over.as_ref() == Some(&target))
            {
                places.over.set(None);
            }
        }
    };
    let drop = move |event: ev::DragEvent| {
        if !places.takes(&target) {
            return;
        }
        event.prevent_default();
        // Not the floor's as well, when this is an area on it.
        event.stop_propagation();
        places.drop_on(target.clone());
    };
    (over, leave, drop)
}

/// What Settings opens the Floors and areas row to.
#[component]
pub fn Section() -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let places = Places {
        live,
        trouble: RwSignal::new(None),
        composing: RwSignal::new(None),
        name: RwSignal::new(String::new()),
        level: RwSignal::new(0),
        mistake: RwSignal::new(None),
        dragging: RwSignal::new(None),
        over: RwSignal::new(None),
        lingering: RwSignal::new(None),
        folded: RwSignal::new(BTreeSet::new()),
        removing: RwSignal::new(None),
    };

    // Something dragged that stays over a folded floor opens it, so it can be dropped inside.
    Effect::new(move |_| {
        let Some(floor) = places.lingering.get() else {
            return;
        };
        set_timeout(
            move || {
                if places.lingering.get_untracked().as_ref() == Some(&floor) {
                    places.folded.update(|folded| {
                        folded.remove(&floor);
                    });
                }
            },
            LINGER,
        );
    });

    // Only the areas, floors and the devices in them, not what those devices are reporting.
    let shape = Memo::new(move |_| {
        live.home.with(|home| {
            (
                home.floors.clone(),
                home.areas.clone(),
                home.devices.clone(),
            )
        })
    });

    view! {
        <div
            class="places"
            class:dragging-device=move || matches!(places.dragging.get(), Some(Dragged::Device(_)))
            class:dragging-area=move || matches!(places.dragging.get(), Some(Dragged::Area(_)))
        >
            {move || places.trouble.get().map(|why| view! { <p class="banner">{why}</p> })}
            {move || {
                let (floors, areas, devices) = shape.get();
                let travels = areas.len() + devices.len() <= THINGS_THAT_TRAVEL;
                let on_no_floor: Vec<Area> = areas
                    .iter()
                    .filter(|area| {
                        area.floor_id
                            .as_ref()
                            .is_none_or(|id| floors.iter().all(|floor| &floor.id != id))
                    })
                    .cloned()
                    .collect();
                let unassigned: Vec<Device> = devices
                    .iter()
                    .filter(|device| device.area_id.is_none())
                    .cloned()
                    .collect();
                let next = next_level(&floors);
                view! {
                    {floors.is_empty().then(|| view! {
                        <p class="muted small places-note">
                            "No floors yet. A floor is a level of the home; the areas on it come "
                            "after. Level 0 is the entrance, 1 above it, -1 below."
                        </p>
                    })}
                    {floors
                        .iter()
                        .map(|floor| {
                            let on_it: Vec<Area> = areas
                                .iter()
                                .filter(|area| area.floor_id.as_ref() == Some(&floor.id))
                                .cloned()
                                .collect();
                            floor_row(places, Some(floor.clone()), on_it, &devices, travels)
                        })
                        .collect_view()}
                    // What a removed floor left behind, and somewhere to drag an area off every
                    // floor. Only there when it has something, or something could be put in it.
                    // Always drawn and only shown or hidden: drawing it as a drag starts would
                    // take the thing being dragged off the page from under the pointer.
                    {
                        let empty = on_no_floor.is_empty();
                        view! {
                            <div
                                class="no-floor"
                                hidden=move || {
                                    empty && !matches!(places.dragging.get(), Some(Dragged::Area(_)))
                                }
                            >
                                {floor_row(places, None, on_no_floor, &devices, travels)}
                            </div>
                        }
                    }
                    {composer(places, Composing::NewFloor, "Add floor", "Upstairs", true, next)}
                    <div class="unassigned">
                        <div class="area-head">
                            <span class="area-name">"Unassigned devices"</span>
                            <span class="fold-state">{device_count(unassigned.len())}</span>
                        </div>
                        {chips(
                            places,
                            None,
                            unassigned,
                            "Nothing here. Drag a device out of its area to unplace it.",
                            travels,
                        )}
                    </div>
                }
            }}
        </div>
    }
}

/// A floor: a row that folds, the areas on it, and the way to add another. `None` is the areas
/// on no floor, which has no name to change and can't be removed.
fn floor_row(
    places: Places,
    floor: Option<Floor>,
    areas: Vec<Area>,
    devices: &[Device],
    travels: bool,
) -> AnyView {
    let id = floor.as_ref().map(|floor| floor.id.clone());
    let target = Target::Floor(id.clone());
    let (over, leave, drop) = landing(places, target.clone());
    let open = {
        let id = id.clone();
        Signal::derive(move || {
            id.as_ref()
                .is_none_or(|id| !places.folded.with(|folded| folded.contains(id)))
        })
    };
    let toggle = {
        let id = id.clone();
        move || {
            let Some(id) = id.clone() else {
                return;
            };
            places.folded.update(|folded| {
                if !folded.remove(&id) {
                    folded.insert(id);
                }
            })
        }
    };
    let count = areas.len();
    let state = match &floor {
        Some(floor) => format!("level {} · {}", floor.level, area_count(count)),
        None => area_count(count),
    };
    let actions = floor.as_ref().map(|floor| {
        let (edit_id, name, level) = (floor.id.clone(), floor.name.to_string(), floor.level);
        let remove_id = floor.id.clone();
        let label = floor.name.to_string();
        view! {
            <button
                type="button"
                class="icon-button"
                aria-label=format!("Rename {label} or change its level")
                on:click=move |_| {
                    places.folded.update(|folded| {
                        folded.remove(&edit_id);
                    });
                    places.compose(Composing::Floor(edit_id.clone()), &name, level);
                }
            >
                {icon(Icon::Edit)}
            </button>
            {remove(
                places,
                format!("floor:{remove_id}"),
                format!("Remove {label}. Its areas stay, on no floor."),
                move || {
                    let id = remove_id.clone();
                    places.send(async move { api::remove_floor(&id).await });
                },
            )}
        }
        .into_any()
    });
    let body = view! {
        <div class="areas">
            {id.clone().map(|id| composer(places, Composing::Floor(id), "", "", true, 0))}
            {areas
                .into_iter()
                .map(|area| area_row(places, area, devices, travels))
                .collect_view()}
            {id.clone().map(|id| {
                composer(places, Composing::NewArea(id), "Add area", "Kitchen", false, 0)
            })}
        </div>
    }
    .into_any();
    let lingered = id.clone();
    let is_over = move || places.over.with(|over| over.as_ref() == Some(&target));
    view! {
        <div
            class="floor"
            class:over=is_over
            on:dragover=move |event| {
                // Folded, with something held over it: about to open.
                if let Some(id) = &lingered
                    && places.dragging.with_untracked(Option::is_some)
                    && places.folded.with_untracked(|folded| folded.contains(id))
                    && places.lingering.with_untracked(|at| at.as_ref() != Some(id))
                {
                    places.lingering.set(Some(id.clone()));
                }
                over(event);
            }
            on:dragleave=leave
            on:drop=drop
        >
            {fold(
                None,
                Head {
                    icon: None,
                    title: match &floor {
                        Some(floor) => floor.name.to_string().into_any(),
                        None => "On no floor".into_any(),
                    },
                    state: Some(state.into_any()),
                    actions,
                },
                open,
                toggle,
                body,
            )}
        </div>
    }
    .into_any()
}

/// An area: its name, its devices, and the two things to do to it. Dragged by its grip onto
/// another floor.
fn area_row(places: Places, area: Area, devices: &[Device], travels: bool) -> AnyView {
    let here: Vec<Device> = devices
        .iter()
        .filter(|device| device.area_id.as_ref() == Some(&area.id))
        .cloned()
        .collect();
    let target = Target::Area(Some(area.id.clone()));
    let (over, leave, drop) = landing(places, target.clone());
    let name = area.name.to_string();
    let (edit_id, edit_name) = (area.id.clone(), name.clone());
    let remove_id = area.id.clone();
    let drag_id = area.id.clone();
    let is_dragged = {
        let id = area.id.clone();
        move || {
            places
                .dragging
                .with(|dragging| dragging.as_ref() == Some(&Dragged::Area(id.clone())))
        }
    };
    view! {
        <div
            class="area"
            class:over=move || places.over.with(|over| over.as_ref() == Some(&target))
            class:dragged=is_dragged
            style=travelling("area", area.id.as_str(), travels)
            on:dragover=over
            on:dragleave=leave
            on:drop=drop
        >
            <div class="area-head">
                <span
                    class="grip"
                    draggable="true"
                    title="Drag to another floor"
                    on:dragstart=move |event| {
                        if let Some(data) = event.data_transfer() {
                            let _ = data.set_data("text/plain", drag_id.as_ref());
                        }
                        event.stop_propagation();
                        places.dragging.set(Some(Dragged::Area(drag_id.clone())));
                    }
                    on:dragend=move |_| {
                        places.dragging.set(None);
                        places.over.set(None);
                        places.lingering.set(None);
                    }
                >
                    {icon(Icon::Grip)}
                </span>
                <span class="area-name">{name.clone()}</span>
                <span class="fold-state">{device_count(here.len())}</span>
                <span class="fold-actions">
                    <button
                        type="button"
                        class="icon-button"
                        aria-label=format!("Rename {name}")
                        on:click=move |_| {
                            places.compose(Composing::Area(edit_id.clone()), &edit_name, 0)
                        }
                    >
                        {icon(Icon::Edit)}
                    </button>
                    {remove(
                        places,
                        format!("area:{remove_id}"),
                        format!("Remove {name}. Its devices become unassigned."),
                        move || {
                            let id = remove_id.clone();
                            places.send(async move { api::remove_area(&id).await });
                        },
                    )}
                </span>
            </div>
            {composer(places, Composing::Area(area.id.clone()), "", "", false, 0)}
            {chips(places, Some(area.id.clone()), here, "Drop a device here.", travels)}
        </div>
    }
    .into_any()
}

/// The devices somewhere, as chips to drag, and where to drop one. `area` is `None` for the
/// unassigned devices.
fn chips(
    places: Places,
    area: Option<AreaId>,
    devices: Vec<Device>,
    empty: &'static str,
    travels: bool,
) -> AnyView {
    // An area takes drops anywhere on its row; the unassigned devices only have this list.
    let unassigned = area.is_none().then(|| landing(places, Target::Area(None)));
    let is_over =
        move || area.is_none() && places.over.with(|over| over == &Some(Target::Area(None)));
    let list = if devices.is_empty() {
        view! { <li class="muted small">{empty}</li> }.into_any()
    } else {
        devices
            .into_iter()
            .map(|device| chip(places, device, travels))
            .collect_view()
            .into_any()
    };
    match unassigned {
        Some((over, leave, drop)) => view! {
            <ul class="chips" class:over=is_over on:dragover=over on:dragleave=leave on:drop=drop>
                {list}
            </ul>
        }
        .into_any(),
        None => view! { <ul class="chips">{list}</ul> }.into_any(),
    }
}

/// A device, to drag into another area or out of every one. Safari is strict about drags:
/// setting the drag data is what starts one, and cancelling `dragstart` (the usual way to stop
/// an anchor's link-drag) ends it before it begins — so the link inside is simply not
/// draggable, and nothing is cancelled.
fn chip(places: Places, device: Device, travels: bool) -> AnyView {
    let id = device.id.to_string();
    let drag_id = device.id.clone();
    let is_dragged = {
        let id = device.id.clone();
        move || {
            places
                .dragging
                .with(|dragging| dragging.as_ref() == Some(&Dragged::Device(id.clone())))
        }
    };
    view! {
        <li
            class="chip-device"
            class:dragged=is_dragged
            draggable="true"
            style=travelling("chip", &id, travels)
            on:dragstart=move |event| {
                if let Some(data) = event.data_transfer() {
                    let _ = data.set_data("text/plain", drag_id.as_ref());
                }
                event.stop_propagation();
                places.dragging.set(Some(Dragged::Device(drag_id.clone())));
            }
            on:dragend=move |_| {
                places.dragging.set(None);
                places.over.set(None);
                places.lingering.set(None);
            }
        >
            <span class="grip" aria-hidden="true">{icon(Icon::Grip)}</span>
            <A href=format!("/devices/{id}") attr:class="row-link" attr:draggable="false">
                {device.name.to_string()}
            </A>
        </li>
    }
    .into_any()
}

/// The one composer everything is made and renamed with: a name, and for a floor its level.
///
/// With a `call` it has a quiet button of its own ("Add area") that opens it; without, it is
/// opened by the row it renames. It rolls down where the thing will appear, Enter saves, Escape
/// puts it away, and what's wrong with what was typed is said underneath.
fn composer(
    places: Places,
    what: Composing,
    call: &'static str,
    placeholder: &'static str,
    with_level: bool,
    // The level a new floor starts at.
    starting_level: i8,
) -> AnyView {
    let open = {
        let what = what.clone();
        Memo::new(move |_| {
            places
                .composing
                .with(|composing| composing.as_ref() == Some(&what))
        })
    };
    let field = NodeRef::<leptos::html::Input>::new();
    // The field is the point of opening it. Also when it's drawn already open: adding one
    // draws the list again, and the next name is typed straight after.
    Effect::new(move |_| {
        if open.get() {
            request_animation_frame(move || {
                if let Some(field) = field.get_untracked() {
                    let _ = field.focus();
                }
            });
        }
    });
    let is_new = !call.is_empty();
    let opener = is_new.then(|| {
        let what = what.clone();
        view! {
            <button
                type="button"
                class="namer-call"
                aria-expanded=move || open.get().to_string()
                on:click=move |_| {
                    if open.get_untracked() {
                        places.composing.set(None);
                    } else {
                        places.compose(what.clone(), "", starting_level);
                    }
                }
            >
                <span class="namer-plus" aria-hidden="true">{icon(Icon::Add)}</span>
                {call}
            </button>
        }
    });
    let step = move |by: i8| {
        move |_| {
            places
                .level
                .update(|level| *level = level.saturating_add(by))
        }
    };
    view! {
        <div class="namer" class:open=move || open.get()>
            {opener}
            <div class="drawer" class:open=move || open.get() inert=move || (!open.get()).then_some("")>
                <div class="drawer-inner">
                    <form
                        class="inline-form namer-form"
                        on:submit=move |event| {
                            event.prevent_default();
                            places.save();
                        }
                        on:keydown=move |event| {
                            if event.key() == "Escape" {
                                places.composing.set(None);
                            }
                        }
                    >
                        <input
                            type="text"
                            node_ref=field
                            aria-label="Name"
                            placeholder=placeholder
                            // Only the open one shows what's being typed: they all share it.
                            prop:value=move || if open.get() { places.name.get() } else { String::new() }
                            on:input:target=move |event| {
                                places.name.set(event.target().value());
                                places.mistake.set(None);
                            }
                        />
                        {with_level.then(|| view! {
                            <span class="stepper" role="group" aria-label="Level">
                                <button type="button" aria-label="A level down" on:click=step(-1)>
                                    "−"
                                </button>
                                <output>{move || format!("level {}", places.level.get())}</output>
                                <button type="button" aria-label="A level up" on:click=step(1)>
                                    "+"
                                </button>
                            </span>
                        })}
                        <button type="submit" class="add solid">
                            {if is_new { "Add" } else { "Save" }}
                        </button>
                        <button type="button" on:click=move |_| places.composing.set(None)>
                            {if is_new { "Done" } else { "Cancel" }}
                        </button>
                    </form>
                    {move || {
                        open.get()
                            .then(|| places.mistake.get())
                            .flatten()
                            .map(|why| view! { <p class="why">{why}</p> })
                    }}
                </div>
            </div>
        </div>
    }
    .into_any()
}

/// Remove, asked twice: the first press turns the button into the question, the second is the
/// answer. Looking away — pressing anything else that asks — takes the question back.
fn remove(places: Places, key: String, what: String, go: impl Fn() + 'static) -> AnyView {
    let asking = {
        let key = key.clone();
        Memo::new(move |_| {
            places
                .removing
                .with(|removing| removing.as_ref() == Some(&key))
        })
    };
    view! {
        <button
            type="button"
            class="icon-button delete"
            class:asking=move || asking.get()
            aria-label=what.clone()
            title=what
            on:click=move |_| {
                if asking.get_untracked() {
                    places.removing.set(None);
                    go();
                } else {
                    places.removing.set(Some(key.clone()));
                }
            }
            on:blur=move |_| {
                if asking.get_untracked() {
                    places.removing.set(None);
                }
            }
        >
            {icon(Icon::Remove)}
            <span class="asking-words">"Remove?"</span>
        </button>
    }
    .into_any()
}

/// What lets a chip or an area travel to where it was dropped (`html[data-nav="place"]`).
fn travelling(kind: &str, id: &str, travels: bool) -> Option<String> {
    travels.then(|| format!("--vt: {kind}-{}", crate::transition::ident(id)))
}

fn device_count(devices: usize) -> String {
    match devices {
        0 => "no devices".to_owned(),
        1 => "1 device".to_owned(),
        n => format!("{n} devices"),
    }
}

fn area_count(areas: usize) -> String {
    match areas {
        0 => "no areas".to_owned(),
        1 => "1 area".to_owned(),
        n => format!("{n} areas"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_floor_needs_a_name_and_a_whole_number() {
        assert!(
            matches!(floor_draft(" Upstairs ", " 1 "), Ok((name, 1)) if name.as_str() == "Upstairs")
        );
        assert_eq!(
            floor_draft("", "0").expect_err("no name"),
            "A floor needs a name."
        );
        assert!(
            floor_draft("Cellar", "below")
                .expect_err("not a number")
                .starts_with("A floor's level is a whole number")
        );
    }

    #[test]
    fn a_new_floor_starts_above_the_highest() {
        let floor = |level| Floor {
            id: format!("f{}", i16::from(level) + 200)
                .parse()
                .expect("valid"),
            name: "Floor".parse().expect("valid"),
            level,
        };
        assert_eq!(next_level(&[]), 0);
        assert_eq!(next_level(&[floor(-1), floor(2), floor(0)]), 3);
        assert_eq!(next_level(&[floor(i8::MAX)]), i8::MAX);
    }

    #[test]
    fn the_arrangement_reads_as_a_person_would_say_it() {
        assert_eq!(summary(0, 0, 0), "0 floors · 0 areas");
        assert_eq!(summary(1, 8, 0), "1 floor · 8 areas");
        assert_eq!(summary(2, 1, 3), "2 floors · 1 area · 3 unassigned");
    }

    #[test]
    fn counts_read_as_a_person_would_say_them() {
        assert_eq!(device_count(0), "no devices");
        assert_eq!(device_count(1), "1 device");
        assert_eq!(device_count(4), "4 devices");
        assert_eq!(area_count(2), "2 areas");
    }
}
