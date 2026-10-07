use irori_types::{Entity, MediaPlayerCapabilities, MediaPlayerState, Playback, State};
use leptos::portal::Portal;
use leptos::prelude::*;
use web_sys::wasm_bindgen::JsCast;

use super::{UNKNOWN, fill, glyph, tuck};
use crate::devices::Controls;
use crate::icons::{Icon, icon};

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
    /// Volume and mute.
    pub sound: bool,
    /// Sending it a link to play, where it takes one.
    pub cast: bool,
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
        cast: capabilities.play_media && !asleep,
    }
}

/// The height the cast popover needs under its button, in CSS pixels, before it opens upward.
const POPOVER_ROOM: f64 = 120.0;

/// Where the cast popover is drawn: the shell, so that Settings turning motion off reaches it.
fn shell() -> web_sys::Element {
    let document = document();
    document
        .query_selector(".shell")
        .ok()
        .flatten()
        .or_else(|| document.body().map(Into::into))
        .expect("the page has a body")
}

/// What the volume group says beside its slider.
pub(crate) fn volume_label(volume: u8, muted: bool) -> String {
    if muted {
        "Muted".to_owned()
    } else {
        format!("{volume}%")
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

    let slider = capabilities.volume.then(|| {
        let level = move || {
            player.with(|player| {
                player
                    .as_ref()
                    .and_then(|player| player.volume)
                    .unwrap_or(0)
            })
        };
        // What the slider shows: where the player says it is, and where the finger is while
        // it's being dragged. Muting empties the track and leaves the thumb where it was.
        let dragged = RwSignal::new(level());
        Effect::new(move |_| dragged.set(level()));
        let act = act.clone();
        view! {
            <input
                type="range"
                min="0"
                max="100"
                step="1"
                aria-label=format!("Volume for {}", entity.name)
                prop:value=move || dragged.get().to_string()
                style:--fill=move || {
                    let shown = if muted.get() { 0 } else { dragged.get() };
                    format!("{}%", fill(shown, 0, 100))
                }
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
            <span class="lv">{move || volume_label(dragged.get(), muted.get())}</span>
        }
    });
    let volume = (mute.is_some() || slider.is_some()).then(|| {
        let group = view! { <span class="volume-group" class:muted=move || muted.get() title="Volume">{mute}{slider}</span> };
        tuck(group.into_any(), Signal::derive(move || can.get().sound))
    });
    let cast = capabilities.play_media.then(|| {
        let act = act.clone();
        let open = RwSignal::new(false);
        let around = NodeRef::<leptos::html::Span>::new();
        let button = NodeRef::<leptos::html::Button>::new();
        let field = NodeRef::<leptos::html::Input>::new();
        // Closing hands the keyboard back to the button that opened it.
        let close = move || {
            if open.get_untracked() {
                open.set(false);
                if let Some(button) = button.get_untracked() {
                    let _ = button.focus();
                }
            }
        };
        // A player that goes off takes its popover with it.
        Effect::new(move |_| {
            if !can.get().cast {
                open.set(false);
            }
        });
        Effect::new(move |_| {
            if let Some(field) = field.get() {
                let _ = field.focus();
            }
        });
        // Where the popover hangs: under the button and flush with its right edge, or above it
        // when the row is at the foot of the window. Measured as it opens, pinned to the window
        // and drawn outside the row, because the lists a row sits in cut off whatever overflows
        // them.
        let place = RwSignal::new((String::new(), String::new(), String::new()));
        let measure = move || {
            let (Some(button), Some(page)) =
                (button.get_untracked(), document().document_element())
            else {
                return;
            };
            // The page without its scrollbar, which is what a pinned box is placed against.
            let (width, height) = (
                f64::from(page.client_width()),
                f64::from(page.client_height()),
            );
            let at = button.get_bounding_client_rect();
            // Flush with the button's right edge, unless that would push the left edge off a
            // narrow page. The width is the stylesheet's: 24rem, or nine tenths of the page.
            let wide = (width * 0.9).min(384.0);
            let right = (width - at.right()).min(width - wide - 8.0).max(0.0);
            let right = format!("{right}px");
            place.set(if height - at.bottom() < POPOVER_ROOM {
                (
                    "auto".to_owned(),
                    format!("{}px", height - at.top() + 8.0),
                    right,
                )
            } else {
                (format!("{}px", at.bottom() + 8.0), "auto".to_owned(), right)
            });
        };
        // Pinned to the window, it has to be moved when the page moves under it.
        let follow = move || {
            if open.get_untracked() {
                measure();
            }
        };
        let scrolled = window_event_listener(leptos::ev::scroll, move |_| follow());
        let resized = window_event_listener(leptos::ev::resize, move |_| follow());
        on_cleanup(move || {
            scrolled.remove();
            resized.remove();
        });
        let show = move || {
            measure();
            open.set(true);
        };
        let outside = window_event_listener(leptos::ev::pointerdown, move |event| {
            if !open.get_untracked() {
                return;
            }
            let target = event
                .target()
                .and_then(|target| target.dyn_into::<web_sys::Element>().ok());
            let inside = target.is_some_and(|target| {
                around
                    .get_untracked()
                    .is_some_and(|around| around.contains(Some(&target)))
                    || matches!(target.closest(".cast-pop"), Ok(Some(_)))
            });
            if !inside {
                close();
            }
        });
        on_cleanup(move || outside.remove());
        let title = format!("Cast to {}", entity.name);
        let heading = title.clone();
        let popover = move || {
            let act = act.clone();
            view! {
                <form
                    class="cast-pop"
                    role="dialog"
                    aria-label=title.clone()
                    style:top=move || place.get().0
                    style:bottom=move || place.get().1
                    style:right=move || place.get().2
                    on:keydown=move |event| {
                        if event.key() == "Escape" {
                            event.stop_propagation();
                            close();
                        }
                    }
                    on:submit=move |event| {
                        event.prevent_default();
                        let Some(field) = field.get_untracked() else {
                            return;
                        };
                        let url = field.value().trim().to_owned();
                        if url.is_empty() {
                            return;
                        }
                        act(
                            "play_media",
                            Some(serde_json::json!({
                                "content_type": "video",
                                "content_id": url,
                            })),
                        );
                        field.set_value("");
                        close();
                    }
                >
                    <span class="cast-title">{heading.clone()}</span>
                    <span class="cast-row">
                        <input
                            node_ref=field
                            type="url"
                            aria-label="Link to play"
                            placeholder="https://"
                            autofocus
                            disabled=move || disable.get()
                        />
                        <button type="submit" disabled=move || disable.get()>"Play"</button>
                    </span>
                </form>
            }
        };
        let popover = StoredValue::new(popover);
        let both = view! {
            <span class="cast" node_ref=around>
                <button
                    node_ref=button
                    type="button"
                    class="press glyph"
                    aria-label="Cast a link"
                    title="Cast a link"
                    aria-haspopup="dialog"
                    aria-expanded=move || open.get().to_string()
                    disabled=move || disable.get()
                    on:click=move |_| if open.get_untracked() { close() } else { show() }
                >
                    <span class="glyph-first">{icon(Icon::Cast)}</span>
                </button>
                <Show when=move || open.get()>
                    <Portal mount=shell()>{popover.with_value(|popover| popover())}</Portal>
                </Show>
            </span>
        };
        tuck(both.into_any(), Signal::derive(move || can.get().cast))
    });
    let words = move || {
        player.with(|player| {
            player
                .as_ref()
                .map(media_player_words)
                .unwrap_or_else(|| UNKNOWN.to_owned())
        })
    };
    view! {
        <>
            <span class="reading media-reading" class:on=move || can.get().pause title=words>
                {words}
            </span>
            <span class="cover-buttons">
                {previous}
                {tuck(transport, Signal::derive(move || can.get().transport))}
                {next}
                {tuck(stop, Signal::derive(move || can.get().stop))}
                {volume}
                {cast}
                {power}
            </span>
        </>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use irori_types::{MediaPlayerCapabilities, MediaPlayerState, Playback};

    use super::{media_player_words, offered, volume_label};

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
        assert!(playing.cast);
        assert_eq!(playing.power, Some(false));
        // Paused: play it again, or stop.
        let paused = offered(&all, Some(Playback::Paused));
        assert!(!paused.pause && paused.stop);
        // Idle: there's nothing to stop.
        assert!(!offered(&all, Some(Playback::Idle)).stop);
        // Off: the only thing to do is turn it on.
        let off = offered(&all, Some(Playback::Off));
        assert!(!off.transport && !off.stop && !off.sound && !off.skip && !off.cast);
        assert_eq!(off.power, Some(true));
        // One that can't be turned on has no power button while it's off.
        assert_eq!(offered(&able(false), Some(Playback::Standby)).power, None);
        // One that takes no links has no cast button, whatever it's doing.
        assert!(!offered(&able(false), Some(Playback::Playing)).cast);
    }

    #[test]
    fn the_volume_reads_as_a_percentage_until_it_is_muted() {
        assert_eq!(volume_label(14, false), "14%");
        assert_eq!(volume_label(0, false), "0%");
        assert_eq!(volume_label(14, true), "Muted");
    }
}
