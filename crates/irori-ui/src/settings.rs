//! The Settings page: the instance itself, the home's arrangement, and the machine running it.
//!
//! Four sections, in the order someone setting a home up is likely to want them: is the instance
//! I mean to run? the floors and areas that say what's where (a card that folds away until
//! wanted)? the people allowed in (none yet); and the machine it all runs on. The last of these
//! is asked for on demand rather than kept — a Settings check that cached could shrug at a disk
//! that filled since the last look.

use irori_types::{Area, AreaId, Device, DeviceId, Name};
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;

use crate::api;

#[component]
pub fn Settings() -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let trouble = RwSignal::new(None::<String>);
    let adding = RwSignal::new(String::new());
    // Where an area is going: the floor whose + is open, if any. An area is made straight onto
    // a floor and stays there, so the + next to a floor's name is the only way one lands.
    let new_area_floor = RwSignal::new(None::<irori_types::FloorId>);
    // Which area's name is being edited, if any: only one at a time.
    let editing = RwSignal::new(None::<AreaId>);
    let draft = RwSignal::new(String::new());
    // Which device is being dragged between areas, if any. A drag only happens on this page, so
    // the signal lives here: the source rows set it on dragstart, the area lists read it to arm
    // themselves, and a drop clears it (as does dragend, in case the drag fell somewhere empty).
    let dragging = RwSignal::new(None::<DeviceId>);
    // Which floors have their areas folded away, so a horde of areas doesn't push the rest of
    // the page down. The chevron on a floor's row flips one on and off; only the areas fold.
    let collapsed = RwSignal::new(Vec::<irori_types::FloorId>::new());
    // Whether a restart is under way. The button stays "Restarting…" until the core answers with a
    // different instance — the new boot's — which is how "it's back" is known. Anything about
    // uptime would be guesswork: a process that happened to start a minute before you pressed
    // looks exactly like one that restarted a minute later, and a slow restart could pass any
    // freshness bound. So the core says which boot it is (`/api/health`'s boot_id), and the
    // effect below lets the button go again the moment the boot changes. Nothing here needs to
    // wait for the POST itself: the server answers Acceptance before it actually goes.
    let restarting = RwSignal::new(false);
    // The boot the restart set off from: the boot_id the health showed when the button was
    // pressed. None — not armed. An empty boot_id would be "we never saw one to come from", in
    // which case any boot that appears is new.
    let restart_from = RwSignal::new(None::<String>);
    Effect::new(move |_| {
        let Some(from) = restart_from.get() else {
            return;
        };
        let Some(health) = live.health.get() else {
            return;
        };
        // The instance we set off from is gone once the core answers with a different one. The
        // old instance's own answers keep its own boot_id, so they can't clear the button early.
        if !health.boot_id.is_empty() && health.boot_id != from {
            restarting.set(false);
            restart_from.set(None);
        }
    });
    let restart = move || {
        if restarting.get_untracked() {
            return;
        }
        if !window()
            .confirm_with_message(
                "Restart Irori? It stays where it runs while it starts again (same container, \
                 same service) — the page just goes quiet for a few seconds, and every device \
                 reconnects.",
            )
            .unwrap_or(false)
        {
            return;
        }
        restarting.set(true);
        restart_from.set(Some(
            live.health
                .get_untracked()
                .map(|health| health.boot_id)
                .unwrap_or_default(),
        ));
        spawn_local(async move {
            match api::restart().await {
                // The new instance is coming up; the page notices it on its own.
                Ok(()) => {}
                Err(why) => {
                    restarting.set(false);
                    restart_from.set(None);
                    trouble.set(Some(why));
                }
            }
        });
    };

    // Only the areas, floors and the devices in them, not what those devices are reporting. A
    // page that redrew every time a sensor spoke would throw away a half-typed name with it.
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
    // Which floor's name and level are being edited, if any: only one at a time.
    let floor_editing = RwSignal::new(None::<irori_types::FloorId>);
    let floor_draft_name = RwSignal::new(String::new());
    let floor_draft_level = RwSignal::new(String::new());

    // The machine under the instance. Asked once when the page opens, and again when "Ask again"
    // is clicked: nothing here is worth polling, and the values are only any use if they're the
    // machine's, now.
    let system = RwSignal::new(None::<Result<crate::api::System, String>>);
    let fetching = RwSignal::new(false);
    let ask = move || {
        if fetching.get_untracked() {
            return;
        }
        fetching.set(true);
        system.set(None);
        spawn_local(async move {
            let result = api::fetch_system().await;
            system.set(Some(result));
            fetching.set(false);
        });
    };
    Effect::new(move |_| ask());

    let add_floor = move || {
        let Some(named) = named(floor_name.get(), trouble) else {
            return;
        };
        let Ok(at) = floor_level.get().trim().parse::<i8>() else {
            trouble.set(Some(
                "A floor's level is a whole number: 0 for the entrance, 1 above it, -1 below."
                    .to_owned(),
            ));
            return;
        };
        floor_name.set(String::new());
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
        <div class="page-head">
            <h1>"Settings"</h1>
            <div class="page-actions">
                <button
                    type="button"
                    disabled=move || restarting.get()
                    on:click=move |_| restart()
                >
                    {move || if restarting.get() { "Restarting…" } else { "Restart" }}
                </button>
            </div>
        </div>
        <p class="lede">
            "The instance itself, what's where in the home, and the machine all of it runs on."
        </p>

        {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}

        <nav class="settings-menu" aria-label="Sections of Settings">
            <a href="#instance">"Instance"</a>
            <a href="#floors-and-areas">"Floors & areas"</a>
            <a href="#users">"Users"</a>
            <a href="#system">"System"</a>
        </nav>

        <section class="card settings-section" id="instance">
            <h2>"Instance"</h2>
            {move || match live.health.get() {
                None => view! { <p class="muted">"Asking…"</p> }.into_any(),
                Some(health) => view! {
                    <dl>
                        <dt>"Version"</dt>
                        <dd>
                            {health.version}
                            {(!health.commit.is_empty())
                                .then(|| format!(" · {}", health.commit))}
                        </dd>
                        {(!health.built_at.is_empty()).then(|| view! {
                            <dt>"Built"</dt>
                            <dd>{health.built_at.clone()}</dd>
                        })}
                        <dt>"Uptime"</dt>
                        <dd>{uptime(health.uptime_ms)}</dd>
                        <dt>"Database"</dt>
                        <dd>
                            {format!("SQLite {} · {}", health.sqlite.version,
                                health.sqlite.journal_mode.to_uppercase())}
                        </dd>
                        <dt>"Built with"</dt>
                        <dd>
                            {if health.features.is_empty() {
                                "nothing optional (barebones)".to_owned()
                            } else {
                                health.features.join(", ")
                            }}
                        </dd>
                    </dl>
                }
                .into_any(),
            }}
        </section>

        // It folds, but starts open: this is where the home's arrangement is managed, so the
        // floors, the areas on them, and the unassigned devices are useful to see at once.
        <details class="card settings-section floors" id="floors-and-areas" open>
            <summary>"Floors and areas"</summary>
            <p class="muted small">
                "Floors are the levels of the home, lowest first, and the areas are the places on "
                "them. Make a floor, then add the areas that sit on it with the + on its row; an "
                "area stays on the floor it was made on. Irori never invents an area, even when a "
                "device says where it thinks it is."
            </p>

            <div class="room-head">
                <h2>"Floors"</h2>
                <span class="muted">
                    {move || match shape.get().2.len() {
                        0 => "no floors".to_owned(),
                        1 => "1 floor".to_owned(),
                        n => format!("{n} floors"),
                    }}
                </span>
            </div>
            <p class="muted small">
                "The level is a whole number: 0 for the entrance floor, 1 above it, -1 for a "
                "cellar. Rename a floor or move it to another level and its areas come with it."
            </p>

            {move || {
                let (areas, devices, floors) = shape.get();
                if floors.is_empty() && areas.is_empty() {
                    return view! {
                        <p class="empty">
                            "No floors yet. Make the first one below; its areas come after."
                        </p>
                    }
                    .into_any();
                }

                let mut groups: Vec<AnyView> = floors
                    .iter()
                    .map(|floor| {
                        let on_it = areas
                            .iter()
                            .filter(|area| area.floor_id.as_ref() == Some(&floor.id))
                            .cloned()
                            .collect();
                        floor_group(
                            floor.clone(),
                            on_it,
                            &devices,
                            editing,
                            draft,
                            adding,
                            new_area_floor,
                            floor_editing,
                            floor_draft_name,
                            floor_draft_level,
                            trouble,
                            live,
                            dragging,
                            collapsed,
                        )
                    })
                    .collect();

                // Anything a removed floor left behind, gathered so its areas still have their
                // tools. There's no + here: a new area is made on a floor.
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
                    groups.push(unfloored_group(
                        unfloored,
                        &devices,
                        editing,
                        draft,
                        trouble,
                        live,
                        dragging,
                    ));
                }

                groups.into_iter().collect_view().into_any()
            }}

            {move || {
                let (_, devices, _) = shape.get();
                let unplaced: Vec<Device> = devices
                    .iter()
                    .filter(|device| device.area_id.is_none())
                    .cloned()
                    .collect();
                view! {
                    <section class="card room">
                        <div class="room-head">
                            <h2>"Unassigned devices"</h2>
                            <span class="muted">{count(unplaced.len())}</span>
                        </div>
                        <ul
                            class="room-devices drop-zone"
                            class:armed=move || dragging.get().is_some()
                            on:dragover=move |ev| ev.prevent_default()
                            on:drop=drop_into(dragging, &devices, None, trouble, live)
                        >
                            {if unplaced.is_empty() {
                                view! {
                                    <li class="muted">
                                        "Nothing here. Drag a device out of its area to unplace it."
                                    </li>
                                }
                                .into_any()
                            } else {
                                unplaced
                                    .into_iter()
                                    .map(|device| in_area(device, dragging))
                                    .collect_view()
                                    .into_any()
                            }}
                        </ul>
                    </section>
                }
            }}

            <form
                class="inline-form"
                on:submit=move |ev| {
                    ev.prevent_default();
                    add_floor();
                }
            >
                <input
                    type="text"
                    aria-label="Name of the new floor"
                    placeholder="Upstairs"
                    prop:value=floor_name
                    on:input:target=move |ev| floor_name.set(ev.target().value())
                />
                <input
                    type="number"
                    class="level"
                    aria-label="Level"
                    min="-128"
                    max="127"
                    prop:value=floor_level
                    on:input:target=move |ev| floor_level.set(ev.target().value())
                />
                <button
                    type="submit"
                    class="add solid"
                    disabled=move || floor_name.get().trim().is_empty()
                >
                    "Add floor"
                </button>
            </form>
        </details>

        <section class="card settings-section" id="users">
            <h2>"Users"</h2>
            <p class="muted">
                "No users yet — and nothing to sign in with. IroriOS is for the person in the "
                "room with it: anyone who can reach it is looking after the home. That changes "
                "before it runs in anyone else's home (ROADMAP M1.6)."
            </p>
        </section>

        <section class="card settings-section" id="system">
            <div class="room-head">
                <h2>"System"</h2>
                <span class="room-actions">
                    <button type="button" disabled=move || fetching.get() on:click=move |_| ask()>
                        {move || if fetching.get() { "Asking again…" } else { "Ask again" }}
                    </button>
                </span>
            </div>
            {move || match system.get() {
                None => view! { <p class="muted">"Asking the machine…"</p> }.into_any(),
                Some(Err(why)) => view! {
                    <p class="why">{why}</p>
                    <p class="muted small">
                        "The machine running Irori stopped answering, or its answer didn't read."
                    </p>
                }
                .into_any(),
                Some(Ok(machine)) => view! {
                    <dl>
                        {machine.host.as_ref().map(|host| view! {
                            <dt>"Host"</dt>
                            <dd>{host.clone()}</dd>
                        })}
                        <dt>"Operating system"</dt>
                        <dd>
                            {machine.os}
                            {(!machine.os_version.is_empty())
                                .then(|| format!(" {}", machine.os_version))}
                        </dd>
                        {(!machine.kernel.is_empty()).then(|| view! {
                            <dt>"Kernel"</dt>
                            <dd>{machine.kernel.clone()}</dd>
                        })}
                        <dt>"Architecture"</dt>
                        <dd>{machine.arch}</dd>
                        {(!machine.cpu.is_empty()).then(|| view! {
                            <dt>"Processor"</dt>
                            <dd>{machine.cpu.clone()}</dd>
                        })}
                        <dt>"Cores"</dt>
                        <dd>{machine.cpu_cores}</dd>
                        <dt>"Memory"</dt>
                        <dd>
                            {format!("{} used · {} total", bytes(machine.memory_used),
                                bytes(machine.memory_total))}
                        </dd>
                        <dt>"Disk"</dt>
                        <dd>
                            {if machine.disk.total > 0 {
                                format!("{} used · {} free", bytes(machine.disk.used),
                                    bytes(machine.disk.available))
                            } else {
                                "The volume with the data didn't answer.".to_owned()
                            }}
                            {(!machine.disk.mount.is_empty()).then(|| {
                                format!(" ({})", machine.disk.mount)
                            })}
                        </dd>
                        <dt>"Up"</dt>
                        <dd>
                            "the machine has been up "{uptime(u128::from(machine.uptime_secs) * 1000)}
                        </dd>
                    </dl>
                    <p class="muted small">
                        "Irori's own uptime is on the Instance card above; this is the machine's."
                    </p>
                }
                .into_any(),
            }}
        </section>
    }
}

/// One area: its name, what's in it, and the two things you can do to it. Its floor isn't
/// repeated here — the area sits under its floor's row above — and there's no way to move it:
/// an area is made on a floor and stays there.
#[allow(clippy::too_many_arguments)]
fn area_card(
    area: Area,
    all: &[Device],
    editing: RwSignal<Option<AreaId>>,
    draft: RwSignal<String>,
    trouble: RwSignal<Option<String>>,
    live: crate::Live,
    dragging: RwSignal<Option<DeviceId>>,
) -> AnyView {
    let devices: Vec<Device> = all
        .iter()
        .filter(|device| device.area_id.as_ref() == Some(&area.id))
        .cloned()
        .collect();
    let id = area.id.clone();
    let display = area.name.to_string();
    let title = display.clone();
    let being_edited = {
        let id = id.clone();
        move || editing.get().as_ref() == Some(&id)
    };

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
        let what = display.clone();
        let count = devices.len();
        move || {
            // Nothing is lost by removing an area — the devices stay, and what was said about
            // them is kept — so this asks only when it would visibly move things.
            if count > 0
                && !window()
                    .confirm_with_message(&format!(
                        "Remove {what}? The {} in it will have no area until you put them \
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
        let display = display.clone();
        move |_| {
            draft.set(display.clone());
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
                                    aria-label="Name of this area"
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
                        view! { <h2>{title.clone()}</h2> }.into_any()
                    }
                }}
                <span class="muted">{count(devices.len())}</span>
                <span class="room-actions">
                    <button
                        type="button"
                        class="icon-button"
                        aria-label=format!("Rename {display}")
                        on:click=start
                    >
                        {icon(Icon::Edit)}
                    </button>
                    <button
                        type="button"
                        class="icon-button delete"
                        aria-label=format!("Remove {display}")
                        on:click=move |_| {
                            let remove = remove.clone();
                            remove();
                        }
                    >
                        {icon(Icon::Remove)}
                    </button>
                </span>
            </div>
            <ul
                class="room-devices drop-zone"
                class:armed=move || dragging.get().is_some()
                on:dragover=move |ev| ev.prevent_default()
                on:drop=drop_into(dragging, all, Some(&area.id), trouble, live)
            >
                {if devices.is_empty() {
                    view! {
                        <li class="muted">
                            "Nothing in here yet — drag a device here, or use its own page."
                        </li>
                    }
                    .into_any()
                } else {
                    devices
                        .into_iter()
                        .map(|device| in_area(device, dragging))
                        .collect_view()
                        .into_any()
                }}
            </ul>
        </section>
    }
    .into_any()
}

/// One floor and the areas on it, made to be seen together: the floor's row carries the tools to
/// rename it, move its level, remove it, and add another area straight onto it — an area is made
/// on a floor and stays there, so there's no per-area floor picker. Removing a floor that has
/// areas on it asks first, because everything under it visibly moves to "On no floor".
#[allow(clippy::too_many_arguments)]
fn floor_group(
    floor: irori_types::Floor,
    on_it: Vec<Area>,
    devices: &[Device],
    area_editing: RwSignal<Option<AreaId>>,
    area_draft: RwSignal<String>,
    adding: RwSignal<String>,
    new_area_floor: RwSignal<Option<irori_types::FloorId>>,
    editing: RwSignal<Option<irori_types::FloorId>>,
    draft_name: RwSignal<String>,
    draft_level: RwSignal<String>,
    trouble: RwSignal<Option<String>>,
    live: crate::Live,
    dragging: RwSignal<Option<DeviceId>>,
    collapsed: RwSignal<Vec<irori_types::FloorId>>,
) -> AnyView {
    let name = floor.name.to_string();
    let level = floor.level;
    let id = floor.id.clone();
    let area_count = on_it.len();
    // Owned copy for the fold closure below, which has to be 'static (it outlives this call).
    let devices: Vec<Device> = devices.to_vec();
    let folded = {
        let id = id.clone();
        move || collapsed.get().contains(&id)
    };
    let toggle_folded = {
        let id = id.clone();
        move |_| {
            if collapsed.get().contains(&id) {
                collapsed.set(
                    collapsed
                        .get()
                        .into_iter()
                        .filter(|other| other != &id)
                        .collect(),
                );
            } else {
                collapsed.update(|list| list.push(id.clone()));
            }
        }
    };
    let being_edited = {
        let id = id.clone();
        move || editing.get().as_ref() == Some(&id)
    };
    let being_added = {
        let id = id.clone();
        move || new_area_floor.get().as_ref() == Some(&id)
    };
    let toggle_add = {
        let id = id.clone();
        move |_| {
            if new_area_floor.get().as_ref() == Some(&id) {
                new_area_floor.set(None);
            } else {
                new_area_floor.set(Some(id.clone()));
            }
        }
    };
    let save = {
        let id = id.clone();
        move || {
            let Some(named) = named(draft_name.get(), trouble) else {
                return;
            };
            let Ok(at) = draft_level.get().trim().parse::<i8>() else {
                trouble.set(Some(
                    "A floor's level is a whole number: 0 for the entrance, 1 above it, -1 below."
                        .to_owned(),
                ));
                return;
            };
            editing.set(None);
            let id = id.clone();
            spawn_local(async move {
                match api::edit_floor(&id, Some(named), Some(at)).await {
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
        let name = name.clone();
        move |_| {
            draft_name.set(name.clone());
            draft_level.set(level.to_string());
            editing.set(Some(id.clone()));
        }
    };
    let remove = {
        let id = id.clone();
        let what = name.clone();
        move |_| {
            if area_count > 0
                && !window()
                    .confirm_with_message(&format!(
                        "Remove {what}? The {} on it will have no floor until you put them on \
                         another.",
                        count_of(area_count),
                    ))
                    .unwrap_or(false)
            {
                return;
            }
            let id = id.clone();
            spawn_local(async move {
                match api::remove_floor(&id).await {
                    Ok(()) => crate::refresh(live),
                    Err(why) => trouble.set(Some(why)),
                }
            });
        }
    };
    let add_area = {
        let id = id.clone();
        move || {
            let Some(name) = named(adding.get(), trouble) else {
                return;
            };
            adding.set(String::new());
            new_area_floor.set(None);
            let floor = Some(id.clone());
            spawn_local(async move {
                match api::add_area(name, floor.as_ref()).await {
                    Ok(()) => {
                        trouble.set(None);
                        crate::refresh(live);
                    }
                    Err(why) => trouble.set(Some(why)),
                }
            });
        }
    };

    view! {
        <div class="room-head floor-group">
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
                                aria-label="Name of this floor"
                                prop:value=draft_name
                                on:input:target=move |ev| draft_name.set(ev.target().value())
                            />
                            <input
                                type="number"
                                class="level"
                                aria-label="Level"
                                min="-128"
                                max="127"
                                prop:value=draft_level
                                on:input:target=move |ev| draft_level.set(ev.target().value())
                            />
                            <button
                                type="submit"
                                class="add"
                                disabled=move || draft_name.get().trim().is_empty()
                            >
                                "Save"
                            </button>
                            <button type="button" on:click=move |_| editing.set(None)>"Cancel"</button>
                        </form>
                    }
                    .into_any()
                } else {
                    let expanded_now = {
                        let id = id.clone();
                        move || !collapsed.get().contains(&id)
                    };
                    let chevron_text = {
                        let id = id.clone();
                        move || {
                            if collapsed.get().contains(&id) {
                                "▸"
                            } else {
                                "▾"
                            }
                        }
                    };
                    let fold_label = {
                        let id = id.clone();
                        let name = name.clone();
                        move || {
                            if collapsed.get().contains(&id) {
                                format!("Show the areas on {name}")
                            } else {
                                format!("Hide the areas on {name}")
                            }
                        }
                    };
                    view! {
                        <button
                            type="button"
                            class="chevron"
                            aria-expanded=expanded_now.clone()
                            aria-label=fold_label.clone()
                            on:click=toggle_folded.clone()
                        >
                            {chevron_text.clone()}
                        </button>
                        <h2 class="floor-heading">{name.clone()}</h2>
                        <span class="muted small">{format!("level {level}")}</span>
                        <span class="room-actions">
                            <button
                                type="button"
                                class="icon-button solid"
                                aria-label="Add an area to this floor"
                                on:click=toggle_add.clone()
                            >
                                {icon(Icon::Add)}
                            </button>
                            <button
                                type="button"
                                class="icon-button"
                                aria-label="Rename this floor or change its level"
                                on:click=start.clone()
                            >
                                {icon(Icon::Edit)}
                            </button>
                            <button
                                type="button"
                                class="icon-button delete"
                                aria-label="Remove this floor"
                                on:click=remove.clone()
                            >
                                {icon(Icon::Remove)}
                            </button>
                        </span>
                    }
                    .into_any()
                }
            }}
        </div>
        {move || {
            if folded() {
                // Folded: the counts live in the "Floors" heading and each overflow is their
                // own surprise, so a closed floor simply shows nothing.

                return ().into_any();
            }
            if on_it.is_empty() {
                view! {
                    <p class="muted small">"No areas on this floor yet — add one with the +."</p>
                }
                .into_any()
            } else {
                on_it
                    .iter()
                    .cloned()
                    .map(|area| {
                        area_card(
                            area,
                            &devices,
                            area_editing,
                            area_draft,
                            trouble,
                            live,
                            dragging,
                        )
                    })
                    .collect_view()
                    .into_any()
            }
        }}
        {move || {
            if !being_added() {
                return ().into_any();
            }
            let add_area = add_area.clone();
            view! {
                <form
                    class="inline-form"
                    on:submit=move |ev| {
                        ev.prevent_default();
                        add_area();
                    }
                >
                    <input
                        type="text"
                        aria-label="Name of the new area on this floor"
                        placeholder="Kitchen"
                        prop:value=adding
                        on:input:target=move |ev| adding.set(ev.target().value())
                    />
                    <button
                        type="submit"
                        class="add"
                        disabled=move || adding.get().trim().is_empty()
                    >
                        "Add area"
                    </button>
                    <button type="button" on:click=move |_| new_area_floor.set(None)>"Cancel"</button>
                </form>
            }
            .into_any()
        }}
    }
    .into_any()
}

/// The areas a removed floor left behind, gathered so they still have their tools. There's no +
/// here: a new area is made on a floor, not on none.
fn unfloored_group(
    areas: Vec<Area>,
    devices: &[Device],
    editing: RwSignal<Option<AreaId>>,
    draft: RwSignal<String>,
    trouble: RwSignal<Option<String>>,
    live: crate::Live,
    dragging: RwSignal<Option<DeviceId>>,
) -> AnyView {
    view! {
        <div class="room-head floor-group">
            <h2 class="floor-heading">"On no floor"</h2>
        </div>
        {areas
            .into_iter()
            .map(|area| {
                area_card(area, devices, editing, draft, trouble, live, dragging)
            })
            .collect_view()}
    }
    .into_any()
}

/// The small stroke icons the floor and area rows use on their buttons, plus the grip shown on
/// each draggable device row.
#[derive(Clone, Copy)]
enum Icon {
    Grip,
    Add,
    Edit,
    Remove,
}

fn icon(kind: Icon) -> AnyView {
    match kind {
        Icon::Grip => view! {
            <svg
                viewBox="0 0 24 24"
                fill="currentColor"
                stroke="none"
                aria-hidden="true"
            >
                <circle cx="9" cy="6" r="1.7"></circle>
                <circle cx="15" cy="6" r="1.7"></circle>
                <circle cx="9" cy="12" r="1.7"></circle>
                <circle cx="15" cy="12" r="1.7"></circle>
                <circle cx="9" cy="18" r="1.7"></circle>
                <circle cx="15" cy="18" r="1.7"></circle>
            </svg>
        }
        .into_any(),
        Icon::Add => view! {
            <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
                aria-hidden="true"
            >
                <path d="M12 5v14"></path>
                <path d="M5 12h14"></path>
            </svg>
        }
        .into_any(),
        Icon::Edit => view! {
            <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
                aria-hidden="true"
            >
                <path d="M12 20h9"></path>
                <path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z"></path>
            </svg>
        }
        .into_any(),
        Icon::Remove => view! {
            <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
                aria-hidden="true"
            >
                <path d="M3 6h18"></path>
                <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"></path>
                <path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"></path>
            </svg>
        }
        .into_any(),
    }
    .into_any()
}

/// The drop side of dragging a device into or out of an area. `into` names the area being
/// dropped on; `None` means the "Unassigned devices" list. The same PATCH the device's own page
/// uses puts it where it was dropped, with `Nowhere` for "out" — deliberately no area, so the
/// protocol's suggestion can't immediately put it straight back. Dropping where it already is
/// does nothing.
fn drop_into(
    dragging: RwSignal<Option<DeviceId>>,
    all: &[Device],
    into: Option<&AreaId>,
    trouble: RwSignal<Option<String>>,
    live: crate::Live,
) -> impl Fn(web_sys::DragEvent) + use<> {
    let all: Vec<Device> = all.to_vec();
    let into = into.cloned();
    move |event| {
        event.prevent_default();
        let Some(device_id) = dragging.get() else {
            return;
        };
        let Some(device) = all.iter().find(|device| device.id == device_id) else {
            return;
        };
        if device.area_id == into {
            dragging.set(None);
            return;
        }
        let area = into
            .as_ref()
            .map_or_else(api::WhereTo::nowhere, |id| api::WhereTo::In(id.clone()));
        let edit = api::DeviceEdit {
            area: Some(Some(area)),
            ..api::DeviceEdit::default()
        };
        let id = device_id.clone();
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
}

/// A device row: draggable so it can be dropped onto another area's list or the "Unassigned
/// devices" one. Drag sets the shared `dragging` signal; the area lists arm and clear themselves
/// around it. Safari is strict about drags: setting the drag data is what starts a drag, and
/// cancelling `dragstart` (the usual way to stop an anchor's link-drag) ends it before it begins
/// — so the link inside is simply not draggable, and nothing is cancelled.
fn in_area(device: Device, dragging: RwSignal<Option<DeviceId>>) -> AnyView {
    let id = device.id.to_string();
    let name = device.name.to_string();
    let through = device.protocol.to_string();
    let drag_id = device.id.clone();
    view! {
        <li
            draggable="true"
            on:dragstart=move |event| {
                if let Some(data) = event.data_transfer() {
                    let _ = data.set_data("text/plain", drag_id.as_ref());
                }
                dragging.set(Some(drag_id.clone()));
            }
            on:dragend=move |_| dragging.set(None)
        >
            <span class="grip" aria-hidden="true">{icon(Icon::Grip)}</span>
            <A href=format!("/devices/{id}") attr:draggable="false">{name}</A>
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

/// Uptime a person can read, to one unit: seconds, then minutes, then hours, then days.
fn uptime(ms: u128) -> String {
    let seconds = ms / 1000;
    let (value, unit) = match seconds {
        0..60 => (seconds, "second"),
        60..3600 => (seconds / 60, "minute"),
        3600..86400 => (seconds / 3600, "hour"),
        _ => (seconds / 86400, "day"),
    };
    format!("{value} {unit}{}", if value == 1 { "" } else { "s" })
}

/// A size a person can read, to one decimal: 512 B, 4.0 KB, 1.5 GB.
fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let shown = if unit == 0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    };
    format!("{shown} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_reads_as_a_person_would_say_it() {
        assert_eq!(uptime(1), "0 seconds");
        assert_eq!(uptime(1_000), "1 second");
        assert_eq!(uptime(90_000), "1 minute");
        assert_eq!(uptime(3_600_000), "1 hour");
        assert_eq!(uptime(90_000_000), "1 day");
        assert_eq!(uptime(180_000_000), "2 days");
    }

    #[test]
    fn device_counts_read_as_a_person_would_say_them() {
        assert_eq!(count(0), "no devices");
        assert_eq!(count(1), "1 device");
        assert_eq!(count(4), "4 devices");
    }

    #[test]
    fn sizes_read_as_a_person_would_say_them() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1_024), "1.0 KB");
        assert_eq!(bytes(1_500_000_000), "1.4 GB");
    }
}
