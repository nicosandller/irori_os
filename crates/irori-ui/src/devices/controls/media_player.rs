//! A TV, a speaker, or a receiver: what it's doing, buttons for playback, and a volume and a
//! URL when it has them.

use irori_types::{Entity, MediaPlayerCapabilities, MediaPlayerState, Playback, State};
use leptos::prelude::*;

use super::{UNKNOWN, fill};
use crate::devices::Controls;

/// What it's doing, in words: "Playing · The evening news", "Idle".
pub(crate) fn media_player_words(player: &MediaPlayerState) -> String {
    let word = match player.state {
        Playback::Off => "Off",
        Playback::Idle => "Idle",
        Playback::Playing => "Playing",
        Playback::Paused => "Paused",
        Playback::Buffering => "Buffering",
        Playback::Standby => "Standby",
    };
    if let Some(title) = player.title.as_deref().filter(|title| !title.is_empty()) {
        format!("{word} · {title}")
    } else if let Some(app) = player.app.as_deref().filter(|app| !app.is_empty()) {
        format!("{word} · {app}")
    } else {
        word.to_owned()
    }
}

pub(crate) fn media_player_control(
    entity: &Entity,
    capabilities: &MediaPlayerCapabilities,
    value: Option<&State>,
    offline: bool,
    controls: Controls,
) -> AnyView {
    let current = match value {
        Some(State::MediaPlayer(player)) => Some(player.clone()),
        _ => None,
    };
    let entity_id = entity.id.clone();
    let disable = {
        let entity_id = entity_id.clone();
        move || offline || controls.busy.get().contains(&entity_id)
    };
    let button = |label: &'static str, action: &'static str, data: Option<serde_json::Value>| {
        let entity_id = entity_id.clone();
        let disable = disable.clone();
        view! {
            <button
                type="button"
                class="press"
                disabled=disable
                on:click=move |_| controls.act.run((entity_id.clone(), action, data.clone()))
            >
                {label}
            </button>
        }
    };
    let volume = capabilities.volume.then(|| {
        let at = current
            .as_ref()
            .and_then(|player| player.volume)
            .unwrap_or(0);
        let entity_id = entity_id.clone();
        let disable = disable.clone();
        let dragged = RwSignal::new(at);
        view! {
            <label class="dim" title="Volume">
                <span class="lv">{move || format!("{}%", dragged.get())}</span>
                <input
                    type="range"
                    min="0"
                    max="100"
                    step="1"
                    aria-label=format!("Volume for {}", entity.name)
                    prop:value=at.to_string()
                    style:--fill=move || format!("{}%", fill(dragged.get(), 0, 100))
                    disabled=disable
                    on:input:target=move |ev| {
                        if let Ok(value) = ev.target().value().parse::<u8>() {
                            dragged.set(value);
                        }
                    }
                    on:change:target=move |ev| {
                        if let Ok(value) = ev.target().value().parse::<u8>() {
                            controls.act.run((
                                entity_id.clone(),
                                "volume_set",
                                Some(serde_json::json!({ "volume": value })),
                            ));
                        }
                    }
                />
            </label>
        }
    });
    let play_media = capabilities.play_media.then(|| {
        let entity_id = entity_id.clone();
        let disable = disable.clone();
        view! {
            <input
                class="text-control media-url"
                type="url"
                aria-label=format!("Play a URL on {}", entity.name)
                placeholder="https://"
                prop:value=""
                disabled=disable
                on:change:target=move |ev| {
                    let url = ev.target().value();
                    if !url.trim().is_empty() {
                        controls.act.run((
                            entity_id.clone(),
                            "play_media",
                            Some(serde_json::json!({
                                "content_type": "video",
                                "content_id": url,
                            })),
                        ));
                    }
                }
            />
        }
    });
    let muted = current
        .as_ref()
        .and_then(|player| player.muted)
        .unwrap_or(false);
    let words = current
        .as_ref()
        .map(media_player_words)
        .unwrap_or_else(|| UNKNOWN.to_owned());
    view! {
        <>
            <span class="reading">{words}</span>
            <span class="cover-buttons">
                {button("Play", "media_play", None)}
                {button("Pause", "media_pause", None)}
                {button("Stop", "media_stop", None)}
                {capabilities.turn_on.then(|| button("On", "turn_on", None))}
                {capabilities.turn_off.then(|| button("Off", "turn_off", None))}
                {capabilities.mute.then(|| {
                    button(
                        if muted { "Unmute" } else { "Mute" },
                        "volume_mute",
                        Some(serde_json::json!({ "mute": !muted })),
                    )
                })}
            </span>
            {(volume.is_some() || play_media.is_some())
                .then(|| view! { <span class="light-controls">{volume}{play_media}</span> })}
        </>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use irori_types::{MediaPlayerState, Playback};

    use super::media_player_words;

    fn player(state: Playback, title: Option<&str>, app: Option<&str>) -> MediaPlayerState {
        MediaPlayerState {
            state,
            volume: Some(40),
            muted: Some(false),
            title: title.map(str::to_owned),
            artist: None,
            album: None,
            app: app.map(str::to_owned),
            content_type: None,
            duration: None,
            position: None,
        }
    }

    #[test]
    fn playback_is_said_with_the_title_or_the_app() {
        assert_eq!(
            media_player_words(&player(Playback::Playing, Some("The evening news"), None)),
            "Playing · The evening news"
        );
        assert_eq!(
            media_player_words(&player(
                Playback::Idle,
                None,
                Some("Default Media Receiver")
            )),
            "Idle · Default Media Receiver"
        );
        assert_eq!(
            media_player_words(&player(Playback::Standby, Some(""), None)),
            "Standby"
        );
        assert_eq!(
            media_player_words(&player(Playback::Off, None, None)),
            "Off"
        );
        assert_eq!(
            media_player_words(&player(Playback::Paused, None, None)),
            "Paused"
        );
    }
}
