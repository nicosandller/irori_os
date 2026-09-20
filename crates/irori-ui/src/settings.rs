//! The Settings page: the instance itself, the home's arrangement, and the machine running it.
//!
//! Four sections, in the order someone setting a home up is likely to want them: is the instance
//! I mean to run? the floors and areas that say what's where (a card that folds away until
//! wanted)? the people allowed in (none yet); and the machine it all runs on. The last of these
//! is asked for on demand rather than kept — a Settings check that cached could shrug at a disk
//! that filled since the last look.

use irori_types::{Area, AreaId, Device, Name};
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;

use crate::api;

#[component]
pub fn Settings() -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let trouble = RwSignal::new(None::<String>);
    let adding = RwSignal::new(String::new());
    // Which area's name is being edited, if any: only one at a time.
    let editing = RwSignal::new(None::<AreaId>);
    let draft = RwSignal::new(String::new());

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

        <details class="card settings-section floors" id="floors-and-areas" open>
            <summary>"Floors and areas"</summary>
            <p class="muted small">
                "Floors are the levels of the home, lowest first; areas are the places on them, "
                "and the devices live in the areas. Irori never invents an area, even when a "
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
                "cellar. Make the floors first and the areas below sit on them; renaming a floor "
                "or moving it keeps its areas on it."
            </p>
            <ul class="room-devices">
                {move || {
                    shape
                        .get()
                        .2
                        .into_iter()
                        .map(|floor| {
                            floor_row(
                                floor,
                                floor_editing,
                                floor_draft_name,
                                floor_draft_level,
                                trouble,
                                live,
                            )
                        })
                        .collect_view()
                }}
            </ul>
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
                    class="add"
                    disabled=move || floor_name.get().trim().is_empty()
                >
                    "Add floor"
                </button>
            </form>

            <div class="room-head areas-head">
                <h2>"Areas"</h2>
                <span class="muted">
                    {move || match shape.get().0.len() {
                        0 => "no areas".to_owned(),
                        1 => "1 area".to_owned(),
                        n => format!("{n} areas"),
                    }}
                </span>
            </div>
            <p class="muted small">
                "An area is a place you can point at — a kitchen, the hall, the shed. A device "
                "that asks for an area you've made goes straight into it."
            </p>
            <form
                class="inline-form"
                on:submit=move |ev| {
                    ev.prevent_default();
                    add();
                }
            >
                <input
                    type="text"
                    aria-label="Name of the new area"
                    placeholder="Kitchen"
                    prop:value=adding
                    on:input:target=move |ev| adding.set(ev.target().value())
                />
                <button type="submit" class="add" disabled=move || adding.get().trim().is_empty()>
                    "Add"
                </button>
            </form>

            {move || {
                let (areas, devices, floors) = shape.get();
                if areas.is_empty() {
                    return view! {
                        <p class="empty">"No areas yet. Add one above, and devices can go in it."</p>
                    }
                    .into_any();
                }
                if floors.is_empty() {
                    return areas
                        .iter()
                        .map(|area| area_card(area.clone(), &devices, &floors, editing, draft, trouble, live))
                        .collect_view()
                        .into_any();
                }
                // By floor, lowest first, then the areas that aren't on one.
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
                    .map(|(heading, areas)| {
                        let areas = if areas.is_empty() {
                            view! { <p class="muted small">"No areas on this floor yet."</p> }.into_any()
                        } else {
                            areas
                                .into_iter()
                                .map(|area| area_card(area, &devices, &floors, editing, draft, trouble, live))
                                .collect_view()
                                .into_any()
                        };
                        view! {
                            <h2 class="floor-heading">{heading}</h2>
                            {areas}
                        }
                    })
                    .collect_view()
                    .into_any()
            }}

            {move || {
                let (_, devices, _) = shape.get();
                let unplaced: Vec<Device> = devices
                    .iter()
                    .filter(|device| device.area_id.is_none())
                    .cloned()
                    .collect();
                (!unplaced.is_empty())
                    .then(|| {
                        view! {
                            <section class="card room">
                                <div class="room-head">
                                    <h2>"Not in an area"</h2>
                                    <span class="muted">{count(unplaced.len())}</span>
                                </div>
                                <ul class="room-devices">
                                    {unplaced.into_iter().map(in_area).collect_view()}
                                </ul>
                            </section>
                        }
                    })
            }}
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

/// One area: its name, what's in it, and the two things you can do to it.
#[allow(clippy::too_many_arguments)]
fn area_card(
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
                        "Nothing in here yet. A device's own page is where you put it in an area."
                    </p>
                }
                .into_any()
            } else {
                view! {
                    <ul class="room-devices">
                        {devices.into_iter().map(in_area).collect_view()}
                    </ul>
                }
                .into_any()
            }}
        </section>
    }
    .into_any()
}

/// Which floor an area is on. Only offered once a floor exists.
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

/// One floor: its name and level, with the two things you can do to it. Editing swaps the row
/// for a form, the same way an area renames. A rename or a level change keeps the areas on it.
fn floor_row(
    floor: irori_types::Floor,
    editing: RwSignal<Option<irori_types::FloorId>>,
    draft_name: RwSignal<String>,
    draft_level: RwSignal<String>,
    trouble: RwSignal<Option<String>>,
    live: crate::Live,
) -> AnyView {
    let name = floor.name.to_string();
    let level = floor.level;
    let id = floor.id.clone();
    let being_edited = {
        let id = id.clone();
        move || editing.get().as_ref() == Some(&id)
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
        let level = level;
        move |_| {
            draft_name.set(name.clone());
            draft_level.set(level.to_string());
            editing.set(Some(id.clone()));
        }
    };
    let remove = {
        let id = id.clone();
        move |_| {
            let id = id.clone();
            spawn_local(async move {
                match api::remove_floor(&id).await {
                    Ok(()) => crate::refresh(live),
                    Err(why) => trouble.set(Some(why)),
                }
            });
        }
    };

    view! {
        <li>
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
                    view! {
                        <span class="name">{name.clone()}</span>
                        <span class="muted small">{format!("level {level}")}</span>
                        <button type="button" class="link" on:click=start.clone()>"Edit"</button>
                        <button type="button" class="link" on:click=remove.clone()>"Remove"</button>
                    }
                    .into_any()
                }
            }}
        </li>
    }
    .into_any()
}

fn in_area(device: Device) -> AnyView {
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
