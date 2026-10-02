//! The living-room TV. Playback changes once a minute of demo time, so a trigger waiting
//! for it to play for 20 seconds gets to fire. A command holds until that minute changes.

use tokio::time::Instant;

use irori_protocol::types::{
    Capabilities, MediaPlayerCapabilities, MediaPlayerClass, MediaPlayerState, Playback, Service,
    State,
};

use super::{Device, Entity, Gadget, Room};

const TV: &str = "tv";
const PLAYER: &str = "tv-player";
/// How long the TV stays in each scripted state.
const STEP: u64 = 60;

pub(crate) struct DemoTv {
    /// Demo seconds on the sensor clock, as of the last tick. Commands use this same clock.
    secs: u64,
    /// A command and the scripted step it belongs to.
    hold: Option<(u64, MediaPlayerState)>,
}

impl DemoTv {
    pub(crate) fn new() -> Self {
        Self {
            secs: 0,
            hold: None,
        }
    }

    fn shown(&self) -> MediaPlayerState {
        match &self.hold {
            Some((step, player)) if *step == self.secs / STEP => player.clone(),
            _ => scripted(self.secs),
        }
    }
}

/// The playback word at `secs`, for the day scene.
pub(crate) fn word(secs: u64) -> &'static str {
    const SHOW: [&str; 5] = ["playing", "paused", "playing", "idle", "off"];
    SHOW[usize::try_from(secs / STEP).unwrap_or(0) % SHOW.len()]
}

fn scripted(secs: u64) -> MediaPlayerState {
    let state = Playback::parse(word(secs)).unwrap_or(Playback::Off);
    MediaPlayerState {
        state,
        volume: Some(40),
        muted: Some(false),
        title: (state == Playback::Playing).then(|| "The evening news".into()),
        artist: None,
        album: None,
        app: None,
        content_type: None,
        duration: None,
        position: None,
    }
}

/// Applies one command. The caller keeps the result until the scripted step changes.
fn apply(player: &mut MediaPlayerState, service: &Service) -> Result<(), String> {
    match service {
        Service::MediaPlayerTurnOn => {
            player.state = Playback::Idle;
            player.title = None;
        }
        Service::MediaPlayerTurnOff => {
            player.state = Playback::Off;
            player.title = None;
        }
        Service::MediaPlayerVolumeSet(data) => player.volume = Some(data.volume),
        Service::MediaPlayerVolumeMute(data) => player.muted = Some(data.mute),
        Service::MediaPlayerPlay => {
            player.state = Playback::Playing;
            if player.title.is_none() {
                player.title = Some("The evening news".into());
            }
        }
        Service::MediaPlayerPause => player.state = Playback::Paused,
        Service::MediaPlayerPlayPause => {
            if player.state == Playback::Playing {
                player.state = Playback::Paused;
            } else {
                player.state = Playback::Playing;
                if player.title.is_none() {
                    player.title = Some("The evening news".into());
                }
            }
        }
        Service::MediaPlayerStop => {
            player.state = Playback::Idle;
            player.title = None;
        }
        Service::MediaPlayerSeek(data) => player.position = Some(data.position),
        Service::MediaPlayerNextTrack => {
            player.state = Playback::Playing;
            player.title = Some("The next show".into());
        }
        Service::MediaPlayerPreviousTrack => {
            player.state = Playback::Playing;
            player.title = Some("The previous show".into());
        }
        Service::MediaPlayerPlayMedia(data) => {
            player.state = Playback::Playing;
            player.content_type = Some(data.content_type.clone());
            player.title = Some(
                data.title
                    .clone()
                    .unwrap_or_else(|| data.content_id.clone()),
            );
        }
        other => return Err(format!("the demo TV has no {}", other.name())),
    }
    Ok(())
}

impl Gadget for DemoTv {
    fn device(&self) -> Device {
        Device {
            unique_id: TV,
            name: "Demo TV",
            model: "Virtual TV",
            room: "Living room",
        }
    }

    fn entities(&self) -> Vec<Entity> {
        vec![Entity {
            unique_id: PLAYER,
            name: None,
            suggested_object_id: Some("demo_tv"),
            capabilities: Capabilities::MediaPlayer(MediaPlayerCapabilities {
                device_class: Some(MediaPlayerClass::Tv),
                volume: true,
                mute: true,
                seek: true,
                play_media: true,
                queue: true,
                turn_on: true,
                turn_off: true,
            }),
            category: None,
        }]
    }

    fn states(&self) -> Vec<(&'static str, State)> {
        vec![(PLAYER, State::MediaPlayer(self.shown()))]
    }

    fn call(
        &mut self,
        unique_id: &str,
        service: &Service,
        _: Instant,
    ) -> Result<&'static str, String> {
        if unique_id != PLAYER {
            return Err(format!("the demo TV has no {}", service.name()));
        }
        let mut player = self.shown();
        apply(&mut player, service)?;
        self.hold = Some((self.secs / STEP, player));
        Ok(PLAYER)
    }

    /// Reports the player on every sensor tick, and drops a command once its minute is over.
    fn tick(&mut self, _: Instant, _: Room, secs: u64) -> Vec<&'static str> {
        self.secs = secs;
        if self
            .hold
            .as_ref()
            .is_some_and(|(step, _)| *step != secs / STEP)
        {
            self.hold = None;
        }
        vec![PLAYER]
    }
}

#[cfg(test)]
mod tests {
    use irori_protocol::types::PlayMedia;

    use super::*;

    #[test]
    fn a_command_holds_until_the_next_minute() {
        let mut held = scripted(60);
        assert_eq!(held.state, Playback::Paused);
        apply(
            &mut held,
            &Service::MediaPlayerPlayMedia(Box::new(PlayMedia {
                content_type: "video".into(),
                content_id: "https://example.com/clip.mp4".into(),
                title: Some("Big Buck Bunny".into()),
                artist: None,
                album: None,
                image_url: None,
            })),
        )
        .expect("plays");
        let during = DemoTv {
            secs: 90,
            hold: Some((60 / STEP, held)),
        };
        assert_eq!(during.shown().title.as_deref(), Some("Big Buck Bunny"));
        let next = DemoTv {
            secs: 120,
            hold: during.hold,
        };
        assert_eq!(next.shown().state, Playback::Playing);
        assert_eq!(next.shown().title.as_deref(), Some("The evening news"));
    }
}
