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
        return "the last restore failed".to_owned();
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
        said.push_str(" · unencrypted");
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
                    {move || if busy.get() { "Creating backup…" } else { "Create backup" }}
                </button>
                <label class="press backup-upload" class:disabled=move || sending.get()>
                    {move || if sending.get() { "Uploading…" } else { "Upload backup…" }}
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
            <Places />
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
        parts.push(format!("{} free on this machine.", size(free)));
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
                    format!("The restore from {} failed.", done.archive)
                }}
            </p>
            {done.error.clone().map(|why| view! { <p class="why">{why}</p> })}
            {(ok && done.safety_copy.is_some()).then(|| view! {
                <p class="muted small">
                    "Your previous setup was saved as "<code>{done.safety_copy.clone()}</code>
                    " in the list below."
                </p>
            })}
            {(!done.reinstalled.is_empty()).then(|| view! {
                <p class="muted small">"Extensions reinstalled: "{done.reinstalled.join(", ")}"."</p>
            })}
            {(!done.awaiting_approval.is_empty()).then(|| view! {
                <p class="muted small">
                    "These extensions need full access to this machine, so they are not reinstalled automatically. \
                     Install them from the Extensions page: "{done.awaiting_approval.join(", ")}"."
                </p>
            })}
            {(!done.reinstall_failed.is_empty()).then(|| view! {
                <p class="why">"Couldn't reinstall: "{done.reinstall_failed.join("; ")}</p>
            })}
            {done.warnings.iter().map(|w| view! { <p class="muted small">"Note: "{w.clone()}</p> }).collect_view()}
            <button type="button" class="press" on:click=dismiss>"Dismiss"</button>
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
                    return view! { <li class="tokens-none muted">"Loading…"</li> }.into_any();
                };
                if o.backups.is_empty() {
                    return view! {
                        <li class="tokens-none muted">
                            "No backups yet. Create one now, or turn on automatic backups below."
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
        // Left as it is by default: the record of who changed the home isn't wound back with it.
        activity: false,
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
        format!("Before restore · {}", when(&entry.modified))
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
                    <span class="scope-chip">{if entry.encrypted { "encrypted" } else { "not encrypted" }}</span>
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
                            <span class="muted small">"This can't be undone."</span>
                            <button type="button" class="press danger" on:click=delete>"Delete"</button>
                            <button type="button" class="press"
                                on:click=move |_| {
                                    if deleting.get_untracked().as_deref() == Some(keep_name.as_str()) {
                                        deleting.set(None);
                                    }
                                }>"Cancel"</button>
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
    let has_activity = entry.manifest.as_ref().is_some_and(|m| m.activity);
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
        if !(chosen.settings
            || chosen.extensions
            || chosen.state
            || chosen.history
            || chosen.activity)
        {
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
                        "This backup is encrypted. Leave this empty to use the key saved on this Irori, or enter the key if the backup came from elsewhere."
                    </span>
                </label>
            })}
            <div class="scope-picks" role="group" aria-label="What to put back">
                {pick("Settings", "Rooms, names, automations, people and their passwords, the https certificate.",
                    |p| p.settings, |p| &mut p.settings, true)}
                {pick("Extension data", "What extensions store, such as Zigbee pairings and keys.",
                    |p| p.extensions, |p| &mut p.extensions, true)}
                {pick("Sessions and chats", "Signed-in sessions, access tokens, the assistant's chats.",
                    |p| p.state, |p| &mut p.state, true)}
                {pick(
                    "History",
                    if has_history { "Sensor readings and state changes. Turn off to keep the history Irori has now." }
                    else { "This backup was made without history." },
                    |p| p.history, |p| &mut p.history, has_history,
                )}
                {pick(
                    "Activity log",
                    if has_activity { "Who changed what, as the backup had it. Turn on only to wind the record back too; off keeps the one Irori has now." }
                    else { "This backup has no activity log." },
                    |p| p.activity, |p| &mut p.activity, has_activity,
                )}
            </div>
            {move || checked.get().map(|v| view! {
                <div class="backup-checked" role="status">
                    <p>
                        "Backup is valid. Made by Irori "{v.manifest.irori_version.clone()}" on "
                        {when(&v.manifest.created_at)}", "{v.files}" files."
                    </p>
                    {v.warnings.iter().map(|w| view! { <p class="muted small">"Note: "{w.clone()}</p> }).collect_view()}
                </div>
            })}
            {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
            <p class="muted small">
                "Irori will save your current setup (it appears in this list), restart, and apply this backup. \
                 If the backup has different users or passwords, sign in with those afterwards."
            </p>
            <div class="settings-form-actions">
                <button type="button" disabled=move || busy.get() on:click=check>"Verify"</button>
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
        let Some(drafted) = draft.get_untracked() else {
            return;
        };
        let Some(saved) = overview.with_untracked(|o| o.as_ref().map(|o| o.settings.clone()))
        else {
            return;
        };
        if saving.get_untracked() || !changed() {
            return;
        }
        let settings = merged(&saved, &drafted);
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
                            <span>"Back up automatically"</span>
                            <span class="muted small">
                                "The oldest backups are deleted as new ones are made. If this machine is off at the \
                                 scheduled time, the backup runs when it starts again."
                            </span>
                        </span>
                    </button>
                    <div class="backup-schedule" class:muted=!enabled>
                        <label class="settings-field backup-time">
                            "Time"
                            <input type="time" prop:value=d.at.clone()
                                on:input=move |e| { let v = event_target_value(&e); edit(Box::new(move |d| d.at = v)); } />
                        </label>
                        <div class="settings-field">
                            <span>"Days"</span>
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
                            "Backups to keep"
                            <input type="number" min="1" inputmode="numeric" prop:value=d.keep.to_string()
                                on:input=move |e| {
                                    if let Ok(n) = event_target_value(&e).parse::<u32>() {
                                        edit(Box::new(move |d| d.keep = n.max(1)));
                                    }
                                } />
                        </label>
                        <label class="settings-field backup-keep">
                            "Maximum age in days (optional)"
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
                                "All sensor readings and state changes. Without it backups are much \
                                 smaller, and a restore starts the history over."
                            </span>
                        </span>
                    </button>
                    <button type="button" class="scope-pick" class:on=d.before_update
                        aria-pressed=d.before_update.to_string()
                        on:click=move |_| edit(Box::new(|d| d.before_update = !d.before_update))>
                        <svg class="scope-tick" viewBox="0 0 24 24" aria-hidden="true">
                            <rect x="3.5" y="3.5" width="17" height="17" rx="4.5" />
                            <path d="M7.5 12.5l3 3 6-6.5" pathLength="1" />
                        </svg>
                        <span class="scope-words">
                            <span>"Back up before updates"</span>
                            <span class="muted small">
                                "Before Irori or an extension is updated. If the backup fails, the update \
                                 doesn't start. The last three are kept."
                            </span>
                        </span>
                    </button>
                }
            })}
            {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
            <div class="place-actions">
                <span class="muted small" class:history-saved=move || done.get()
                    class:history-unsaved=move || !done.get() && changed()>
                    {move || if done.get() { "Saved." } else if changed() { "Unsaved changes." } else { "" }}
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

/// Whether the schedule form's own fields are as they were saved. The places and whether backups
/// are sealed are other blocks' to change, so this form neither compares nor sends them.
fn same(a: &BackupSettings, b: &BackupSettings) -> bool {
    a.enabled == b.enabled
        && a.at == b.at
        && a.days == b.days
        && a.keep == b.keep
        && a.keep_days == b.keep_days
        && a.include_history == b.include_history
        && a.before_update == b.before_update
}

/// What was saved, with this form's fields changed to the draft's.
fn merged(saved: &BackupSettings, draft: &BackupSettings) -> BackupSettings {
    BackupSettings {
        enabled: draft.enabled,
        at: draft.at.clone(),
        days: draft.days.clone(),
        keep: draft.keep,
        keep_days: draft.keep_days,
        include_history: draft.include_history,
        before_update: draft.before_update,
        ..saved.clone()
    }
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
                    <span>"Encrypt backups"</span>
                    <span class="muted small">
                        "A backup contains password hashes and every secret in the home. Encrypted, \
                         the file can't be read without the recovery key."
                    </span>
                </span>
            </button>
            {move || (!encrypt()).then(|| view! {
                <p class="why">
                    "Anyone who gets a backup file can read it. Backups can't be copied to \
                     other locations while encryption is off."
                </p>
            })}
            {move || encrypt().then(|| view! {
                <div class="backup-key-actions">
                    <button type="button" class="press" on:click=reveal>"Show the recovery key"</button>
                    {move || if renewing.get() {
                        view! {
                            <span class="token-sure">
                                <span class="muted small">"Existing backups still need the key they were made with."</span>
                                <button type="button" class="press danger" on:click=renew>"Generate new key"</button>
                                <button type="button" class="press" on:click=move |_| renewing.set(false)>"Cancel"</button>
                            </span>
                        }.into_any()
                    } else {
                        view! {
                            <button type="button" class="press" on:click=move |_| renewing.set(true)>"Generate new key…"</button>
                        }.into_any()
                    }}
                </div>
            })}
            {move || shown.get().map(|key| {
                let file = format!(
                    "data:text/plain;charset=utf-8,Irori backup recovery key%0A%0A{key}%0A%0AKeep this somewhere that isn't the machine Irori runs on. Without it, an encrypted backup can't be opened.%0A"
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
                                 to open your backups. Keep a copy somewhere else."
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
                            <a class="press" href=file download="irori-recovery-key.txt">"Download key"</a>
                            <button type="button" class="press" on:click=move |_| shown.set(None)>"Hide"</button>
                        </div>
                    </div>
                }
            })}
            {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
        </div>
    }
}

// ----- other places -----

/// What a place is, in a few words.
fn describe(destination: &api::BackupDestination) -> (&'static str, String) {
    match destination {
        api::BackupDestination::Path { path, .. } => ("Folder", path.clone()),
        api::BackupDestination::Webdav { url, username, .. } => {
            ("WebDAV", format!("{username} at {url}"))
        }
        api::BackupDestination::S3 {
            endpoint,
            bucket,
            prefix,
            ..
        } => ("S3", format!("{bucket}/{prefix} at {endpoint}")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Folder,
    Webdav,
    S3,
}

#[component]
fn Places() -> impl IntoView {
    let Backups(overview) = expect_context::<Backups>();
    let trouble = RwSignal::new(None::<String>);
    let adding = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let kind = RwSignal::new(Kind::Folder);
    let name = RwSignal::new(String::new());
    let (path, url, user, password) = (
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
    );
    let (endpoint, bucket, region, prefix, access, secret) = (
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new("us-east-1".to_owned()),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
    );
    // What a Test said, by place: Ok(()) or the reason.
    let tested = RwSignal::new(BTreeMap::<String, Result<(), String>>::new());
    // The place whose backups are being looked through.
    let browsing = RwSignal::new(None::<(String, Vec<api::BackupThere>)>);
    let removing = RwSignal::new(None::<String>);

    let save = move |settings: BackupSettings,
                     secrets: BTreeMap<String, String>,
                     then_test: Option<String>| {
        busy.set(true);
        trouble.set(None);
        spawn_local(async move {
            match api::save_backup_settings(&settings, &secrets).await {
                Ok(fresh) => {
                    overview.set(Some(fresh));
                    if let Some(place) = then_test {
                        let said = api::test_backup_destination(&place).await;
                        tested.update(|t| {
                            t.insert(place, said);
                        });
                    }
                    adding.set(false);
                    name.set(String::new());
                    for field in [
                        path, url, user, password, endpoint, bucket, prefix, access, secret,
                    ] {
                        field.set(String::new());
                    }
                }
                Err(why) => trouble.set(Some(why)),
            }
            busy.set(false);
        });
    };

    let add = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }
        let Some(mut settings) =
            overview.with_untracked(|o| o.as_ref().map(|o| o.settings.clone()))
        else {
            return;
        };
        let place = name.get_untracked().trim().to_lowercase().replace(' ', "-");
        let (destination, secrets) = match kind.get_untracked() {
            Kind::Folder => (
                api::BackupDestination::Path {
                    name: place.clone(),
                    path: path.get_untracked().trim().to_owned(),
                },
                BTreeMap::new(),
            ),
            Kind::Webdav => (
                api::BackupDestination::Webdav {
                    name: place.clone(),
                    url: url.get_untracked().trim().to_owned(),
                    username: user.get_untracked().trim().to_owned(),
                },
                BTreeMap::from([(place.clone(), password.get_untracked())]),
            ),
            Kind::S3 => (
                api::BackupDestination::S3 {
                    name: place.clone(),
                    endpoint: endpoint.get_untracked().trim().to_owned(),
                    bucket: bucket.get_untracked().trim().to_owned(),
                    region: region.get_untracked().trim().to_owned(),
                    prefix: prefix.get_untracked().trim().to_owned(),
                    access_key: access.get_untracked().trim().to_owned(),
                },
                BTreeMap::from([(place.clone(), secret.get_untracked())]),
            ),
        };
        settings.destination.retain(|d| d.name() != place);
        settings.destination.push(destination);
        save(settings, secrets, Some(place));
    };

    let remove = move |place: String| {
        let Some(mut settings) =
            overview.with_untracked(|o| o.as_ref().map(|o| o.settings.clone()))
        else {
            return;
        };
        settings.destination.retain(|d| d.name() != place);
        removing.set(None);
        save(settings, BTreeMap::new(), None);
    };
    let test = move |place: String| {
        spawn_local(async move {
            let said = api::test_backup_destination(&place).await;
            tested.update(|t| {
                t.insert(place, said);
            });
        });
    };
    let browse = move |place: String| {
        spawn_local(async move {
            match api::browse_backup_destination(&place).await {
                Ok(list) => browsing.set(Some((place, list))),
                Err(why) => {
                    trouble.set(Some(why));
                    browsing.set(None);
                }
            }
        });
    };
    let bring = move |place: String, file: String| {
        spawn_local(async move {
            match api::fetch_backup_from(&place, &file).await {
                Ok(()) => {
                    trouble.set(None);
                    refresh(overview);
                    if let Ok(list) = api::browse_backup_destination(&place).await {
                        browsing.set(Some((place, list)));
                    }
                }
                Err(why) => trouble.set(Some(why)),
            }
        });
    };

    let field =
        |label: &'static str, hint: &'static str, signal: RwSignal<String>, secret_input: bool| {
            view! {
                <label class="settings-field">
                    {label}
                    <input
                        type=if secret_input { "password" } else { "text" }
                        autocomplete="off"
                        spellcheck="false"
                        placeholder=hint
                        prop:value=move || signal.get()
                        on:input=move |e| signal.set(event_target_value(&e))
                    />
                </label>
            }
        };

    let kinds = NodeRef::<leptos::html::Div>::new();
    crate::glide::across(kinds, "button.chosen", move || kind.track());

    view! {
        <div class="backup-block">
            <h4 class="backup-heading">"Backup locations"</h4>
            <p class="muted small">
                "Every backup is also copied, encrypted, to each location below. To use a network \
                 share or USB disk, mount it on this machine and add it as a folder."
            </p>
            <ul class="tokens backups">
                {move || {
                    let Some(o) = overview.get() else { return ().into_any() };
                    if o.settings.destination.is_empty() {
                        return view! { <li class="tokens-none muted">"Backups are only stored on this machine."</li> }.into_any();
                    }
                    o.settings
                        .destination
                        .iter()
                        .map(|d| {
                            let (label, detail) = describe(d);
                            let place = d.name().to_owned();
                            let state = o.status.destinations.get(&place).cloned().unwrap_or_default();
                            let (t_place, b_place, r_place, a_place, k_place) =
                                (place.clone(), place.clone(), place.clone(), place.clone(), place.clone());
                            view! {
                                <li class="token backup">
                                    <span class="token-mark" aria-hidden="true">
                                        <svg viewBox="0 0 24 24" inner_html=OPEN_BOX></svg>
                                    </span>
                                    <span class="token-words">
                                        <span class="token-name">{place.clone()}</span>
                                        <span class="token-may">
                                            <span class="scope-chip">{label}</span>
                                            <span class="scope-chip">{detail}</span>
                                            {state.last_ok.as_deref().map(|at| view! {
                                                <span class="scope-chip">"copied "{ago(at)}</span>
                                            })}
                                        </span>
                                        {state.last_error.clone().map(|why| view! { <span class="why">{why}</span> })}
                                        {move || tested.with(|t| t.get(&t_place).cloned()).map(|said| match said {
                                            Ok(()) => view! { <span class="muted small">"Connection works and the location is writable."</span> }.into_any(),
                                            Err(why) => view! { <span class="why">{why}</span> }.into_any(),
                                        })}
                                    </span>
                                    {move || if removing.get().as_deref() == Some(a_place.as_str()) {
                                        let (go, keep) = (r_place.clone(), a_place.clone());
                                        view! {
                                            <span class="token-sure">
                                                <span class="muted small">"Backups already copied there are kept."</span>
                                                <button type="button" class="press danger" on:click=move |_| remove(go.clone())>"Remove"</button>
                                                <button type="button" class="press" on:click=move |_| { let _ = &keep; removing.set(None); }>"Keep"</button>
                                            </span>
                                        }.into_any()
                                    } else {
                                        let (t, b, k) = (k_place.clone(), b_place.clone(), k_place.clone());
                                        view! {
                                            <span class="backup-actions">
                                                <button type="button" class="press" on:click=move |_| test(t.clone())>"Test"</button>
                                                <button type="button" class="press" on:click=move |_| browse(b.clone())>"Browse"</button>
                                                <button type="button" class="press" on:click=move |_| removing.set(Some(k.clone()))>"Remove"</button>
                                            </span>
                                        }.into_any()
                                    }}
                                </li>
                            }
                        })
                        .collect_view()
                        .into_any()
                }}
            </ul>
            {move || browsing.get().map(|(place, list)| view! {
                <div class="backup-checked">
                    <p>"Backups at "<b>{place.clone()}</b></p>
                    {if list.is_empty() {
                        view! { <p class="muted small">"No backups there yet."</p> }.into_any()
                    } else {
                        list.into_iter().map(|there| {
                            let (p, n) = (place.clone(), there.name.clone());
                            view! {
                                <p class="backup-there">
                                    <code>{there.name.clone()}</code>
                                    {if there.here {
                                        view! { <span class="muted small">"already on this machine"</span> }.into_any()
                                    } else {
                                        view! {
                                            <button type="button" class="press" on:click=move |_| bring(p.clone(), n.clone())>
                                                "Copy to this machine"
                                            </button>
                                        }.into_any()
                                    }}
                                </p>
                            }
                        }).collect_view().into_any()
                    }}
                    <button type="button" class="press" on:click=move |_| browsing.set(None)>"Close"</button>
                </div>
            })}
            {move || (!adding.get()).then(|| view! {
                <button type="button" class="press token-add" on:click=move |_| { trouble.set(None); adding.set(true); }>
                    <span aria-hidden="true">"+"</span>" Add location"
                </button>
            })}
            <div class="drawer" class:open=move || adding.get() inert=move || (!adding.get()).then_some("")>
                <form class="drawer-inner token-form" on:submit=add>
                    {field("Name", "nas", name, false)}
                    <div class="settings-field">
                        <span id="place-kind">"Type"</span>
                        <div class="switcher" role="group" aria-labelledby="place-kind" node_ref=kinds>
                            <span class="glide" aria-hidden="true"></span>
                            {[(Kind::Folder, "Folder"), (Kind::Webdav, "WebDAV"), (Kind::S3, "S3")].into_iter().map(|(k, label)| view! {
                                <button type="button" class:chosen=move || kind.get() == k
                                    aria-pressed=move || (kind.get() == k).to_string()
                                    on:click=move |_| kind.set(k)>{label}</button>
                            }).collect_view()}
                        </div>
                    </div>
                    {move || match kind.get() {
                        Kind::Folder => view! {
                            <div class="token-kind-body">
                                {field("Folder path", "/mnt/nas/irori", path, false)}
                                <span class="muted small">"The full path on this machine. The folder must exist and Irori must be able to write to it."</span>
                            </div>
                        }.into_any(),
                        Kind::Webdav => view! {
                            <div class="token-kind-body">
                                {field("WebDAV URL", "https://cloud.example.com/remote.php/dav/files/me/irori", url, false)}
                                {field("Username", "me", user, false)}
                                {field("Password", "an app password is recommended", password, true)}
                            </div>
                        }.into_any(),
                        Kind::S3 => view! {
                            <div class="token-kind-body">
                                {field("Endpoint", "https://s3.us-west-000.backblazeb2.com", endpoint, false)}
                                {field("Bucket", "my-irori-backups", bucket, false)}
                                {field("Region", "us-east-1", region, false)}
                                {field("Path prefix (optional)", "home", prefix, false)}
                                {field("Access key ID", "", access, false)}
                                {field("Secret access key", "", secret, true)}
                            </div>
                        }.into_any(),
                    }}
                    <div class="settings-form-actions">
                        <button type="button" on:click=move |_| { trouble.set(None); adding.set(false); }>"Cancel"</button>
                        <button type="submit" class="add" disabled=move || busy.get()>
                            {move || if busy.get() { "Saving…" } else { "Add and test" }}
                        </button>
                    </div>
                </form>
            </div>
            {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
        </div>
    }
}
