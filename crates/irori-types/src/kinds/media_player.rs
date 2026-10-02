//! `media_player`: a TV, a speaker, or a receiver. Its typed value is what it's doing:
//! `off`, `idle`, `playing`, `paused`, `buffering`, or `standby`.

use std::sync::LazyLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Data, Typed};
use crate::{InvariantError, Service, ServiceName};

/// The longest title, artist, album, app name, or content type, in characters.
const TEXT_MAX: usize = 255;
/// The longest media address or image address, in characters.
const URL_MAX: usize = 2048;

/// Every text a media player's primary value can be, for rules to check against.
pub(crate) static PLAYBACK_STATES: LazyLock<Vec<String>> = LazyLock::new(|| {
    PLAYBACK
        .iter()
        .map(|state| state.as_str().to_owned())
        .collect()
});

const PLAYBACK: [Playback; 6] = [
    Playback::Off,
    Playback::Idle,
    Playback::Playing,
    Playback::Paused,
    Playback::Buffering,
    Playback::Standby,
];

/// What it's doing. The primary value automations compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Playback {
    /// Switched off, or the TV isn't showing this source.
    Off,
    /// On, with nothing playing. A backdrop counts as this.
    Idle,
    Playing,
    Paused,
    Buffering,
    /// The TV is in standby. Only when the device says so.
    Standby,
}

impl Playback {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Idle => "idle",
            Self::Playing => "playing",
            Self::Paused => "paused",
            Self::Buffering => "buffering",
            Self::Standby => "standby",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        super::from_ha(text)
    }
}

/// What kind of player it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MediaPlayerClass {
    Tv,
    Speaker,
    Receiver,
}

impl MediaPlayerClass {
    /// The class Home Assistant calls `name`.
    pub fn from_ha(name: &str) -> Option<Self> {
        super::from_ha(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MediaPlayerCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_class: Option<MediaPlayerClass>,
    /// Volume can be set, 0–100.
    #[serde(default)]
    pub volume: bool,
    /// Can be muted.
    #[serde(default)]
    pub mute: bool,
    /// Playback can jump to a position.
    #[serde(default)]
    pub seek: bool,
    /// Can be told to play a piece of media.
    #[serde(default)]
    pub play_media: bool,
    /// Has a queue to skip through.
    #[serde(default)]
    pub queue: bool,
    /// Can be turned on.
    #[serde(default)]
    pub turn_on: bool,
    /// Can be turned off.
    #[serde(default)]
    pub turn_off: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MediaPlayerState {
    /// What it's doing. Its typed value, for rules (`text()`).
    pub state: Playback,
    /// 0–100, when volume can be set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(max = 100))]
    pub volume: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 255))]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 255))]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 255))]
    pub album: Option<String>,
    /// The app that's showing, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 255))]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 255))]
    pub content_type: Option<String>,
    /// How long the current media is, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    /// Where playback is, in seconds, when it can seek.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<f64>,
}

impl MediaPlayerState {
    /// Deserialization runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        if let Some(volume) = self.volume {
            check_volume(volume)?;
        }
        for (what, value) in [
            ("title", &self.title),
            ("artist", &self.artist),
            ("album", &self.album),
            ("app", &self.app),
            ("content_type", &self.content_type),
        ] {
            if let Some(value) = value {
                text_field(what, value, TEXT_MAX)?;
            }
        }
        if let Some(duration) = self.duration {
            seconds("duration", duration)?;
        }
        if let Some(position) = self.position {
            seconds("position", position)?;
        }
        if let (Some(position), Some(duration)) = (self.position, self.duration)
            && position > duration + 1.0
        {
            return Err(InvariantError(format!(
                "position {position} is more than a second past its duration {duration}"
            )));
        }
        Ok(())
    }
}

/// Data for `media_player.volume_set`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VolumeSet {
    #[schemars(range(max = 100))]
    pub volume: u8,
}

impl VolumeSet {
    /// Deserialization runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        check_volume(self.volume)
    }
}

/// Data for `media_player.volume_mute`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VolumeMute {
    pub mute: bool,
}

/// Data for `media_player.media_seek`. `position` is seconds from the start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MediaSeek {
    pub position: f64,
}

impl MediaSeek {
    /// Deserialization runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        seconds("position", self.position)
    }
}

/// Data for `media_player.play_media`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlayMedia {
    #[schemars(length(min = 1, max = 255))]
    pub content_type: String,
    #[schemars(length(min = 1, max = 2048))]
    pub content_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 255))]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 255))]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 255))]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 2048))]
    pub image_url: Option<String>,
}

impl PlayMedia {
    /// Deserialization runs this; call it yourself when building one in code.
    pub fn validate(&self) -> Result<(), InvariantError> {
        text_field("content_type", &self.content_type, TEXT_MAX)?;
        text_field("content_id", &self.content_id, URL_MAX)?;
        for (what, value) in [
            ("title", &self.title),
            ("artist", &self.artist),
            ("album", &self.album),
        ] {
            if let Some(value) = value {
                text_field(what, value, TEXT_MAX)?;
            }
        }
        if let Some(url) = &self.image_url {
            text_field("image_url", url, URL_MAX)?;
        }
        Ok(())
    }
}

fn check_volume(volume: u8) -> Result<(), InvariantError> {
    if volume > 100 {
        Err(InvariantError(format!("volume is 0-100, not {volume}")))
    } else {
        Ok(())
    }
}

fn text_field(what: &str, value: &str, max: usize) -> Result<(), InvariantError> {
    let length = value.chars().count();
    if value.trim().is_empty() || length > max {
        Err(InvariantError(format!(
            "{what} must be 1-{max} characters and not blank"
        )))
    } else {
        Ok(())
    }
}

fn seconds(what: &str, value: f64) -> Result<(), InvariantError> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(InvariantError(format!(
            "{what} is 0 or more seconds, not {value}"
        )))
    }
}

fn needs(flag: bool, why: &str) -> Result<(), String> {
    if flag { Ok(()) } else { Err(why.into()) }
}

/// Whether a reported state is one this player can be in.
pub(crate) fn fits(caps: &MediaPlayerCapabilities, state: &MediaPlayerState) -> Result<(), String> {
    if state.volume.is_some() && !caps.volume {
        return Err("it reports a volume, but can't be set".into());
    }
    if state.muted.is_some() && !caps.mute {
        return Err("it says whether it's muted, but can't be muted".into());
    }
    if state.position.is_some() && !caps.seek {
        return Err("it reports a position, but can't seek".into());
    }
    Ok(())
}

pub(crate) fn primary(state: &MediaPlayerState) -> Typed {
    Typed::Text(state.state.as_str().to_owned())
}

/// Keeps the title and the rest of what it last reported.
pub(crate) fn with_primary(
    previous: Option<&MediaPlayerState>,
    value: &Typed,
) -> Option<MediaPlayerState> {
    let Typed::Text(text) = value else {
        return None;
    };
    let state = Playback::parse(text)?;
    Some(match previous {
        Some(old) => MediaPlayerState {
            state,
            ..old.clone()
        },
        None => MediaPlayerState {
            state,
            volume: None,
            muted: None,
            title: None,
            artist: None,
            album: None,
            app: None,
            content_type: None,
            duration: None,
            position: None,
        },
    })
}

pub(crate) fn data_of(name: ServiceName) -> Data {
    match name {
        ServiceName::MediaPlayerVolumeSet
        | ServiceName::MediaPlayerVolumeMute
        | ServiceName::MediaPlayerSeek
        | ServiceName::MediaPlayerPlayMedia => Data::Required,
        _ => Data::None,
    }
}

pub(crate) fn service(
    name: ServiceName,
    data: serde_json::Map<String, serde_json::Value>,
) -> Result<Service, InvariantError> {
    Ok(match name {
        ServiceName::MediaPlayerTurnOn => Service::MediaPlayerTurnOn,
        ServiceName::MediaPlayerTurnOff => Service::MediaPlayerTurnOff,
        ServiceName::MediaPlayerVolumeSet => {
            Service::MediaPlayerVolumeSet(super::parse(name, data)?)
        }
        ServiceName::MediaPlayerVolumeMute => {
            Service::MediaPlayerVolumeMute(super::parse(name, data)?)
        }
        ServiceName::MediaPlayerPlay => Service::MediaPlayerPlay,
        ServiceName::MediaPlayerPause => Service::MediaPlayerPause,
        ServiceName::MediaPlayerPlayPause => Service::MediaPlayerPlayPause,
        ServiceName::MediaPlayerStop => Service::MediaPlayerStop,
        ServiceName::MediaPlayerSeek => Service::MediaPlayerSeek(super::parse(name, data)?),
        ServiceName::MediaPlayerNextTrack => Service::MediaPlayerNextTrack,
        ServiceName::MediaPlayerPreviousTrack => Service::MediaPlayerPreviousTrack,
        ServiceName::MediaPlayerPlayMedia => {
            Service::MediaPlayerPlayMedia(Box::new(super::parse(name, data)?))
        }
        _ => return Err(super::not_mine(name)),
    })
}

/// Play and pause ask for a playback word. `media_play_pause` depends on what it's doing now,
/// which the protocol knows and the core doesn't.
pub(crate) fn asks_for(service: &Service) -> Option<Typed> {
    let state = match service {
        Service::MediaPlayerTurnOn | Service::MediaPlayerStop => Playback::Idle,
        Service::MediaPlayerTurnOff => Playback::Off,
        Service::MediaPlayerPlay => Playback::Playing,
        Service::MediaPlayerPause => Playback::Paused,
        _ => return None,
    };
    Some(Typed::Text(state.as_str().to_owned()))
}

pub(crate) fn supports_service(
    caps: &MediaPlayerCapabilities,
    service: &Service,
) -> Result<(), String> {
    match service {
        Service::MediaPlayerTurnOn => needs(caps.turn_on, "can't be turned on"),
        Service::MediaPlayerTurnOff => needs(caps.turn_off, "can't be turned off"),
        Service::MediaPlayerVolumeSet(_) => needs(caps.volume, "can't set its volume"),
        Service::MediaPlayerVolumeMute(_) => needs(caps.mute, "can't be muted"),
        Service::MediaPlayerSeek(_) => needs(caps.seek, "can't seek"),
        Service::MediaPlayerNextTrack | Service::MediaPlayerPreviousTrack => {
            needs(caps.queue, "has no queue")
        }
        Service::MediaPlayerPlayMedia(_) => needs(caps.play_media, "can't play media"),
        Service::MediaPlayerPlay
        | Service::MediaPlayerPause
        | Service::MediaPlayerPlayPause
        | Service::MediaPlayerStop => Ok(()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tv() -> MediaPlayerCapabilities {
        MediaPlayerCapabilities {
            device_class: Some(MediaPlayerClass::Tv),
            volume: true,
            mute: true,
            seek: true,
            play_media: true,
            queue: true,
            turn_on: true,
            turn_off: true,
        }
    }

    #[test]
    fn playback_is_the_six_words() {
        assert_eq!(Playback::parse("standby"), Some(Playback::Standby));
        assert_eq!(Playback::Playing.as_str(), "playing");
        assert_eq!(PLAYBACK_STATES.len(), 6);
    }

    #[test]
    fn volume_past_100_is_refused() {
        let err = check_volume(200).expect_err("volume past 100");
        assert_eq!(err.to_string(), "volume is 0-100, not 200");
    }

    #[test]
    fn a_position_past_the_duration_is_refused() {
        let state = MediaPlayerState {
            state: Playback::Playing,
            volume: Some(40),
            muted: Some(false),
            title: Some("The evening news".into()),
            artist: None,
            album: None,
            app: None,
            content_type: None,
            duration: Some(60.0),
            position: Some(62.0),
        };
        assert!(state.validate().is_err());
        let mut speaker = tv();
        speaker.device_class = Some(MediaPlayerClass::Speaker);
        speaker.volume = false;
        speaker.mute = false;
        speaker.seek = false;
        let reported = MediaPlayerState {
            volume: Some(10),
            ..state
        };
        assert_eq!(
            fits(&speaker, &reported),
            Err("it reports a volume, but can't be set".into())
        );
        assert!(supports_service(&speaker, &Service::MediaPlayerTurnOn).is_ok());
        let bare = MediaPlayerCapabilities {
            device_class: None,
            volume: false,
            mute: false,
            seek: false,
            play_media: false,
            queue: false,
            turn_on: false,
            turn_off: false,
        };
        assert_eq!(
            supports_service(&bare, &Service::MediaPlayerTurnOn),
            Err("can't be turned on".into())
        );
        assert_eq!(
            asks_for(&Service::MediaPlayerPlay),
            Some(Typed::Text("playing".into()))
        );
        assert_eq!(asks_for(&Service::MediaPlayerPlayPause), None);
    }
}
