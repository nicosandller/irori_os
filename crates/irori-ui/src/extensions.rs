//! The Extensions page: every official extension, install and uninstall.

use std::collections::BTreeMap;
use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{self, CatalogEntry};
use crate::devices::icon;
use crate::settings_form::SettingsForm;

#[component]
pub fn Extensions() -> impl IntoView {
    let catalog = RwSignal::new(Vec::<CatalogEntry>::new());
    let trouble = RwSignal::new(None::<String>);
    let busy = RwSignal::new(None::<String>);
    let search = RwSignal::new(String::new());
    let filter = RwSignal::new("all".to_owned());
    // Which extension's settings form is open, if any — at most one at a time.
    let settings_open = RwSignal::new(None::<String>);
    // And which one's log window, same rule.
    let log_open = RwSignal::new(None::<String>);
    // An extension with full access to the machine doesn't install on the first click. This is
    // the one waiting for that approval.
    let confirm_install = RwSignal::new(None::<String>);
    let updating = Updating(RwSignal::new(None::<String>));
    provide_context(updating);
    // Which extension's whole story is open: a card says what fits, and this is the rest.
    let more_open = RwSignal::new(None::<String>);
    // The newest error in each extension's own log, for the mark on its log button.
    let log_errors = RwSignal::new(BTreeMap::<String, String>::new());
    // The trouble somebody has read and cleared, per extension: the mark stays off until that
    // extension's trouble is a different one. Kept by the browser, like other things that are
    // about reading rather than about the home.
    let cleared = RwSignal::new(remembered_cleared());
    Effect::new(move |_| {
        let saved = serde_json::to_string(&cleared.get()).unwrap_or_default();
        crate::devices::remember(CLEARED_KEY, &saved);
    });

    let reload = move || {
        spawn_local(async move {
            match api::fetch_catalog().await {
                Ok(entries) => {
                    catalog.set(entries);
                    trouble.set(None);
                }
                Err(why) => trouble.set(Some(why)),
            }
        });
    };
    reload();

    // Looked for when the page opens and whenever what's installed changes — not with every
    // refresh of the catalog, which while something installs is once a second.
    let installed = Memo::new(move |_| {
        catalog.with(|catalog| {
            catalog
                .iter()
                .filter(|entry| entry.installed)
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>()
        })
    });
    Effect::new(move |_| {
        for id in installed.get() {
            spawn_local(async move {
                let Ok(lines) = api::fetch_extension_log(&id).await else {
                    return;
                };
                let newest = crate::log_window::last_error(&lines).map(str::to_owned);
                log_errors.update(|marked| match newest {
                    Some(error) => {
                        marked.insert(id, error);
                    }
                    None => {
                        marked.remove(&id);
                    }
                });
            });
        }
    });

    // The categories actually present, in the order the catalog lists them, so a quick filter
    // never offers a choice that would show nothing.
    let categories = Memo::new(move |_| {
        let mut seen = Vec::new();
        for entry in catalog.get() {
            if !seen.contains(&entry.category) {
                seen.push(entry.category.clone());
            }
        }
        seen
    });

    let visible = Memo::new(move |_| {
        let needle = search.get().to_lowercase();
        let active = filter.get();
        catalog
            .get()
            .into_iter()
            .filter(|entry| {
                (active == "all" || entry.category == active)
                    && (needle.is_empty()
                        || entry.name.to_lowercase().contains(&needle)
                        || entry.description.to_lowercase().contains(&needle))
            })
            .collect::<Vec<_>>()
    });

    view! {
        <div class="page-head">
            <h1>"Extensions"</h1>
        </div>
        <p class="lede">
            "Irori itself doesn't talk to devices or run automations. Those arrive as "
            "extensions: official ones live in this repo and are not compiled into the core. "
            "Install downloads (or, in a checkout, builds) a package; uninstall deletes it "
            "and anything it brought in."
        </p>
        {move || trouble.get().map(|why| view! { <p class="banner">{why}</p> })}

        <div class="ext-controls">
            <input
                class="ext-search"
                type="search"
                placeholder="Search extensions"
                aria-label="Search extensions"
                prop:value=search
                on:input:target=move |ev| search.set(ev.target().value())
            />
            <div class="ext-chips" role="group" aria-label="Filter by category">
                <button
                    type="button"
                    class:active=move || filter.get() == "all"
                    on:click=move |_| filter.set("all".into())
                >
                    "All"
                </button>
                {move || {
                    categories
                        .get()
                        .into_iter()
                        .map(|category| {
                            let label = category_label(&category);
                            let value = category.clone();
                            view! {
                                <button
                                    type="button"
                                    class:active=move || filter.get() == category
                                    on:click=move |_| filter.set(value.clone())
                                >
                                    {label}
                                </button>
                            }
                        })
                        .collect_view()
                }}
            </div>
        </div>

        {move || {
            let entries = visible.get();
            if entries.is_empty() {
                view! { <p class="muted">"No extensions match."</p> }.into_any()
            } else {
                view! {
                    <div class="ext-grid">
                        {
                            entries
                                .into_iter()
                                .map(|entry| {
                                    card(
                                        entry,
                                        busy,
                                        catalog,
                                        trouble,
                                        settings_open,
                                        log_open,
                                        confirm_install,
                                        more_open,
                                        log_errors,
                                        cleared,
                                    )
                                })
                                .collect_view()
                        }
                    </div>
                }
                    .into_any()
            }
        }}

        // The open settings form, over the page rather than inside the card that opened it: a
        // form that grows its own card reflows the whole grid around it. Looked up from the
        // catalog by id so the card itself doesn't have to hold it.
        {move || {
            let id = settings_open.get()?;
            let entry = catalog.get().into_iter().find(|entry| entry.id == id)?;
            let schema = entry.config_schema.clone()?;
            Some(view! {
                <crate::modal::Modal
                    title=format!("{} settings", entry.name)
                    on_close=move || settings_open.set(None)
                >
                    <SettingsForm
                        id=id.clone()
                        schema=schema
                        current=entry.settings.clone()
                        on_close=move || {
                            settings_open.set(None);
                            reload();
                        }
                    />
                </crate::modal::Modal>
            })
        }}

        // Everything a card had no room for: the whole description, what access it has, how it
        // is faring and why, and what it found that Irori can't use yet.
        {move || {
            let id = more_open.get()?;
            let entry = catalog.get().into_iter().find(|entry| entry.id == id)?;
            let unmodeled = unplaced_unmodeled(&id);
            let log_id = id.clone();
            Some(view! {
                <crate::modal::Modal title=entry.name.clone() on_close=move || more_open.set(None)>
                    <div class="ext-more">
                        <p class="muted small">
                            {category_label(&entry.category)}" · "{version_words(&entry)}
                            {entry.state.clone().map(|state| format!(" · {}", state.replace('_', " ")))}
                        </p>
                        <p>{entry.description.clone()}</p>
                        {entry.full_access.then(|| view! {
                            <p class="ext-access">
                                "Full access to this machine: it can download and run other "
                                "programs, the same as a terminal."
                            </p>
                        })}
                        {entry.reason.clone().map(|why| view! {
                            <p class="why">{why}</p>
                            {entry.installed.then(|| view! {
                                <p>
                                    <button
                                        type="button"
                                        class="quiet-button"
                                        on:click=move |_| {
                                            more_open.set(None);
                                            log_open.set(Some(log_id.clone()));
                                        }
                                    >
                                        "View log"
                                    </button>
                                </p>
                            })}
                        })}
                        {unmodeled.map(|text| view! { <p class="muted small">{text}</p> })}
                    </div>
                </crate::modal::Modal>
            })
        }}

        {move || log_open.get().map(|id| {
            let reason = catalog.with(|catalog| {
                catalog
                    .iter()
                    .find(|entry| entry.id == id)
                    .and_then(|entry| entry.reason.clone())
            });
            // Only offered when there is a mark to clear.
            let marked = cleared.with_untracked(|cleared| {
                log_errors.with_untracked(|errors| {
                    troubled(&id, reason.as_deref(), errors, cleared)
                })
            });
            let clear = {
                let id = id.clone();
                Callback::new(move |()| {
                    let (id, reason) = (id.clone(), reason.clone());
                    // What's in the log now, not what was there when the page opened: the
                    // error being cleared is the one just read.
                    spawn_local(async move {
                        let newest = match api::fetch_extension_log(&id).await {
                            Ok(lines) => crate::log_window::last_error(&lines).map(str::to_owned),
                            Err(_) => log_errors.with_untracked(|errors| errors.get(&id).cloned()),
                        };
                        log_errors.update(|errors| match &newest {
                            Some(error) => {
                                errors.insert(id.clone(), error.clone());
                            }
                            None => {
                                errors.remove(&id);
                            }
                        });
                        let seen = trouble_of(reason.as_deref(), newest.as_deref());
                        cleared.update(|cleared| {
                            cleared.insert(id, seen);
                        });
                    });
                })
            };
            if marked {
                view! {
                    <crate::log_window::LogWindow
                        source=crate::log_window::Source::Extension(id)
                        on_close=move || log_open.set(None)
                        on_clear=clear
                    />
                }
                .into_any()
            } else {
                view! {
                    <crate::log_window::LogWindow
                        source=crate::log_window::Source::Extension(id)
                        on_close=move || log_open.set(None)
                    />
                }
                .into_any()
            }
        })}

        // Full access is the one install that has to be asked about (extensions.md §7). The
        // words are the contract's own: "full access to this machine".
        {move || {
            let id = confirm_install.get()?;
            let entry = catalog.get().into_iter().find(|entry| entry.id == id)?;
            let name = entry.name.clone();
            let id_install = id.clone();
            // The same question for a newer version of one that's installed: it's new code
            // being given the same reach.
            let (what, verb) = if entry.installed {
                (Do::Update, "Update")
            } else {
                (Do::Install, "Install")
            };
            Some(view! {
                <crate::modal::Modal
                    title=format!("{verb} {name}?")
                    on_close=move || confirm_install.set(None)
                >
                    <p>
                        {name}
                        " has "
                        <strong>"full access to this machine"</strong>
                        ". It can download and run other programs, the same as a terminal. "
                        {format!("{verb} it only if that is what you want.")}
                    </p>
                    <div class="ext-actions">
                        <button
                            type="button"
                            class="danger"
                            on:click=move |_| confirm_install.set(None)
                        >
                            "Cancel"
                        </button>
                        <button
                            type="button"
                            class="add solid"
                            on:click=move |_| {
                                confirm_install.set(None);
                                act(id_install.clone(), what, busy, catalog, trouble, true, updating);
                            }
                        >
                            {verb}
                        </button>
                    </div>
                </crate::modal::Modal>
            })
        }}
    }
}

/// One extension: what it is in a couple of lines, and its buttons. Every card is the same
/// height whatever its extension has to say; the rest is behind "Show more". What's wrong with
/// one isn't written on its card — the mark on its log button says there is something to read.
#[allow(clippy::too_many_arguments)]
fn card(
    entry: CatalogEntry,
    busy: RwSignal<Option<String>>,
    catalog: RwSignal<Vec<CatalogEntry>>,
    trouble: RwSignal<Option<String>>,
    settings_open: RwSignal<Option<String>>,
    log_open: RwSignal<Option<String>>,
    confirm_install: RwSignal<Option<String>>,
    more_open: RwSignal<Option<String>>,
    log_errors: RwSignal<BTreeMap<String, String>>,
    cleared: RwSignal<BTreeMap<String, String>>,
) -> impl IntoView {
    let id = entry.id.clone();
    let id_busy = id.clone();
    let id_click = id.clone();
    let id_gear = id.clone();
    let id_log = id.clone();
    let id_more = id.clone();
    let id_update = id.clone();
    let id_marked = id.clone();
    let installed = entry.installed;
    let full_access = entry.full_access;
    let running = entry.state.as_deref() == Some("running");
    // Nothing is wrong with it — it just hasn't been told something it can't start without, and
    // the way out is the very button next to this.
    let needs_setup = entry.state.as_deref() == Some("needs_setup");
    // Waiting for its program to dial in is not a failure. The reason is what it's waiting
    // for, shown as ordinary text, and it doesn't mark the log button.
    let waiting = entry.state.as_deref() == Some("waiting");
    let waiting_for = if waiting { entry.reason.clone() } else { None };
    let schema = entry.config_schema.clone();
    // Irori's own word that something went wrong, or an error in what the extension wrote —
    // unless that very trouble has been read and cleared.
    let reason = if waiting { None } else { entry.reason.clone() };
    let troubled = move || {
        log_errors.with(|errors| {
            cleared.with(|cleared| troubled(&id_marked, reason.as_deref(), errors, cleared))
        })
    };
    // A `Memo` rather than a plain closure: it's `Copy`, so the same check can be read from the
    // button's `disabled`, its progress bar, and its label without cloning the id three times.
    let is_busy = Memo::new(move |_| busy.get().as_deref() == Some(id_busy.as_str()));
    let updating = expect_context::<Updating>();
    let id_updating = id.clone();
    let is_updating = Memo::new(move |_| {
        is_busy.get() && updating.0.get().as_deref() == Some(id_updating.as_str())
    });

    view! {
        <section class="ext-card">
            <span class="ext-category">{category_label(&entry.category)}</span>
            <div class="ext-card-head">
                // `entry.icon` already reflects whether the icon endpoint has bytes to serve
                // right now (the server checks the same live state), so nothing to combine here.
                {icon(&entry.id, entry.icon)}
                <div class="ext-card-title">
                    <span class="name">{entry.name.clone()}</span>
                    <span class="muted small">{version_words(&entry)}</span>
                </div>
            </div>
            <div class="ext-standing">
                {entry.state.clone().map(|state| view! {
                    <span class="state" class:ok=running class:wants-setup=needs_setup>
                        {state.replace('_', " ")}
                    </span>
                })}
                {waiting_for.map(|why| view! { <span class="muted small">{why}</span> })}
                {full_access.then(|| view! { <span class="chip quiet ext-full">"full access"</span> })}
            </div>
            <p class="muted ext-description">{entry.description.clone()}</p>
            <button
                type="button"
                class="quiet-button ext-more-button"
                on:click=move |_| more_open.set(Some(id_more.clone()))
            >
                "Show more"
            </button>
            <div class="ext-actions">
                {if installed {
                    view! {
                        <button
                            type="button"
                            class="danger ext-btn"
                            disabled=move || is_busy.get()
                            on:click=move |_| act(id_click.clone(), Do::Uninstall, busy, catalog, trouble, false, updating)
                        >
                            {move || (is_busy.get() && !is_updating.get()).then(|| view! { <span class="bar" aria-hidden="true"></span> })}
                            <span class="label">
                                {move || if is_busy.get() && !is_updating.get() { "Uninstalling…" } else { "Uninstall" }}
                            </span>
                        </button>
                    }
                        .into_any()
                } else {
                    view! {
                        <button
                            type="button"
                            class="add ext-btn"
                            disabled=move || is_busy.get()
                            on:click=move |_| {
                                if full_access {
                                    confirm_install.set(Some(id.clone()));
                                } else {
                                    act(id.clone(), Do::Install, busy, catalog, trouble, false, updating);
                                }
                            }
                        >
                            {move || is_busy.get().then(|| view! { <span class="bar" aria-hidden="true"></span> })}
                            <span class="label">
                                {move || if is_busy.get() { "Installing…" } else { "Install" }}
                            </span>
                        </button>
                    }
                        .into_any()
                }}
                {outdated(&entry).then(|| {
                    let id = id_update.clone();
                    view! {
                        <button
                            type="button"
                            class="add ext-btn"
                            disabled=move || is_busy.get()
                            title="Replaces this version with the newer one. Its devices, settings and data stay."
                            on:click=move |_| {
                                if full_access {
                                    confirm_install.set(Some(id.clone()));
                                } else {
                                    act(id.clone(), Do::Update, busy, catalog, trouble, false, updating);
                                }
                            }
                        >
                            {move || is_updating.get().then(|| view! { <span class="bar" aria-hidden="true"></span> })}
                            <span class="label">
                                {move || if is_updating.get() { "Updating…" } else { "Update" }}
                            </span>
                        </button>
                    }
                })}
                {(installed && schema.is_some()).then(|| {
                    // An extension that can't start until someone fills a setting in says so
                    // in words: a gear next to "needs setup" is a puzzle, not an instruction.
                    view! {
                        <button
                            type="button"
                            class="ext-settings-btn"
                            class:wants-setup=needs_setup
                            aria-label=if needs_setup { "Set it up" } else { "Settings" }
                            title=if needs_setup { "Set it up" } else { "Settings" }
                            on:click=move |_| settings_open.set(Some(id_gear.clone()))
                        >
                            {if needs_setup {
                                "Set it up".into_any()
                            } else {
                                crate::icons::icon(crate::icons::Icon::Gear)
                            }}
                        </button>
                    }
                })}
                // The same button on every installed extension, in the same place, whether or
                // not anything is wrong. When something is, it carries a mark.
                {installed.then(|| {
                    let label = troubled.clone();
                    let title = troubled.clone();
                    let marked = troubled.clone();
                    view! {
                        <button
                            type="button"
                            class="ext-settings-btn ext-log-btn"
                            class:troubled=troubled
                            aria-label=move || if label() { "Log — it has errors" } else { "Log" }
                            title=move || {
                                if title() {
                                    "Something went wrong: read what this extension said"
                                } else {
                                    "What this extension has said for itself"
                                }
                            }
                            on:click=move |_| log_open.set(Some(id_log.clone()))
                        >
                            {crate::icons::icon(crate::icons::Icon::Logs)}
                            <span class="ext-mark" aria-hidden="true" hidden=move || !marked()>"!"</span>
                        </button>
                    }
                })}
            </div>
        </section>
    }
}

/// Where the browser keeps which troubles have been cleared.
const CLEARED_KEY: &str = "irori.extensions.cleared";

fn remembered_cleared() -> BTreeMap<String, String> {
    crate::devices::stored(CLEARED_KEY)
        .and_then(|saved| serde_json::from_str(&saved).ok())
        .unwrap_or_default()
}

/// An extension's trouble, as one thing that can be told apart from its next trouble: why Irori
/// says it failed, and the newest error in its log.
fn trouble_of(reason: Option<&str>, newest_error: Option<&str>) -> String {
    format!(
        "{}\n{}",
        reason.unwrap_or_default(),
        newest_error.unwrap_or_default()
    )
}

/// Whether an extension's log button carries the mark: it has trouble, and it isn't the trouble
/// somebody already cleared.
fn troubled(
    id: &str,
    reason: Option<&str>,
    errors: &BTreeMap<String, String>,
    cleared: &BTreeMap<String, String>,
) -> bool {
    let newest = errors.get(id).map(String::as_str);
    if reason.is_none() && newest.is_none() {
        return false;
    }
    cleared.get(id).map(String::as_str) != Some(trouble_of(reason, newest).as_str())
}

fn category_label(category: &str) -> String {
    match category {
        "protocol" => "Protocol".into(),
        "demo" => "Demo".into(),
        "automation" => "Automation".into(),
        other => other.into(),
    }
}

/// How often to re-check the catalog while an install is still settling.
const SETTLE_POLL: Duration = Duration::from_secs(1);
/// How long to keep polling before giving up and clearing the busy state regardless — long
/// enough for a real first-time provisioning step (Zigbee downloads Node.js and installs
/// Zigbee2MQTT the first time it runs), not so long that a genuinely stuck extension spins the
/// button forever. Its own state badge keeps showing what's going on either way.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// The extension being updated, if one is: its card says "Updating…", not "Uninstalling…".
#[derive(Debug, Clone, Copy)]
struct Updating(RwSignal<Option<String>>);

/// What a card's button asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Do {
    Install,
    /// Replace the installed version with the catalog's, keeping everything else.
    Update,
    Uninstall,
}

#[allow(clippy::too_many_arguments)]
fn act(
    id: String,
    what: Do,
    busy: RwSignal<Option<String>>,
    catalog: RwSignal<Vec<CatalogEntry>>,
    trouble: RwSignal<Option<String>>,
    approve_full_access: bool,
    updating: Updating,
) {
    // Which of its buttons says it's working: read by the card, which is drawn again each
    // time the catalog is.
    updating.0.set((what == Do::Update).then(|| id.clone()));
    busy.set(Some(id.clone()));
    spawn_local(async move {
        let result = match what {
            Do::Install => api::install_extension(&id, approve_full_access, false).await,
            Do::Update => api::install_extension(&id, approve_full_access, true).await,
            Do::Uninstall => api::uninstall_extension(&id).await,
        };
        match result {
            Ok(()) => {
                trouble.set(None);
                settle(&id, catalog, trouble).await;
            }
            Err(why) => trouble.set(Some(why)),
        }
        busy.set(None);
    });
}

/// Keeps refreshing the catalog until this extension's own action has visibly finished, instead
/// of clearing `busy` — and so the button — the instant the HTTP request returns. "Installed"
/// (`entry.installed`, which decides Install vs. Uninstall) turns true as soon as the package is
/// on disk and supervised; for most extensions that's also when they're ready, but Zigbee
/// downloads Node.js and installs Zigbee2MQTT the first time it runs, entirely after that point
/// — `state` stays `starting`/`degraded` while that's happening. Showing a plain, clickable
/// "Uninstall" before that settles reads as the extension being ready when it isn't.
async fn settle(id: &str, catalog: RwSignal<Vec<CatalogEntry>>, trouble: RwSignal<Option<String>>) {
    let mut waited = Duration::ZERO;
    loop {
        let entries = match api::fetch_catalog().await {
            Ok(entries) => entries,
            Err(why) => {
                trouble.set(Some(why));
                return;
            }
        };
        let still_settling = entries
            .iter()
            .find(|entry| entry.id == id)
            .is_some_and(|entry| matches!(entry.state.as_deref(), Some("starting" | "degraded")));
        catalog.set(entries);
        if !still_settling || waited >= SETTLE_TIMEOUT {
            return;
        }
        gloo_timers::future::sleep(SETTLE_POLL).await;
        waited += SETTLE_POLL;
    }
}

/// What a protocol found that Irori has no entity kind for, and that isn't on a device in the
/// home (those are listed on their device's page): "Also found a cover and a fan, which Irori
/// doesn't support yet."
fn unplaced_unmodeled(id: &str) -> Option<String> {
    let live = expect_context::<crate::Live>();
    live.home.with(|home| {
        let extension = home
            .extensions
            .iter()
            .find(|(extension, _)| extension.as_str() == id)
            .map(|(_, extension)| extension)?;
        // A protocol extension's devices carry its id as their protocol.
        let in_home = |device: &irori_types::UniqueId| {
            home.devices
                .iter()
                .any(|d| &d.unique_id == device && d.protocol.as_str() == id)
        };
        let mut platforms: Vec<String> = extension
            .unmodeled
            .iter()
            .filter(|entry| !entry.device_unique_id.as_ref().is_some_and(in_home))
            .map(|entry| entry.platform.as_str().replace('_', " "))
            .collect();
        platforms.sort();
        platforms.dedup();
        (!platforms.is_empty()).then(|| {
            format!(
                "Also found {}, which Irori doesn't support yet.",
                platforms.join(", ")
            )
        })
    })
}

/// Whether a newer version than the one installed is there to update to.
fn outdated(entry: &CatalogEntry) -> bool {
    entry
        .installed_version
        .as_ref()
        .is_some_and(|installed| *installed != entry.version)
}

/// The version to show for an extension: the one running, and the newer one when the catalog
/// has moved on. An installed extension stays as it was until it's updated.
fn version_words(entry: &CatalogEntry) -> String {
    match &entry.installed_version {
        Some(installed) if *installed != entry.version => {
            format!("{installed} · {} available", entry.version)
        }
        Some(installed) => installed.clone(),
        None => entry.version.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(id, value)| ((*id).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn the_mark_is_for_trouble_nobody_has_cleared() {
        let none = BTreeMap::new();
        // Nothing wrong, no mark.
        assert!(!troubled("demo", None, &none, &none));
        // An error in its log, or Irori saying it failed, is a mark.
        let errors = map(&[("zigbee", "12:00 ERROR no dongle")]);
        assert!(troubled("zigbee", None, &errors, &none));
        assert!(troubled("cast", Some("exited"), &none, &none));
        // Somebody else's error isn't.
        assert!(!troubled("demo", None, &errors, &none));
        // Cleared, it stays off...
        let cleared = map(&[("zigbee", &trouble_of(None, Some("12:00 ERROR no dongle")))]);
        assert!(!troubled("zigbee", None, &errors, &cleared));
        // ...until the trouble is a different one: a newer error, or a failure.
        let newer = map(&[("zigbee", "12:05 ERROR no dongle")]);
        assert!(troubled("zigbee", None, &newer, &cleared));
        assert!(troubled("zigbee", Some("exited"), &errors, &cleared));
    }
}
