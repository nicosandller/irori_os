//! The welcome's "Connect to Home Assistant" step: a trial of Irori on a home that Home
//! Assistant already runs, set up in one go.
//!
//! Given Home Assistant's address and a long-lived access token, this installs the Home
//! Assistant extension, saves its settings, waits for it to read Home Assistant, brings over
//! its floors and rooms, and adds every device it found. It does it all with the calls the
//! rest of the page already makes (install, settings, the layout offer, Add all), so nothing
//! here is new to the server, and a home that never picks this step never runs any of it.
//!
//! Home Assistant is only read, as the extension does; the commands are the extension's, and
//! stay on unless "Only look" is ticked.

use std::time::Duration;

use irori_types::SuggestedLayout;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{self, Home};

/// The extension's id.
const ID: &str = "homeassistant";
/// How often the extension's state is read while it connects.
const POLL: Duration = Duration::from_millis(1500);
/// How long it gets to read Home Assistant before the step says it hasn't.
const CONNECT_FOR: Duration = Duration::from_secs(60);
/// What it says when the address is the kind of one Home Assistant answers at on most homes.
pub const ADDRESS_HINT: &str = "http://homeassistant.local:8123";
/// Said under an `http://` address, as the extension's own settings page says it.
const NOT_ENCRYPTED: &str = "Not encrypted: your Home Assistant administrator's access token travels unencrypted on your local network. Use https:// if your Home Assistant has a certificate.";

/// What came of it, for the last step to say back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Brought {
    pub devices: usize,
    pub rooms: usize,
}

/// What the extension is doing, read from the home.
#[derive(Debug, Clone, PartialEq)]
pub enum Reading {
    /// Not there yet, or still trying; `why` is what it last said it was waiting for.
    Connecting { why: Option<String> },
    /// It can't go on until the owner does something: its message says what.
    Stuck(String),
    /// It has read Home Assistant: how many devices it holds, and the rooms it proposes.
    Running {
        held: usize,
        layout: Option<SuggestedLayout>,
    },
}

/// Reads how the extension is faring from the home.
pub fn read(home: &Home) -> Reading {
    let Some((_, extension)) = home.extensions.iter().find(|(id, _)| id.as_str() == ID) else {
        return Reading::Connecting { why: None };
    };
    // A token with too few rights signs in and then can't read the registries: the extension
    // says what it needs, and nothing will change until the owner gives another.
    if let Some(waiting) = extension.waiting.first() {
        return Reading::Stuck(waiting.reason.clone());
    }
    match extension.state.as_str() {
        "running" => Reading::Running {
            held: home.held.iter().filter(|h| h.protocol == ID).count(),
            layout: extension
                .suggested_layout
                .clone()
                .filter(|layout| !layout.is_empty()),
        },
        "failed" => Reading::Stuck(
            extension
                .reason
                .clone()
                .unwrap_or_else(|| "The Home Assistant extension stopped.".to_owned()),
        ),
        _ => Reading::Connecting {
            why: extension.reason.clone(),
        },
    }
}

/// The rooms the Home Assistant devices ended up in, after they were added.
pub fn brought(home: &Home) -> Brought {
    let mut rooms = std::collections::BTreeSet::new();
    let mut devices = 0;
    for device in home.devices.iter().filter(|d| d.protocol.as_str() == ID) {
        devices += 1;
        if let Some(area) = &device.area_id {
            rooms.insert(area.clone());
        }
    }
    Brought {
        devices,
        rooms: rooms.len(),
    }
}

/// "12 devices in 4 rooms", "1 device".
pub fn said(brought: &Brought) -> String {
    let devices = match brought.devices {
        1 => "1 device".to_owned(),
        n => format!("{n} devices"),
    };
    match brought.rooms {
        0 => devices,
        1 => format!("{devices} in 1 room"),
        n => format!("{devices} in {n} rooms"),
    }
}

/// The settings to save: the address, the token and whether Irori may send commands.
pub fn settings(
    url: &str,
    token: &str,
    only_look: bool,
) -> serde_json::Map<String, serde_json::Value> {
    let mut settings = serde_json::Map::new();
    settings.insert("url".into(), url.trim().into());
    settings.insert("token".into(), token.trim().into());
    settings.insert("commands".into(), (!only_look).into());
    settings
}

#[component]
pub fn ConnectHomeAssistant(
    /// Leaves the step: with what was brought over, or without when it was skipped.
    done: Callback<Option<Brought>>,
) -> impl IntoView {
    let live = expect_context::<crate::Live>();
    let url = RwSignal::new(String::new());
    let token = RwSignal::new(String::new());
    let only_look = RwSignal::new(false);
    // What it is doing now, in a sentence, while it works.
    let working = RwSignal::new(None::<String>);
    // What it is waiting for from Home Assistant, shown under that.
    let waiting_for = RwSignal::new(None::<String>);
    let trouble = RwSignal::new(None::<String>);
    // Counts tries, so one that was given up on stops when the next begins.
    let attempt = RwSignal::new(0u32);

    let connect = move || {
        if working.get_untracked().is_some() {
            return;
        }
        let (address, secret) = (url.get_untracked(), token.get_untracked());
        if address.trim().is_empty() || secret.trim().is_empty() {
            return trouble.set(Some(
                "Home Assistant's address and an access token are both needed.".to_owned(),
            ));
        }
        let this = attempt.get_untracked() + 1;
        attempt.set(this);
        let only = only_look.get_untracked();
        trouble.set(None);
        waiting_for.set(None);
        spawn_local(async move {
            let still_here = move || attempt.try_get_untracked() == Some(this);
            let stop = |why: String| {
                let _ = trouble.try_set(Some(why));
                let _ = working.try_set(None);
            };
            let _ = working.try_set(Some("Looking at your home…".to_owned()));
            let installed = match api::fetch_home().await {
                Ok(home) => home.extensions.keys().any(|id| id.as_str() == ID),
                Err(why) => return stop(why),
            };
            if !installed {
                let _ = working.try_set(Some(
                    "Installing the Home Assistant extension. The first time can take a few \
                     minutes."
                        .to_owned(),
                ));
                match api::install_extension(ID, false, false).await {
                    Ok(()) => {}
                    // Another tab or an earlier try is installing it: wait for it below.
                    Err(refused) if refused.underway.is_some() => {}
                    Err(refused) => return stop(refused.error),
                }
            }
            if !still_here() {
                return;
            }
            let _ = working.try_set(Some("Signing in to Home Assistant…".to_owned()));
            if let Err(why) =
                api::set_extension_settings(ID, &settings(&address, &secret, only)).await
            {
                return stop(why);
            }
            // The extension starts again with what was saved; what it said before is out of
            // date until it has.
            gloo_timers::future::sleep(Duration::from_secs(2)).await;

            let mut waited = Duration::ZERO;
            let mut settled = 0u32;
            let mut seen = None::<usize>;
            let found = loop {
                if !still_here() {
                    return;
                }
                let home = match api::fetch_home().await {
                    Ok(home) => home,
                    Err(why) => return stop(why),
                };
                match read(&home) {
                    Reading::Stuck(why) => return stop(why),
                    Reading::Connecting { why } => {
                        let _ = waiting_for.try_set(why);
                        settled = 0;
                        seen = None;
                    }
                    Reading::Running { held, layout } => {
                        let _ = waiting_for.try_set(None);
                        let _ = working.try_set(Some("Reading your Home Assistant…".to_owned()));
                        // It describes its devices over a few moments: it's all there once
                        // the count has stopped moving. An empty one is given a few seconds.
                        settled = if seen == Some(held) { settled + 1 } else { 0 };
                        seen = Some(held);
                        if settled >= 2 && (held > 0 || layout.is_some() || waited >= POLL * 6) {
                            break (held, layout);
                        }
                    }
                }
                if waited >= CONNECT_FOR {
                    let why = waiting_for
                        .try_get_untracked()
                        .flatten()
                        .unwrap_or_else(|| "Home Assistant didn't answer in time.".to_owned());
                    return stop(format!(
                        "Couldn't connect to Home Assistant: {why} Check the address and the \
                         token, and that Home Assistant is on."
                    ));
                }
                gloo_timers::future::sleep(POLL).await;
                waited += POLL;
            };

            if !still_here() {
                return;
            }
            let (held, layout) = found;
            // The rooms first, so each device is added into its own.
            if let Some(layout) = layout {
                let _ = working.try_set(Some("Bringing over your rooms and floors…".to_owned()));
                let extension = irori_types::ExtensionId::try_from(ID).expect("a valid id");
                if let Err(why) = api::accept_layout(&extension, &layout).await {
                    return stop(format!("Couldn't bring your rooms over: {why}"));
                }
            }
            if held > 0 {
                let _ = working.try_set(Some("Adding your devices…".to_owned()));
                let extension = irori_types::ExtensionId::try_from(ID).expect("a valid id");
                if let Err(why) = api::add_found_devices(&extension, None).await {
                    return stop(format!("Couldn't add your devices: {why}"));
                }
            }
            crate::reload(live).await;
            let result = live.home.with_untracked(brought);
            let _ = token.try_set(String::new());
            let _ = working.try_set(None);
            done.run(Some(result));
        });
    };

    let cancel = move |_| {
        // Whatever is under way gives up at its next look.
        attempt.update(|n| *n += 1);
        working.set(None);
        waiting_for.set(None);
    };

    view! {
        <form
            class="welcome-step add-step"
            on:submit=move |event| {
                event.prevent_default();
                connect();
            }
        >
            <h1>"Connect to Home Assistant"</h1>
            <p class="welcome-lede">
                "Try IroriOS on the home Home Assistant already runs. Its devices, rooms and \
                 floors come over, and Home Assistant keeps running them. Nothing changes in \
                 Home Assistant's setup."
            </p>
            <label class="settings-field">
                "Home Assistant's address"
                <input
                    type="text"
                    inputmode="url"
                    autocomplete="off"
                    autocapitalize="off"
                    spellcheck="false"
                    placeholder=ADDRESS_HINT
                    disabled=move || working.with(Option::is_some)
                    prop:value=move || url.get()
                    on:input=move |event| url.set(event_target_value(&event))
                />
            </label>
            {move || url.with(|url| url.trim_start().starts_with("http://")).then(|| view! {
                <p class="muted small">{NOT_ENCRYPTED}</p>
            })}
            <label class="settings-field">
                "A long-lived access token"
                <input
                    type="password"
                    autocomplete="off"
                    spellcheck="false"
                    disabled=move || working.with(Option::is_some)
                    prop:value=move || token.get()
                    on:input=move |event| token.set(event_target_value(&event))
                />
            </label>
            <p class="muted small">
                "In Home Assistant, signed in as an administrator: your profile, then Security, \
                 then Long-lived access tokens, then Create token. Only an administrator's can \
                 read the devices and rooms."
            </p>
            <label class="pick-all" style="white-space: normal">
                <input
                    type="checkbox"
                    disabled=move || working.with(Option::is_some)
                    prop:checked=move || only_look.get()
                    on:change=move |event| only_look.set(event_target_checked(&event))
                />
                " Only look: don't let IroriOS control anything in Home Assistant"
            </label>
            {move || working.get().map(|what| view! {
                <p class="muted small" role="status">
                    {what}
                    {move || waiting_for.get().map(|why| format!(" Home Assistant says: {why}"))}
                </p>
            })}
            {move || trouble.get().map(|why| view! { <p class="why">{why}</p> })}
            <div class="welcome-actions">
                {move || if working.with(Option::is_some) {
                    view! {
                        <button type="button" class="welcome-later" on:click=cancel>
                            "Stop and change it"
                        </button>
                    }
                    .into_any()
                } else {
                    view! {
                        <button
                            type="button"
                            class="welcome-later"
                            on:click=move |_| done.run(None)
                        >
                            "Skip for now"
                        </button>
                        <button type="submit" class="welcome-next">"Connect"</button>
                    }
                    .into_any()
                }}
            </div>
        </form>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_says_what_came_over() {
        let said = |devices, rooms| said(&Brought { devices, rooms });
        assert_eq!(said(12, 4), "12 devices in 4 rooms");
        assert_eq!(said(1, 1), "1 device in 1 room");
        assert_eq!(said(3, 0), "3 devices");
    }

    #[test]
    fn it_saves_what_was_typed_without_stray_spaces() {
        let saved = settings(" http://ha.local:8123 ", " abc ", true);
        assert_eq!(saved["url"], "http://ha.local:8123");
        assert_eq!(saved["token"], "abc");
        assert_eq!(saved["commands"], false);
        assert_eq!(settings("a", "b", false)["commands"], true);
    }

    #[test]
    fn before_the_extension_is_there_it_is_connecting() {
        assert_eq!(read(&Home::default()), Reading::Connecting { why: None });
    }
}
