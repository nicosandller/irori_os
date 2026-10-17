//! The Backups row of Settings (`docs/specs/backup.md`): make one, send one, put one back, and
//! say when and how they are made.
//!
//! A backup holds the whole home, so the row says what each choice puts at risk in plain words,
//! shows the recovery key only when asked, and asks again before anything is replaced.

use std::collections::BTreeMap;

use leptos::prelude::*;
use leptos::task::spawn_local;
use web_sys::js_sys::Date;

use crate::api::{self, BackupEntry, BackupOverview, BackupParts, BackupSettings, BackupVerified};

/// How long the copy button says it copied.
const COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1600);

const WEEKDAYS: [(&str, &str); 7] = [
    ("mon", "Mon"),
    ("tue", "Tue"),
    ("wed", "Wed"),
    ("thu", "Thu"),
    ("fri", "Fri"),
    ("sat", "Sat"),
    ("sun", "Sun"),
];

/// The overview as last fetched. Kept above the row so its folded state can say how it stands.
#[derive(Debug, Clone, Copy)]
pub struct Backups(pub RwSignal<Option<BackupOverview>>);

/// How the row stands while it's folded.
pub fn summary(overview: Option<&BackupOverview>) -> String {
    let Some(overview) = overview else {
        return String::new();
    };
    if let Some(done) = &overview.last_restore
        && !done.ok
    {
        return "the last restore didn't happen".to_owned();
    }
    let newest = overview
        .backups
        .iter()
        .find(|b| !b.name.starts_with("pre-restore-"));
    let Some(newest) = newest else {
        return if overview.settings.enabled {
            "none yet · scheduled".to_owned()
        } else {
            "none yet".to_owned()
        };
    };
    let mut said = format!("last {}", ago(&newest.modified));
    if overview.status.last_error.is_some() {
        said.push_str(" · the latest attempt failed");
    } else if !overview.settings.enabled {
        said.push_str(" · not scheduled");
    }
    if !newest.encrypted {
        said.push_str(" · not encrypted");
    }
    said
}

/// "just now", "5 minutes ago", "3 hours ago", "yesterday", "12 Mar".
pub fn ago(iso: &str) -> String {
    let then = Date::parse(iso);
    if then.is_nan() {
        return iso.to_owned();
    }
    let minutes = ((Date::now() - then) / 60_000.0).max(0.0);
    match minutes {
        m if m < 1.0 => "just now".to_owned(),
        m if m < 60.0 => format!(
            "{} minute{} ago",
            m as u32,
            if m as u32 == 1 { "" } else { "s" }
        ),
        m if m < 60.0 * 24.0 => {
            let hours = (m / 60.0) as u32;
            format!("{hours} hour{} ago", if hours == 1 { "" } else { "s" })
        }
        m if m < 60.0 * 48.0 => "yesterday".to_owned(),
        _ => day_month(then),
    }
}

fn day_month(ms: f64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let date = Date::new(&web_sys::wasm_bindgen::JsValue::from_f64(ms));
    format!(
        "{} {}",
        date.get_date(),
        MONTHS[(date.get_month() as usize).min(11)]
    )
}

/// "17 Oct, 13:11", in this browser's time.
pub fn when(iso: &str) -> String {
    let ms = Date::parse(iso);
    if ms.is_nan() {
        return iso.to_owned();
    }
    let date = Date::new(&web_sys::wasm_bindgen::JsValue::from_f64(ms));
    format!(
        "{}, {:02}:{:02}",
        day_month(ms),
        date.get_hours(),
        date.get_minutes()
    )
}

pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Fetches again, so the list and the row's summary follow what just happened.
fn refresh(overview: RwSignal<Option<BackupOverview>>) {
    spawn_local(async move {
        if let Ok(fresh) = api::fetch_backups().await {
            overview.set(Some(fresh));
        }
    });
}

#[component]
pub fn Section() -> impl IntoView {
    let Backups(overview) = expect_context::<Backups>();
    let crate::Session(session) = expect_context::<crate::Session>();
    let owner = move || session.with(|session| session.as_ref().is_none_or(|s| s.owner));
    let trouble = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let sending = RwSignal::new(false);

    let make = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        trouble.set(None);
        spawn_local(async move {
            if let Err(why) = api::make_backup().await {
                trouble.set(Some(why));
            }
            refresh(overview);
            busy.set(false);
        });
    };

    let picked_file = move |event: leptos::ev::Event| {
        use web_sys::wasm_bindgen::JsCast as _;
        let Some(input) = event
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
        else {
            return;
        };
        let Some(file) = input.files().and_then(|files| files.get(0)) else {
            return;
        };
        input.set_value("");
        sending.set(true);
        trouble.set(None);
        spawn_local(async move {
            if let Err(why) = api::upload_backup(file, false).await {
                trouble.set(Some(why));
            }
            refresh(overview);
            sending.set(false);
        });
    };

    view! {
        {move || overview.get().and_then(|o| o.last_restore).map(|done| view! { <Restored done=done /> })}
        {move || owner().then(|| view! {
            <div class="backup-now">
                <button type="button" class="press" disabled=move || busy.get() on:click=make>
                    {move || if busy.get() { "Backing up…" } else { "Back up now" }}
                </button>
                <label class="press backup-upload" class:disabled=move || sending.get()>
                    {move || if sending.get() { "Sending…" } else { "Send a backup here…" }}
                    <input type="file" hidden accept=".gz,.enc,application/gzip"
                        disabled=move || sending.get() on:change=picked_file />
                </label>
                <span class="muted small backup-state">
                    {move || overview.with(|o| o.as_ref().map(state_line))}
                </span>
            </div>
        })}
        {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
        <List owner=Signal::derive(owner) />
        {move || owner().then(|| view! {
            <Plan />
            <Seal />
        })}
    }
}

/// The one line beside the buttons: how the last backup went, and how much room is left.
fn state_line(overview: &BackupOverview) -> String {
    let mut parts = Vec::new();
    if let Some(error) = &overview.status.last_error {
        parts.push(format!("The last backup failed: {error}"));
    } else if let Some(at) = &overview.status.last_success {
        parts.push(format!("Last backup {}.", ago(at)));
    }
    if let Some(free) = overview.free_bytes {
        parts.push(format!("{} free here.", size(free)));
    }
    parts.join(" ")
}

#[component]
fn Restored(done: api::BackupRestored) -> impl IntoView {
    let Backups(overview) = expect_context::<Backups>();
    let dismiss = move |_| {
        spawn_local(async move {
            api::backup_restore_seen().await;
            refresh(overview);
        });
    };
    let ok = done.ok;
    view! {
        <div class="backup-restored" class:failed=!ok role="status">
            <p class="backup-restored-head">
                {if ok {
                    format!("Restored from {} {}.", done.archive, ago(&done.at))
                } else {
                    format!("The restore from {} did not happen.", done.archive)
                }}
            </p>
            {done.error.clone().map(|why| view! { <p class="why">{why}</p> })}
            {(ok && done.safety_copy.is_some()).then(|| view! {
                <p class="muted small">
                    "The home as it was is kept as "<code>{done.safety_copy.clone()}</code>
                    " in the list below."
                </p>
            })}
            {(!done.reinstalled.is_empty()).then(|| view! {
                <p class="muted small">"Extensions fetched again: "{done.reinstalled.join(", ")}"."</p>
            })}
            {(!done.awaiting_approval.is_empty()).then(|| view! {
                <p class="muted small">
                    "These need full access to this machine, so they wait for you to install \
                     them from the Extensions page: "{done.awaiting_approval.join(", ")}"."
                </p>
            })}
            {(!done.reinstall_failed.is_empty()).then(|| view! {
                <p class="why">"Couldn't fetch: "{done.reinstall_failed.join("; ")}</p>
            })}
            {done.warnings.iter().map(|w| view! { <p class="muted small">"Note: "{w.clone()}</p> }).collect_view()}
            <button type="button" class="press" on:click=dismiss>"Got it"</button>
        </div>
    }
}

// ----- the list -----

#[component]
fn List(owner: Signal<bool>) -> impl IntoView {
    let Backups(overview) = expect_context::<Backups>();
    // The backup the person has pressed Restore or Delete on.
    let restoring = RwSignal::new(None::<String>);
    let deleting = RwSignal::new(None::<String>);
    let trouble = RwSignal::new(None::<String>);

    view! {
        <ul class="tokens backups">
            {move || {
                let Some(o) = overview.get() else {
                    return view! { <li class="tokens-none muted">"Looking…"</li> }.into_any();
                };
                if o.backups.is_empty() {
                    return view! {
                        <li class="tokens-none muted">
                            "No backups yet. Back one up now, or turn on the schedule below."
                        </li>
                    }
                    .into_any();
                }
                o.backups
                    .into_iter()
                    .enumerate()
                    .map(|(place, entry)| {
                        view! { <Item entry=entry place=place owner=owner
                            restoring=restoring deleting=deleting trouble=trouble /> }
                    })
                    .collect_view()
                    .into_any()
            }}
        </ul>
        {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
    }
}

fn parts_default(entry: &BackupEntry) -> BackupParts {
    BackupParts {
        settings: true,
        extensions: true,
        state: true,
        history: entry.manifest.as_ref().is_none_or(|m| m.history),
    }
}

#[component]
fn Item(
    entry: BackupEntry,
    place: usize,
    owner: Signal<bool>,
    restoring: RwSignal<Option<String>>,
    deleting: RwSignal<Option<String>>,
    trouble: RwSignal<Option<String>>,
) -> impl IntoView {
    let Backups(overview) = expect_context::<Backups>();
    let name = entry.name.clone();
    let safety = name.starts_with("pre-restore-");
    let title = if safety {
        format!("Before a restore · {}", when(&entry.modified))
    } else {
        when(&entry.modified)
    };
    let version = entry.manifest.as_ref().map(|m| m.irori_version.clone());
    let has_history = entry.manifest.as_ref().is_none_or(|m| m.history);
    let href = format!("/api/backups/{name}");

    let (open_name, close_name) = (name.clone(), name.clone());
    let is_open = {
        let name = name.clone();
        move || restoring.get().as_deref() == Some(name.as_str())
    };
    let asking = {
        let name = name.clone();
        move || deleting.get().as_deref() == Some(name.as_str())
    };
    let delete = {
        let name = name.clone();
        move |_| {
            let name = name.clone();
            spawn_local(async move {
                match api::delete_backup(&name).await {
                    Ok(()) => trouble.set(None),
                    Err(why) => trouble.set(Some(why)),
                }
                deleting.set(None);
                refresh(overview);
            });
        }
    };
    let (ask_name, keep_name) = (name.clone(), name.clone());

    view! {
        <li class="token backup" style=format!("--place: {place}")>
            <span class="token-mark" class:connects=entry.encrypted aria-hidden="true">
                <svg viewBox="0 0 24 24" inner_html=if entry.encrypted { LOCK } else { OPEN_BOX }></svg>
            </span>
            <span class="token-words">
                <span class="token-name">{title}</span>
                <span class="token-may">
                    <span class="scope-chip">{size(entry.bytes)}</span>
                    <span class="scope-chip">{if entry.encrypted { "sealed" } else { "not encrypted" }}</span>
                    {version.map(|v| view! { <span class="scope-chip">"Irori "{v}</span> })}
                    {(!has_history).then(|| view! { <span class="scope-chip">"no history"</span> })}
                    {entry.copies.iter().map(|c| view! { <span class="scope-chip">"copy at "{c.clone()}</span> }).collect_view()}
                </span>
            </span>
            {move || owner.get().then(|| {
                let (ask_name, keep_name, delete, href, open_name, close_name) = (
                    ask_name.clone(), keep_name.clone(), delete.clone(), href.clone(),
                    open_name.clone(), close_name.clone(),
                );
                if asking() {
                    view! {
                        <span class="token-sure">
                            <span class="muted small">"It can't be got back."</span>
                            <button type="button" class="press danger" on:click=delete>"Delete"</button>
                            <button type="button" class="press"
                                on:click=move |_| {
                                    if deleting.get_untracked().as_deref() == Some(keep_name.as_str()) {
                                        deleting.set(None);
                                    }
                                }>"Keep"</button>
                        </span>
                    }.into_any()
                } else {
                    view! {
                        <span class="backup-actions">
                            <a class="press" href=href download>"Download"</a>
                            <button type="button" class="press"
                                on:click=move |_| {
                                    if restoring.get_untracked().as_deref() == Some(open_name.as_str()) {
                                        restoring.set(None);
                                    } else {
                                        restoring.set(Some(close_name.clone()));
                                    }
                                }>"Restore…"</button>
                            <button type="button" class="press"
                                on:click=move |_| deleting.set(Some(ask_name.clone()))>"Delete"</button>
                        </span>
                    }.into_any()
                }
            })}
            <div class="drawer backup-drawer" class:open=is_open.clone()
                inert={
                    let is_open = is_open.clone();
                    move || (!is_open()).then_some("")
                }>
                <div class="drawer-inner">
                    <RestoreForm entry=entry.clone() />
                </div>
            </div>
        </li>
    }
}

const LOCK: &str = r#"<rect x="5" y="11" width="14" height="9" rx="2" fill="none" stroke="currentColor" stroke-width="1.8"/><path d="M8 11V8a4 4 0 0 1 8 0v3" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>"#;
const OPEN_BOX: &str = r#"<path d="M4 13v6h16v-6M12 4v10M8 10.5l4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>"#;

#[component]
fn RestoreForm(entry: BackupEntry) -> impl IntoView {
    let key = RwSignal::new(String::new());
    let parts = RwSignal::new(parts_default(&entry));
    let checked = RwSignal::new(None::<BackupVerified>);
    let trouble = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let restarting = RwSignal::new(false);
    let live = expect_context::<crate::Live>();
    // The boot the restart set off from; the page reloads when a different one answers.
    let from = RwSignal::new(None::<String>);
    Effect::new(move |_| {
        let (Some(from), Some(health)) = (from.get(), live.health.get()) else {
            return;
        };
        if health.boot_id != from {
            let _ = window().location().reload();
        }
    });
    let sealed = entry.encrypted;
    let has_history = entry.manifest.as_ref().is_none_or(|m| m.history);
    let name = entry.name.clone();
    let (check_name, restore_name) = (name.clone(), name);

    let check = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        trouble.set(None);
        let (name, key) = (check_name.clone(), key.get_untracked());
        spawn_local(async move {
            match api::verify_backup(&name, &key).await {
                Ok(verified) => checked.set(Some(verified)),
                Err(why) => {
                    checked.set(None);
                    trouble.set(Some(why));
                }
            }
            busy.set(false);
        });
    };
    let restore = move |_| {
        if busy.get_untracked() {
            return;
        }
        let chosen = parts.get_untracked();
        if !(chosen.settings || chosen.extensions || chosen.state || chosen.history) {
            trouble.set(Some("Choose something to put back.".to_owned()));
            return;
        }
        let Some(boot) = live.health.get_untracked().map(|h| h.boot_id) else {
            return;
        };
        busy.set(true);
        trouble.set(None);
        let (name, key) = (restore_name.clone(), key.get_untracked());
        spawn_local(async move {
            match api::restore_backup(&name, &key, chosen).await {
                Ok(_) => {
                    restarting.set(true);
                    from.set(Some(boot));
                }
                Err(why) => {
                    trouble.set(Some(why));
                    busy.set(false);
                }
            }
        });
    };

    let toggle = |pick: fn(&mut BackupParts) -> &mut bool| {
        move |_| {
            parts.update(|p| {
                let on = pick(p);
                *on = !*on;
            });
            checked.set(None);
        }
    };
    let pick = |label: &'static str,
                detail: &'static str,
                read: fn(&BackupParts) -> bool,
                flip: fn(&mut BackupParts) -> &mut bool,
                enabled: bool| {
        let on = move || parts.with(read);
        view! {
            <button type="button" class="scope-pick" class:on=on disabled=!enabled
                aria-pressed=move || on().to_string() on:click=toggle(flip)>
                <svg class="scope-tick" viewBox="0 0 24 24" aria-hidden="true">
                    <rect x="3.5" y="3.5" width="17" height="17" rx="4.5" />
                    <path d="M7.5 12.5l3 3 6-6.5" pathLength="1" />
                </svg>
                <span class="scope-words">
                    <span>{label}</span>
                    <span class="muted small">{detail}</span>
                </span>
            </button>
        }
    };

    view! {
        <div class="backup-restore">
            {sealed.then(|| view! {
                <label class="settings-field">
                    "Recovery key"
                    <input type="text" autocomplete="off" spellcheck="false"
                        placeholder="XXXXX-XXXXX-XXXXX-XXXXX-XXXXX"
                        prop:value=move || key.get()
                        on:input=move |e| { key.set(event_target_value(&e)); checked.set(None); } />
                    <span class="muted small">
                        "This backup is sealed. Leave this empty to use the key this Irori has on \
                         file, which opens anything it made; type the key if it came from elsewhere."
                    </span>
                </label>
            })}
            <div class="scope-picks" role="group" aria-label="What to put back">
                {pick("Settings", "Rooms, names, automations, people and their passwords, the https certificate.",
                    |p| p.settings, |p| &mut p.settings, true)}
                {pick("What extensions remember", "Pairings and keys, like the Zigbee network.",
                    |p| p.extensions, |p| &mut p.extensions, true)}
                {pick("Sign-ins and chats", "Who is signed in, access tokens, the assistant's chats, what extensions keep.",
                    |p| p.state, |p| &mut p.state, true)}
                {pick(
                    "History",
                    if has_history { "Readings and changes. Leave this off to keep the history Irori has now." }
                    else { "This backup was made without history." },
                    |p| p.history, |p| &mut p.history, has_history,
                )}
            </div>
            {move || checked.get().map(|v| view! {
                <div class="backup-checked" role="status">
                    <p>
                        "It opens: made by Irori "{v.manifest.irori_version.clone()}" on "
                        {when(&v.manifest.created_at)}", "{v.files}" files."
                    </p>
                    {v.warnings.iter().map(|w| view! { <p class="muted small">"Note: "{w.clone()}</p> }).collect_view()}
                </div>
            })}
            {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
            <p class="muted small">
                "Irori keeps the home as it is now (you'll find it in this list), restarts, and puts \
                 this back. If the backup has different people or passwords, you sign in again with those."
            </p>
            <div class="settings-form-actions">
                <button type="button" disabled=move || busy.get() on:click=check>"Check it"</button>
                <button type="button" class="add" disabled=move || busy.get() on:click=restore>
                    {move || if restarting.get() { "Restarting…" } else { "Restore and restart" }}
                </button>
            </div>
        </div>
    }
}

// ----- the schedule -----

#[component]
fn Plan() -> impl IntoView {
    let Backups(overview) = expect_context::<Backups>();
    let draft = RwSignal::new(None::<BackupSettings>);
    let trouble = RwSignal::new(None::<String>);
    let saving = RwSignal::new(false);
    let done = RwSignal::new(false);
    // The draft starts as what the home says, once, and not again while it is being edited.
    Effect::new(move |_| {
        let saved = overview.with(|o| o.as_ref().map(|o| o.settings.clone()));
        if draft.get_untracked().is_none() {
            draft.set(saved);
        }
    });
    let changed = move || {
        overview.with(|o| {
            draft.with(|d| match (o, d) {
                (Some(o), Some(d)) => !same(&o.settings, d),
                _ => false,
            })
        })
    };
    let edit = move |change: Box<dyn FnOnce(&mut BackupSettings)>| {
        done.set(false);
        draft.update(|d| {
            if let Some(d) = d {
                change(d);
            }
        });
    };
    let save = move |_| {
        let Some(settings) = draft.get_untracked() else {
            return;
        };
        if saving.get_untracked() || !changed() {
            return;
        }
        saving.set(true);
        trouble.set(None);
        spawn_local(async move {
            match api::save_backup_settings(&settings, &BTreeMap::new()).await {
                Ok(fresh) => {
                    draft.set(Some(fresh.settings.clone()));
                    overview.set(Some(fresh));
                    done.set(true);
                    gloo_timers::future::TimeoutFuture::new(1800).await;
                    let _ = done.try_set(false);
                }
                Err(why) => trouble.set(Some(why)),
            }
            let _ = saving.try_set(false);
        });
    };

    view! {
        <div class="backup-block">
            <h4 class="backup-heading">"Automatic backups"</h4>
            {move || draft.get().map(|d| {
                let enabled = d.enabled;
                view! {
                    <button type="button" class="scope-pick" class:on=enabled aria-pressed=enabled.to_string()
                        on:click=move |_| edit(Box::new(|d| d.enabled = !d.enabled))>
                        <svg class="scope-tick" viewBox="0 0 24 24" aria-hidden="true">
                            <rect x="3.5" y="3.5" width="17" height="17" rx="4.5" />
                            <path d="M7.5 12.5l3 3 6-6.5" pathLength="1" />
                        </svg>
                        <span class="scope-words">
                            <span>"Back up on a schedule"</span>
                            <span class="muted small">
                                "The oldest are deleted as new ones are made. A machine that was off at the \
                                 time makes one when it comes back."
                            </span>
                        </span>
                    </button>
                    <div class="backup-schedule" class:muted=!enabled>
                        <label class="settings-field backup-time">
                            "At"
                            <input type="time" prop:value=d.at.clone()
                                on:input=move |e| { let v = event_target_value(&e); edit(Box::new(move |d| d.at = v)); } />
                        </label>
                        <div class="settings-field">
                            <span>"On"</span>
                            <div class="backup-days" role="group" aria-label="Days">
                                {WEEKDAYS.into_iter().map(|(id, label)| {
                                    let on = d.days.is_empty() || d.days.iter().any(|x| x == id);
                                    view! {
                                        <button type="button" class="scope-chip day" class:on=on aria-pressed=on.to_string()
                                            on:click=move |_| edit(Box::new(move |d| toggle_day(d, id)))>
                                            {label}
                                        </button>
                                    }
                                }).collect_view()}
                            </div>
                        </div>
                        <label class="settings-field backup-keep">
                            "Keep the newest"
                            <input type="number" min="1" inputmode="numeric" prop:value=d.keep.to_string()
                                on:input=move |e| {
                                    if let Ok(n) = event_target_value(&e).parse::<u32>() {
                                        edit(Box::new(move |d| d.keep = n.max(1)));
                                    }
                                } />
                        </label>
                        <label class="settings-field backup-keep">
                            "And none older than (days, optional)"
                            <input type="number" min="1" inputmode="numeric" placeholder="no limit"
                                prop:value=d.keep_days.map(|n| n.to_string()).unwrap_or_default()
                                on:input=move |e| {
                                    let v = event_target_value(&e).parse::<u32>().ok().filter(|n| *n > 0);
                                    edit(Box::new(move |d| d.keep_days = v));
                                } />
                        </label>
                    </div>
                    <button type="button" class="scope-pick" class:on=d.include_history
                        aria-pressed=d.include_history.to_string()
                        on:click=move |_| edit(Box::new(|d| d.include_history = !d.include_history))>
                        <svg class="scope-tick" viewBox="0 0 24 24" aria-hidden="true">
                            <rect x="3.5" y="3.5" width="17" height="17" rx="4.5" />
                            <path d="M7.5 12.5l3 3 6-6.5" pathLength="1" />
                        </svg>
                        <span class="scope-words">
                            <span>"Include history"</span>
                            <span class="muted small">
                                "Every reading and change Irori has kept. Without it a backup is much \
                                 smaller, and a restore starts the history again."
                            </span>
                        </span>
                    </button>
                }
            })}
            {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
            <div class="place-actions">
                <span class="muted small" class:history-saved=move || done.get()
                    class:history-unsaved=move || !done.get() && changed()>
                    {move || if done.get() { "Saved." } else if changed() { "Not saved yet." } else { "" }}
                </span>
                <button type="button" class="add-one" class:busy=move || saving.get()
                    disabled=move || saving.get() || !changed() on:click=save>
                    <span class="add-one-label">"Save"</span>
                </button>
            </div>
        </div>
    }
}

fn toggle_day(settings: &mut BackupSettings, id: &str) {
    // Empty means every day; the first click on one of seven narrows it to the other six.
    if settings.days.is_empty() {
        settings.days = WEEKDAYS.iter().map(|(d, _)| (*d).to_owned()).collect();
    }
    if let Some(at) = settings.days.iter().position(|d| d == id) {
        // Never none at all: that would be a schedule that never runs.
        if settings.days.len() > 1 {
            settings.days.remove(at);
        }
    } else {
        settings.days.push(id.to_owned());
    }
    if settings.days.len() == WEEKDAYS.len() {
        settings.days.clear();
    }
}

fn same(a: &BackupSettings, b: &BackupSettings) -> bool {
    a.enabled == b.enabled
        && a.encrypt == b.encrypt
        && a.at == b.at
        && a.days == b.days
        && a.keep == b.keep
        && a.keep_days == b.keep_days
        && a.include_history == b.include_history
        && a.destination == b.destination
}

// ----- the key -----

#[component]
fn Seal() -> impl IntoView {
    let Backups(overview) = expect_context::<Backups>();
    let shown = RwSignal::new(None::<String>);
    let copied = RwSignal::new(None::<bool>);
    let trouble = RwSignal::new(None::<String>);
    let renewing = RwSignal::new(false);
    let encrypt = move || overview.with(|o| o.as_ref().is_none_or(|o| o.settings.encrypt));

    let set_encrypt = move |on: bool| {
        let Some(mut settings) = overview.with(|o| o.as_ref().map(|o| o.settings.clone())) else {
            return;
        };
        settings.encrypt = on;
        spawn_local(async move {
            match api::save_backup_settings(&settings, &BTreeMap::new()).await {
                Ok(fresh) => {
                    trouble.set(None);
                    shown.set(None);
                    overview.set(Some(fresh));
                }
                Err(why) => trouble.set(Some(why)),
            }
        });
    };
    let reveal = move |_| {
        spawn_local(async move {
            match api::fetch_backup_key().await {
                Ok(key) => shown.set(Some(key)),
                Err(why) => trouble.set(Some(why)),
            }
        });
    };
    let renew = move |_| {
        spawn_local(async move {
            match api::new_backup_key(None).await {
                Ok(key) => {
                    shown.set(Some(key));
                    renewing.set(false);
                    refresh(overview);
                }
                Err(why) => trouble.set(Some(why)),
            }
        });
    };
    let copy = move |_| {
        let Some(key) = shown.get_untracked() else {
            return;
        };
        spawn_local(async move {
            copied.set(Some(crate::log_window::write_to_clipboard(&key).await));
            set_timeout(move || copied.set(None), COPIED_FOR);
        });
    };

    view! {
        <div class="backup-block">
            <h4 class="backup-heading">"Encryption"</h4>
            <button type="button" class="scope-pick" class:on=encrypt aria-pressed=move || encrypt().to_string()
                on:click=move |_| set_encrypt(!encrypt())>
                <svg class="scope-tick" viewBox="0 0 24 24" aria-hidden="true">
                    <rect x="3.5" y="3.5" width="17" height="17" rx="4.5" />
                    <path d="M7.5 12.5l3 3 6-6.5" pathLength="1" />
                </svg>
                <span class="scope-words">
                    <span>"Seal backups with a recovery key"</span>
                    <span class="muted small">
                        "A backup holds your people's password hashes and every secret in the home. \
                         Sealed, the file is unreadable without the key."
                    </span>
                </span>
            </button>
            {move || (!encrypt()).then(|| view! {
                <p class="why">
                    "Backups are readable by anyone who gets the file. They can't be copied to \
                     another place like this."
                </p>
            })}
            {move || encrypt().then(|| view! {
                <div class="backup-key-actions">
                    <button type="button" class="press" on:click=reveal>"Show the recovery key"</button>
                    {move || if renewing.get() {
                        view! {
                            <span class="token-sure">
                                <span class="muted small">"Backups already made keep the key they were sealed with."</span>
                                <button type="button" class="press danger" on:click=renew>"Make a new key"</button>
                                <button type="button" class="press" on:click=move |_| renewing.set(false)>"Keep this one"</button>
                            </span>
                        }.into_any()
                    } else {
                        view! {
                            <button type="button" class="press" on:click=move |_| renewing.set(true)>"New key…"</button>
                        }.into_any()
                    }}
                </div>
            })}
            {move || shown.get().map(|key| {
                let file = format!(
                    "data:text/plain;charset=utf-8,Irori backup recovery key%0A%0A{key}%0A%0AKeep this somewhere that isn't the machine Irori runs on. Without it, a sealed backup can't be opened.%0A"
                );
                view! {
                    <div class="token-secret" role="status">
                        <p class="token-secret-head">
                            <svg viewBox="0 0 24 24" aria-hidden="true">
                                <circle cx="12" cy="12" r="9.5" pathLength="1" />
                                <path d="M7.5 12.5l3 3 6-6.5" pathLength="1" />
                            </svg>
                            <span>
                                "This is the recovery key. If this machine is lost, it is the only way \
                                 into your backups: keep a copy that isn't on it."
                            </span>
                        </p>
                        <div class="token-secret-row">
                            <code>{key.clone()}</code>
                            <button type="button" class="log-copy" class:done=move || copied.get() == Some(true)
                                aria-label="Copy the key" on:click=copy>
                                {crate::log_window::copy_icon()}
                            </button>
                        </div>
                        <div class="token-secret-foot">
                            <a class="press" href=file download="irori-recovery-key.txt">"Save as a file"</a>
                            <button type="button" class="press" on:click=move |_| shown.set(None)>"Hide"</button>
                        </div>
                    </div>
                }
            })}
            {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
        </div>
    }
}
