//! The System row of Settings: the instance, and the machine it runs on.
//!
//! What's fixed — the version, the kernel, the processor — is a list of facts. What moves —
//! memory, disk, the processor's load, its temperature — is a meter each: how full it is at a
//! glance, and, opened, what is using it and how the last day went. The day comes from the
//! readings Irori already keeps of itself (the built-in "Irori" device), so a meter's history
//! is the same one its sensor has on the Devices page.
//!
//! The machine is asked when the row opens and every few seconds while it stays open, and not
//! at all while it's folded: a Settings check that cached could shrug at a disk that filled
//! since the last look.

use std::time::Duration;

use irori_types::{Entity, EntityId, SensorValue, State};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{self, Disk, Health, Process, System, Usage};
use crate::history::Histories;
use crate::icons::{Icon, icon};

/// How often an open row asks the machine again.
const EVERY: Duration = Duration::from_secs(10);

/// How full is full enough to say so.
const NEARLY_FULL: f64 = 0.85;

/// How the instance stands, for the heading of the row: which version, and for how long.
pub fn state(health: Option<&Health>) -> String {
    match health {
        None => "asking…".to_owned(),
        Some(health) => format!("v{} · up {}", health.version, uptime(health.uptime_ms)),
    }
}

#[component]
pub fn Panel(#[prop(into)] open: Signal<bool>) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let system = RwSignal::new(None::<Result<System, String>>);
    // What is using it: asked for only while a meter is open to show it, since finding out
    // means watching every process for a moment. `None` until the first answer; an older core,
    // which can't say, leaves it there.
    let usage = RwSignal::new(None::<Usage>);
    let looking = RwSignal::new(0u32);
    let ask_usage = move || {
        if !open.get_untracked() || looking.get_untracked() == 0 {
            return;
        }
        spawn_local(async move {
            if let Ok(answer) = api::fetch_usage().await
                && usage.with_untracked(|shown| shown.as_ref() != Some(&answer))
            {
                usage.set(Some(answer));
            }
        });
    };
    let ask = move || {
        if !open.get_untracked() {
            return;
        }
        spawn_local(async move {
            let answer = api::fetch_system().await;
            // The same answer again is nothing to draw again.
            if system.with_untracked(|shown| shown.as_ref() != Some(&answer)) {
                system.set(Some(answer));
            }
        });
        ask_usage();
    };
    Effect::new(move |_| {
        open.track();
        ask();
    });
    // The first meter to open asks straight away rather than waiting for the next round.
    Effect::new(move |_| {
        looking.track();
        ask_usage();
    });
    let handle = set_interval_with_handle(ask, EVERY).ok();
    on_cleanup(move || {
        if let Some(handle) = handle {
            handle.clear();
        }
    });

    // The machine as last heard from, kept while a new answer is on its way and through one
    // that failed, so the meters hold still rather than blink.
    let machine = Memo::new(
        move |previous: Option<&Option<System>>| match system.get() {
            Some(Ok(machine)) => Some(machine),
            _ => previous.cloned().flatten(),
        },
    );
    let histories = Histories::new();

    view! {
        <div class="machine">
            {move || match system.get() {
                Some(Err(why)) => Some(view! { <p class="why">{why}</p> }),
                _ => None,
            }}
            {move || facts(live.health.get(), machine.get())}
            <div class="meters">
                {meter(
                    Meter {
                        icon: Icon::Memory,
                        name: "Memory",
                        sensor: "sensor.irori_memory",
                    },
                    histories,
                    looking,
                    Signal::derive(move || {
                        machine.with(|machine| {
                            machine.as_ref().and_then(|m| share(m.memory_used, m.memory_total))
                        })
                    }),
                    Signal::derive(move || {
                        machine.with(|machine| {
                            machine.as_ref().map(|m| {
                                format!("{} of {}", bytes(m.memory_used), bytes(m.memory_total))
                            })
                        })
                    }),
                    move || machine.get().map(|machine| memory_breakdown(&machine, usage.get().as_ref())),
                )}
                {meter(
                    Meter {
                        icon: Icon::Disk,
                        name: "Disk",
                        sensor: "sensor.irori_disk",
                    },
                    histories,
                    looking,
                    Signal::derive(move || {
                        machine.with(|machine| {
                            machine.as_ref().and_then(|m| share(m.disk.used, m.disk.total))
                        })
                    }),
                    Signal::derive(move || {
                        machine.with(|machine| {
                            machine.as_ref().map(|m| {
                                if m.disk.total > 0 {
                                    format!("{} free", bytes(m.disk.available))
                                } else {
                                    "The volume with the data didn't answer.".to_owned()
                                }
                            })
                        })
                    }),
                    move || machine.get().map(|machine| disk_breakdown(&machine, usage.get().as_ref())),
                )}
                {meter(
                    Meter {
                        icon: Icon::Processor,
                        name: "Processor",
                        sensor: "sensor.irori_cpu",
                    },
                    histories,
                    looking,
                    Signal::derive(move || reading(live, "sensor.irori_cpu").map(|n| n / 100.0)),
                    Signal::derive(move || {
                        machine.with(|machine| {
                            machine.as_ref().filter(|m| m.cpu_cores > 0).map(cores)
                        })
                    }),
                    move || machine.get().map(|machine| processor_breakdown(&machine, usage.get().as_ref())),
                )}
                // Only on a machine that says how warm it is: a Raspberry Pi does, a Mac doesn't.
                {move || {
                    let has = live.home.with(|home| {
                        home.entities
                            .iter()
                            .any(|entity| entity.id.as_str() == "sensor.irori_temperature")
                    });
                    has.then(|| {
                        meter(
                            Meter {
                                icon: Icon::Temperature,
                                name: "Temperature",
                                sensor: "sensor.irori_temperature",
                            },
                            histories,
                            looking,
                            // Of the 85 °C a Pi starts holding itself back at.
                            Signal::derive(move || {
                                reading(live, "sensor.irori_temperature").map(|c| c / 85.0)
                            }),
                            Signal::derive(move || {
                                reading(live, "sensor.irori_temperature")
                                    .map(|celsius| format!("{celsius:.1} °C"))
                            }),
                            || None::<AnyView>,
                        )
                    })
                }}
            </div>
        </div>
    }
}

/// One of the things that fills up.
#[derive(Clone, Copy)]
struct Meter {
    icon: Icon,
    name: &'static str,
    /// The reading Irori keeps of it, whose last day is the meter's history.
    sensor: &'static str,
}

/// A meter: a bar that fills, a figure, a few words — and, opened, what's behind the figure.
fn meter<Breakdown>(
    meter: Meter,
    histories: Histories,
    // How many meters are open, which is whether anybody is looking at what's using the machine.
    looking: RwSignal<u32>,
    // How full, from 0 to 1. `None` until the machine has said.
    full: Signal<Option<f64>>,
    words: Signal<Option<String>>,
    breakdown: impl Fn() -> Option<Breakdown> + Send + Sync + 'static,
) -> AnyView
where
    Breakdown: IntoView + 'static,
{
    let live = expect_context::<crate::Live>();
    let open = RwSignal::new(false);
    let id: Option<EntityId> = meter.sensor.parse().ok();
    // The sensor itself, not what it's reading: the chart is drawn once and grows on its own.
    let sensor: Memo<Option<Entity>> = Memo::new(move |_| {
        live.home.with(|home| {
            home.entities
                .iter()
                .find(|entity| entity.id.as_str() == meter.sensor)
                .cloned()
        })
    });
    let toggle = {
        let id = id.clone();
        move |_| {
            // The day is asked for the first time the meter opens, and kept.
            if !open.get_untracked()
                && sensor.with_untracked(Option::is_some)
                && let Some(id) = &id
            {
                histories.ask(id);
            }
            let opening = !open.get_untracked();
            looking.update(|looking| {
                *looking = if opening {
                    *looking + 1
                } else {
                    looking.saturating_sub(1)
                }
            });
            open.set(opening);
        }
    };
    let now = {
        let id = id.clone();
        Signal::derive(move || {
            let id = id.as_ref()?;
            live.home.with(|home| {
                home.states
                    .iter()
                    .find(|state| &state.entity_id == id)
                    .cloned()
            })
        })
    };
    let chart = move || {
        let entity = sensor.get()?;
        let day = histories.day(&entity.id);
        Some(crate::device::history_panel(entity, day, now))
    };
    let percent = move || {
        full.get()
            .map(|full| format!("{:.0}%", full.clamp(0.0, 1.0) * 100.0))
    };
    view! {
        <div class="meter" class:open=move || open.get()>
            <button
                type="button"
                class="meter-head"
                aria-expanded=move || open.get().to_string()
                on:click=toggle
            >
                <span class="meter-icon">{icon(meter.icon)}</span>
                <span class="meter-name">{meter.name}</span>
                <span class="meter-figure">{move || percent().unwrap_or_else(|| "—".to_owned())}</span>
                <span
                    class="meter-bar"
                    class:high=move || full.get().is_some_and(|full| full >= NEARLY_FULL)
                    role="meter"
                    aria-label=meter.name
                    aria-valuemin="0"
                    aria-valuemax="100"
                    aria-valuenow=move || full.get().map(|full| format!("{:.0}", full * 100.0))
                >
                    <span
                        class="meter-fill"
                        style=move || format!("scale: {:.4} 1", full.get().unwrap_or(0.0).clamp(0.0, 1.0))
                    ></span>
                </span>
                <span class="meter-words">{move || words.get()}</span>
                <span class="unroll-mark" aria-hidden="true"></span>
            </button>
            <div class="drawer" class:open=move || open.get() inert=move || (!open.get()).then_some("")>
                <div class="drawer-inner">
                    <div class="meter-more">
                        {move || breakdown()}
                        {chart}
                    </div>
                </div>
            </div>
        </div>
    }
    .into_any()
}

/// Where the memory is: in use, still free, how much of it is Irori's own, and what has been
/// pushed out to disk.
fn memory_breakdown(machine: &System, usage: Option<&Usage>) -> AnyView {
    let free = machine.memory_total.saturating_sub(machine.memory_used);
    view! {
        <dl class="facts">
            <dt>"In use"</dt>
            <dd>{bytes(machine.memory_used)}</dd>
            <dt>"Free"</dt>
            <dd>{bytes(free)}</dd>
            {(machine.process_memory > 0).then(|| view! {
                <dt>"Irori itself"</dt>
                <dd>{bytes(machine.process_memory)}</dd>
            })}
            {(machine.swap_total > 0).then(|| view! {
                <dt>"Swap"</dt>
                <dd>
                    {format!("{} of {}", bytes(machine.swap_used), bytes(machine.swap_total))}
                </dd>
            })}
        </dl>
        {using(
            "Using the most",
            usage.map(|usage| {
                let mut processes: Vec<&Process> = usage.processes.iter().collect();
                processes.sort_by_key(|process| std::cmp::Reverse(process.memory));
                processes
                    .into_iter()
                    .filter(|process| process.memory > 0)
                    .take(ROWS)
                    .map(|process| Row {
                        name: process.name.clone(),
                        own: process.own,
                        part: share(process.memory, machine.memory_total).unwrap_or(0.0),
                        words: bytes(process.memory),
                    })
                    .collect()
            }),
            "Nothing is holding any memory worth a row.",
        )}
    }
    .into_any()
}

/// Every volume with how full it is, and how much of the data's volume is Irori's database.
fn disk_breakdown(machine: &System, usage: Option<&Usage>) -> AnyView {
    // An older core names only the data's volume.
    let volumes: Vec<Disk> = if machine.disks.is_empty() {
        vec![machine.disk.clone()]
    } else {
        machine.disks.clone()
    };
    let data = machine.disk.mount.clone();
    view! {
        <ul class="volumes">
            {volumes
                .into_iter()
                .filter(|volume| volume.total > 0)
                .map(|volume| {
                    let full = share(volume.used, volume.total).unwrap_or(0.0);
                    let holds_data = volume.mount == data;
                    let high = full >= NEARLY_FULL;
                    view! {
                        <li>
                            <span class="volume-mount">
                                {volume.mount.clone()}
                                {holds_data.then(|| view! { <span class="chip quiet">"data"</span> })}
                            </span>
                            <span class="meter-bar" class:high=high>
                                <span class="meter-fill" style=format!("scale: {full:.4} 1")></span>
                            </span>
                            <span class="volume-words">
                                {format!("{} of {}", bytes(volume.used), bytes(volume.total))}
                            </span>
                        </li>
                    }
                })
                .collect_view()}
        </ul>
        {(machine.database_bytes > 0 || !machine.data_dir.is_empty()).then(|| view! {
            <dl class="facts">
                {(machine.database_bytes > 0).then(|| view! {
                    <dt>"Database"</dt>
                    <dd>{bytes(machine.database_bytes)}</dd>
                })}
                {(!machine.data_dir.is_empty()).then(|| view! {
                    <dt>"Kept in"</dt>
                    <dd>{machine.data_dir.clone()}</dd>
                })}
            </dl>
        })}
        {using(
            "What Irori keeps there",
            usage.map(|usage| {
                usage
                    .storage
                    .iter()
                    .take(ROWS + 1)
                    .map(|stored| Row {
                        name: stored.name.clone(),
                        own: false,
                        // Against the biggest, so the list reads as a comparison: against the
                        // whole disk, everything Irori keeps is a sliver.
                        part: share(
                            stored.bytes,
                            usage.storage.iter().map(|s| s.bytes).max().unwrap_or(0),
                        )
                        .unwrap_or(0.0),
                        words: bytes(stored.bytes),
                    })
                    .collect()
            }),
            "Nothing yet.",
        )}
    }
    .into_any()
}

/// How much is being asked of the processor: the load, against the cores there are to carry it.
fn processor_breakdown(machine: &System, usage: Option<&Usage>) -> AnyView {
    let [one, five, fifteen] = machine.load_average;
    let has_load = one > 0.0 || five > 0.0 || fifteen > 0.0;
    view! {
        <dl class="facts">
            {(!machine.cpu.is_empty()).then(|| view! {
                <dt>"Processor"</dt>
                <dd>{machine.cpu.clone()}</dd>
            })}
            {has_load.then(|| view! {
                <dt>"Load"</dt>
                <dd>
                    {format!("{one:.2} · {five:.2} · {fifteen:.2}")}
                    <span class="muted">" over 1, 5 and 15 minutes"</span>
                </dd>
            })}
        </dl>
        {using(
            "Busiest just now",
            usage.map(|usage| {
                let mut processes: Vec<&Process> = usage.processes.iter().collect();
                processes.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
                processes
                    .into_iter()
                    .filter(|process| process.cpu >= 0.1)
                    .take(ROWS)
                    .map(|process| Row {
                        name: process.name.clone(),
                        own: process.own,
                        part: f64::from(process.cpu / 100.0).clamp(0.0, 1.0),
                        words: format!("{:.1}%", process.cpu),
                    })
                    .collect()
            }),
            "Nothing is busy right now.",
        )}
    }
    .into_any()
}

/// How many rows of what's using something are worth showing.
const ROWS: usize = 8;

/// One thing using some of what a meter measures.
struct Row {
    name: String,
    /// Irori itself, which gets a word saying so.
    own: bool,
    /// How much of it, 0 to 1, for the bar.
    part: f64,
    words: String,
}

/// What is using it, biggest first: a name, a bar, a figure. `None` is not having been told
/// yet, which reads as looking rather than as nothing.
fn using(title: &'static str, rows: Option<Vec<Row>>, empty: &'static str) -> AnyView {
    view! {
        <div class="using">
            <h4 class="using-title">{title}</h4>
            {match rows {
                None => view! { <p class="muted small">"Looking…"</p> }.into_any(),
                Some(rows) if rows.is_empty() => view! { <p class="muted small">{empty}</p> }.into_any(),
                Some(rows) => view! {
                    <ul class="volumes">
                        {rows
                            .into_iter()
                            .map(|row| view! {
                                <li>
                                    <span class="volume-mount">
                                        {row.name}
                                        {row.own.then(|| view! { <span class="chip quiet">"Irori"</span> })}
                                    </span>
                                    <span class="meter-bar">
                                        <span class="meter-fill" style=format!("scale: {:.4} 1", row.part)></span>
                                    </span>
                                    <span class="volume-words">{row.words}</span>
                                </li>
                            })
                            .collect_view()}
                    </ul>
                }
                .into_any(),
            }}
        </div>
    }
    .into_any()
}

/// What doesn't move: the instance as built, and the machine as it is.
fn facts(health: Option<Health>, machine: Option<System>) -> AnyView {
    view! {
        <dl class="facts">
            {health.map(|health| view! {
                <dt>"Version"</dt>
                <dd>
                    {health.version.clone()}
                    {(!health.commit.is_empty()).then(|| format!(" · {}", health.commit))}
                </dd>
                {(!health.built_at.is_empty()).then(|| view! {
                    <dt>"Built"</dt>
                    <dd>{health.built_at.clone()}</dd>
                })}
                <dt>"Built with"</dt>
                <dd>
                    {if health.features.is_empty() {
                        "nothing optional (barebones)".to_owned()
                    } else {
                        health.features.join(", ")
                    }}
                </dd>
                <dt>"Database"</dt>
                <dd>
                    {format!(
                        "SQLite {} · {}",
                        health.sqlite.version,
                        health.sqlite.journal_mode.to_uppercase(),
                    )}
                </dd>
                <dt>"Irori up"</dt>
                <dd>{uptime(health.uptime_ms)}</dd>
            })}
            {machine.map(|machine| view! {
                <dt>"Machine up"</dt>
                <dd>{uptime(u128::from(machine.uptime_secs) * 1000)}</dd>
                {machine.host.clone().map(|host| view! {
                    <dt>"Host"</dt>
                    <dd>{host}</dd>
                })}
                <dt>"Operating system"</dt>
                <dd>
                    {machine.os.clone()}
                    {(!machine.os_version.is_empty()).then(|| format!(" {}", machine.os_version))}
                </dd>
                {(!machine.kernel.is_empty()).then(|| view! {
                    <dt>"Kernel"</dt>
                    <dd>{machine.kernel.clone()}</dd>
                })}
                <dt>"Architecture"</dt>
                <dd>{machine.arch.clone()}</dd>
            })}
        </dl>
    }
    .into_any()
}

/// What a sensor of Irori's own is reading, as a number.
fn reading(live: crate::Live, sensor: &str) -> Option<f64> {
    live.home.with(|home| {
        let state = home
            .states
            .iter()
            .find(|state| state.entity_id.as_str() == sensor)?;
        match state.state.as_ref()? {
            State::Sensor(sensor) => match sensor.value {
                SensorValue::Number(n) => Some(n),
                SensorValue::Text(_) => None,
            },
            _ => None,
        }
    })
}

/// `used` of `total`, from 0 to 1. Nothing when there's no total to be a share of.
fn share(used: u64, total: u64) -> Option<f64> {
    (total > 0).then(|| (used as f64 / total as f64).clamp(0.0, 1.0))
}

/// The cores, and the threads the OS runs on them where that's more.
fn cores(machine: &System) -> String {
    let cores = machine.cpu_cores;
    let said = format!("{cores} core{}", if cores == 1 { "" } else { "s" });
    if machine.cpu_logical > cores {
        format!("{said}, {} threads", machine.cpu_logical)
    } else {
        said
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
    fn sizes_read_as_a_person_would_say_them() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1_024), "1.0 KB");
        assert_eq!(bytes(1_500_000_000), "1.4 GB");
    }

    #[test]
    fn a_share_is_of_something() {
        assert_eq!(share(1, 0), None);
        assert_eq!(share(1, 4), Some(0.25));
        assert_eq!(share(9, 4), Some(1.0));
    }

    #[test]
    fn the_row_says_which_version_and_for_how_long() {
        assert_eq!(state(None), "asking…");
    }
}
