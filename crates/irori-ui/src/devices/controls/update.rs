//! An update: what's installed, what's on offer, and how far an install has got.
//!
//! Installing rewrites what the device runs and takes it away while it does, so it asks first,
//! and the same window is where a version is passed over. An install that has to wait for
//! another on the same protocol says how many go first.

use irori_types::{Entity, State, UpdateCapabilities, UpdateState};
use leptos::portal::Portal;
use leptos::prelude::*;

use super::media_player::shell;
use super::{UNKNOWN, tuck};
use crate::devices::Controls;

/// How an update stands, in one word for the bar and the buttons to follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Standing {
    /// Hasn't said.
    Unknown,
    UpToDate,
    Available,
    /// Someone passed this version over.
    Skipped,
    /// Asked for, behind this many others.
    Waiting(usize),
    Installing,
}

pub(crate) fn standing(update: Option<&UpdateState>, ahead: Option<usize>) -> Standing {
    let Some(update) = update else {
        return Standing::Unknown;
    };
    if update.in_progress {
        Standing::Installing
    } else if let Some(ahead) = ahead {
        Standing::Waiting(ahead)
    } else if update.is_skipped() {
        Standing::Skipped
    } else if update.available {
        Standing::Available
    } else {
        Standing::UpToDate
    }
}

/// An update as a few words: "Up to date · 1.5.0", "1.4.2 → 1.5.0", "Installing 40%".
pub(crate) fn update_words(update: &UpdateState, ahead: Option<usize>) -> String {
    let installed = update.installed_version.as_deref();
    let latest = update.latest_version.as_deref();
    match standing(Some(update), ahead) {
        Standing::Installing => match update.progress {
            Some(progress) => format!("Installing {}%", progress.round()),
            None => "Installing".to_owned(),
        },
        Standing::Waiting(0) => "Waiting to install".to_owned(),
        Standing::Waiting(ahead) => format!("Waiting, {ahead} ahead"),
        Standing::Skipped => match latest {
            Some(latest) => format!("Skipped {latest}"),
            None => "Skipped".to_owned(),
        },
        Standing::Available => match (installed, latest) {
            (Some(installed), Some(latest)) => format!("{installed} → {latest}"),
            (None, Some(latest)) => format!("{latest} available"),
            _ => "Update available".to_owned(),
        },
        Standing::UpToDate | Standing::Unknown => match installed {
            Some(installed) => format!("Up to date · {installed}"),
            None => "Up to date".to_owned(),
        },
    }
}

/// How many installs go before `entity`'s, when it's waiting its turn.
pub(crate) fn ahead_of(entity: &Entity) -> Signal<Option<usize>> {
    let live = expect_context::<crate::Live>();
    let id = entity.id.clone();
    Signal::derive(move || {
        live.home.with(|home| {
            home.installs
                .iter()
                .find(|queued| queued.entity_id == id)
                .map(|queued| queued.ahead)
        })
    })
}

/// An update's row. Drawn once and kept, so the bar fills as the install goes and the button
/// gives way to it in place.
pub(crate) fn update_control(
    entity: &Entity,
    capabilities: &UpdateCapabilities,
    state: Signal<Option<State>>,
    offline: Signal<bool>,
    controls: Controls,
) -> AnyView {
    let update = Memo::new(move |_| match state.get() {
        Some(State::Update(update)) => Some(update),
        _ => None,
    });
    let ahead = ahead_of(entity);
    let stands = Memo::new(move |_| update.with(|update| standing(update.as_ref(), ahead.get())));
    let entity_id = entity.id.clone();
    let name = entity.name.to_string();
    let (can_install, can_check) = (capabilities.install, capabilities.check);
    let disable = {
        let entity_id = entity_id.clone();
        Signal::derive(move || {
            offline.get() || controls.busy.with(|busy| busy.contains(&entity_id))
        })
    };
    let asking = RwSignal::new(false);
    let send = {
        let entity_id = entity_id.clone();
        move |action: &'static str| {
            controls.act.run((entity_id.clone(), action, None));
            asking.set(false);
        }
    };
    let press = |label: &'static str, on_click: Callback<()>| {
        view! {
            <button
                type="button"
                class="press"
                disabled=move || disable.get()
                on:click=move |_| on_click.run(())
            >
                {label}
            </button>
        }
        .into_any()
    };
    // The window is drawn over the page, not inside the row: a row sits in a group that folds,
    // which clips whatever is in it.
    let window = {
        let send = send.clone();
        move || {
            let update = update.get_untracked().unwrap_or_default();
            let (install, skip) = (send.clone(), send.clone());
            let versions = update_words(&update, None);
            // The software's own name where it has one: an entity is often just "Firmware".
            let what = update.title.clone().unwrap_or_else(|| name.clone());
            view! {
                <crate::modal::Modal
                    title=format!("Update {what}?")
                    on_close=Callback::new(move |()| asking.set(false))
                >
                    <p class="update-versions">{versions}</p>
                    {update.release_summary.clone().map(|summary| view! { <p>{summary}</p> })}
                    {update.release_url.clone().map(|url| view! {
                        <p>
                            <a href=url target="_blank" rel="noopener noreferrer">
                                "Release notes"
                            </a>
                        </p>
                    })}
                    <p class="muted small">
                        "It can't be used while it installs, and restarts when it's done. "
                        "If another update is installing on the same network, this one waits "
                        "its turn."
                    </p>
                    <div class="ext-actions">
                        <button type="button" class="danger" on:click=move |_| asking.set(false)>
                            "Cancel"
                        </button>
                        <button type="button" class="danger" on:click=move |_| skip("skip")>
                            "Skip this version"
                        </button>
                        <button type="button" class="add solid" on:click=move |_| install("install")>
                            "Update"
                        </button>
                    </div>
                </crate::modal::Modal>
            }
        }
    };
    let window = StoredValue::new(window);
    let (undo, check) = (send.clone(), send);
    view! {
        <>
            <span
                class="reading update-reading"
                class:on=move || matches!(stands.get(), Standing::Available | Standing::Installing)
            >
                {move || update.with(|update| match update {
                    Some(update) => update_words(update, ahead.get()),
                    None => UNKNOWN.to_owned(),
                })}
                {tuck(
                    view! {
                        <span
                            class="update-bar"
                            role="progressbar"
                            aria-valuemin="0"
                            aria-valuemax="100"
                            aria-valuenow=move || {
                                update.with(|u| u.as_ref().and_then(|u| u.progress)).map(|p| p.round())
                            }
                            // Without a figure there's nothing to fill to: the bar says only
                            // that something is going on.
                            class:unsaid=move || {
                                update.with(|u| u.as_ref().is_none_or(|u| u.progress.is_none()))
                            }
                        >
                            <span
                                class="update-bar-fill"
                                style:width=move || {
                                    let progress = update
                                        .with(|u| u.as_ref().and_then(|u| u.progress))
                                        .unwrap_or(0.0);
                                    format!("{progress}%")
                                }
                            ></span>
                        </span>
                    }
                    .into_any(),
                    Signal::derive(move || stands.get() == Standing::Installing),
                )}
            </span>
            {move || update.with(|u| u.as_ref().and_then(|u| u.last_error.clone())).map(|why| view! {
                <span class="update-error small" role="status">{why}</span>
            })}
            <span class="cover-buttons">
                {can_install.then(|| tuck(
                    press("Update", Callback::new(move |()| asking.set(true))),
                    Signal::derive(move || stands.get() == Standing::Available),
                ))}
                {tuck(
                    press("Undo", Callback::new(move |()| undo("clear_skipped"))),
                    Signal::derive(move || stands.get() == Standing::Skipped),
                )}
                {can_check.then(|| tuck(
                    press("Check", Callback::new(move |()| check("check"))),
                    Signal::derive(move || stands.get() == Standing::UpToDate),
                ))}
            </span>
            <Show when=move || asking.get()>
                <Portal mount=shell()>{window.with_value(|window| window())}</Portal>
            </Show>
        </>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(installed: &str, latest: &str) -> UpdateState {
        UpdateState {
            available: installed != latest,
            installed_version: Some(installed.into()),
            latest_version: Some(latest.into()),
            ..UpdateState::default()
        }
    }

    #[test]
    fn an_update_says_where_it_stands() {
        assert_eq!(
            update_words(&at("1.5.0", "1.5.0"), None),
            "Up to date · 1.5.0"
        );
        assert_eq!(update_words(&at("1.4.2", "1.5.0"), None), "1.4.2 → 1.5.0");
        assert_eq!(
            update_words(&at("1.4.2", "1.5.0"), Some(2)),
            "Waiting, 2 ahead"
        );
        let installing = UpdateState {
            in_progress: true,
            progress: Some(39.6),
            ..at("1.4.2", "1.5.0")
        };
        // Installing wins over a place in the line it has just left.
        assert_eq!(update_words(&installing, Some(1)), "Installing 40%");
        let skipped = UpdateState {
            available: false,
            skipped_version: Some("1.5.0".into()),
            ..at("1.4.2", "1.5.0")
        };
        assert_eq!(update_words(&skipped, None), "Skipped 1.5.0");
        assert_eq!(standing(None, None), Standing::Unknown);
    }
}
