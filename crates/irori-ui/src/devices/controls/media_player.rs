use irori_types::{Entity, MediaPlayerCapabilities, MediaPlayerState, Playback, State};
use leptos::prelude::*;

use super::{UNKNOWN, fill, glyph, tuck};
use crate::devices::Controls;
use crate::icons::Icon;

/// What a player is doing, and what, in the words its row and its history both use.
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

/// Which of a player's buttons make sense right now. Nobody wants Play on something already
/// playing, or a volume slider on a television that's off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Offered {
    /// The one transport button is Pause rather than Play.
    pub pause: bool,
    /// There is something to play, pause or stop: it isn't off.
    pub transport: bool,
    pub stop: bool,
    /// Skipping back and forward through a queue, where it has one.
    pub skip: bool,
    /// Whether the power button turns it on (`true`) or off, where it can do that one at all.
    pub power: Option<bool>,
    /// Volume, mute and playing something new.
    pub sound: bool,
}

pub(crate) fn offered(
    capabilities: &MediaPlayerCapabilities,
    playback: Option<Playback>,
) -> Offered {
    let asleep = matches!(playback, Some(Playback::Off | Playback::Standby));
    let playing = matches!(playback, Some(Playback::Playing | Playback::Buffering));
    Offered {
        pause: playing,
        transport: !asleep,
        stop: playing || playback == Some(Playback::Paused),
        skip: capabilities.queue && (playing || playback == Some(Playback::Paused)),
        power: if asleep {
            capabilities.turn_on.then_some(true)
        } else {
            capabilities.turn_off.then_some(false)
        },
        sound: !asleep,
    }
}

/// A player's row. Drawn once and kept: what it's doing arrives through `state`, and the
/// buttons change in place — Play becomes Pause, Stop comes and goes — rather than the row
/// being drawn again around a finger on the volume.
pub(crate) fn media_player_control(
    entity: &Entity,
    capabilities: &MediaPlayerCapabilities,
    state: Signal<Option<State>>,
    offline: Signal<bool>,
    controls: Controls,
) -> AnyView {
    let player = Memo::new(move |_| match state.get() {
        Some(State::MediaPlayer(player)) => Some(player),
        _ => None,
    });
    let capabilities = capabilities.clone();
    let can = {
        let capabilities = capabilities.clone();
        Memo::new(move |_| {
            offered(
                &capabilities,
                player.with(|player| player.as_ref().map(|player| player.state)),
            )
        })
    };
    let muted = Memo::new(move |_| {
        player.with(|player| {
            player
                .as_ref()
                .and_then(|player| player.muted)
                .unwrap_or(false)
        })
    });
    let entity_id = entity.id.clone();
    let disable = {
        let entity_id = entity_id.clone();
        Signal::derive(move || {
            offline.get() || controls.busy.with(|busy| busy.contains(&entity_id))
        })
    };
    let act = {
        let entity_id = entity_id.clone();
        move |action: &'static str, data: Option<serde_json::Value>| {
            controls.act.run((entity_id.clone(), action, data));
        }
    };

    let transport = {
        let act = act.clone();
        glyph(
            (Icon::Play, Icon::Pause),
            Signal::derive(move || can.get().pause),
            Signal::derive(move || if can.get().pause { "Pause" } else { "Play" }.to_owned()),
            disable,
            move || {
                let action = if can.get_untracked().pause {
                    "media_pause"
                } else {
                    "media_play"
                };
                act(action, None);
            },
        )
    };
    let stop = {
        let act = act.clone();
        glyph(
            (Icon::Stop, Icon::Stop),
            Signal::stored(false),
            Signal::stored("Stop".to_owned()),
            disable,
            move || act("media_stop", None),
        )
    };
    let skip = |icon: Icon, label: &'static str, action: &'static str| {
        let act = act.clone();
        let button = glyph(
            (icon, icon),
            Signal::stored(false),
            Signal::stored(label.to_owned()),
            disable,
            move || act(action, None),
        );
        tuck(button, Signal::derive(move || can.get().skip))
    };
    let previous = capabilities
        .queue
        .then(|| skip(Icon::Previous, "Previous", "media_previous_track"));
    let next = capabilities
        .queue
        .then(|| skip(Icon::Next, "Next", "media_next_track"));
    let mute = capabilities.mute.then(|| {
        let act = act.clone();
        glyph(
            (Icon::Volume, Icon::Muted),
            muted.into(),
            Signal::derive(move || if muted.get() { "Unmute" } else { "Mute" }.to_owned()),
            disable,
            move || {
                let mute = !muted.get_untracked();
                act("volume_mute", Some(serde_json::json!({ "mute": mute })));
            },
        )
    });
    let power = (capabilities.turn_on || capabilities.turn_off).then(|| {
        let act = act.clone();
        let button = glyph(
            (Icon::Power, Icon::Power),
            Signal::stored(false),
            Signal::derive(move || {
                if can.get().power == Some(true) {
                    "Turn on"
                } else {
                    "Turn off"
                }
                .to_owned()
            }),
            disable,
            move || match can.get_untracked().power {
                Some(true) => act("turn_on", None),
                Some(false) => act("turn_off", None),
                None => {}
            },
        );
        tuck(button, Signal::derive(move || can.get().power.is_some()))
    });

    let volume = capabilities.volume.then(|| {
        let level = move || {
            player.with(|player| {
                player
                    .as_ref()
                    .and_then(|player| player.volume)
                    .unwrap_or(0)
            })
        };
        // What the slider shows: where the player says it is, and where the finger is while
        // it's being dragged.
        let dragged = RwSignal::new(level());
        Effect::new(move |_| dragged.set(level()));
        let act = act.clone();
        view! {
            <label class="dim" title="Volume">
                <span class="lv">{move || format!("{}%", dragged.get())}</span>
                <input
                    type="range"
                    min="0"
                    max="100"
                    step="1"
                    aria-label=format!("Volume for {}", entity.name)
                    prop:value=move || dragged.get().to_string()
                    style:--fill=move || format!("{}%", fill(dragged.get(), 0, 100))
                    disabled=move || disable.get()
                    on:input:target=move |ev| {
                        if let Ok(value) = ev.target().value().parse::<u8>() {
                            dragged.set(value);
                        }
                    }
                    on:change:target=move |ev| {
                        if let Ok(value) = ev.target().value().parse::<u8>() {
                            act("volume_set", Some(serde_json::json!({ "volume": value })));
                        }
                    }
                />
            </label>
        }
    });
    let play_media = capabilities.play_media.then(|| {
        let act = act.clone();
        view! {
            <input
                class="text-control media-url"
                type="url"
                aria-label=format!("Play a URL on {}", entity.name)
                placeholder="Play a link: https://"
                prop:value=""
                disabled=move || disable.get()
                on:change:target=move |ev| {
                    let url = ev.target().value();
                    if !url.trim().is_empty() {
                        act(
                            "play_media",
                            Some(serde_json::json!({
                                "content_type": "video",
                                "content_id": url,
                            })),
                        );
                        ev.target().set_value("");
                    }
                }
            />
        }
    });
    let words = move || {
        player.with(|player| {
            player
                .as_ref()
                .map(media_player_words)
                .unwrap_or_else(|| UNKNOWN.to_owned())
        })
    };
    let has_more = volume.is_some() || play_media.is_some();
    view! {
        <>
            <span class="reading" class:on=move || can.get().pause>{words}</span>
            <span class="cover-buttons">
                {previous}
                {tuck(transport, Signal::derive(move || can.get().transport))}
                {next}
                {tuck(stop, Signal::derive(move || can.get().stop))}
                {mute.map(|mute| tuck(mute, Signal::derive(move || can.get().sound)))}
                {power}
            </span>
            {has_more.then(|| view! {
                <span class="light-controls" hidden=move || !can.get().sound>
                    {volume}
                    {play_media}
                </span>
            })}
        </>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use irori_types::{MediaPlayerCapabilities, MediaPlayerState, Playback};

    use super::{media_player_words, offered};

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

    #[test]
    fn a_player_offers_only_what_makes_sense_now() {
        let able = |yes: bool| MediaPlayerCapabilities {
            device_class: None,
            volume: yes,
            mute: yes,
            seek: yes,
            play_media: yes,
            queue: yes,
            turn_on: yes,
            turn_off: yes,
        };
        let all = able(true);
        // Playing: pause it, stop it, turn it off. Nobody presses Play.
        let playing = offered(&all, Some(Playback::Playing));
        assert!(playing.pause && playing.stop && playing.sound && playing.skip);
        assert_eq!(playing.power, Some(false));
        // Paused: play it again, or stop.
        let paused = offered(&all, Some(Playback::Paused));
        assert!(!paused.pause && paused.stop);
        // Idle: there's nothing to stop.
        assert!(!offered(&all, Some(Playback::Idle)).stop);
        // Off: the only thing to do is turn it on.
        let off = offered(&all, Some(Playback::Off));
        assert!(!off.transport && !off.stop && !off.sound && !off.skip);
        assert_eq!(off.power, Some(true));
        // One that can't be turned on has no power button while it's off.
        assert_eq!(offered(&able(false), Some(Playback::Standby)).power, None);
    }
}
