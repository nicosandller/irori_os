//! Receiver status and media status, turned into one media player, and a service call turned
//! into the Cast messages that ask for it.
//!
//! HDMI standby is whatever the receiver status already says (`isStandBy`, `isActiveInput`).
//! Nothing here speaks CEC on the cable. A missing flag is left unknown: it is not standby.

use irori_types::{
    Capabilities, DeviceDescription, EntityDescription, MediaPlayerCapabilities, MediaPlayerClass,
    MediaPlayerState, Name, PlayMedia, Playback, Service, UniqueId,
};
use serde::Deserialize;

use crate::discover::Found;

/// The Default Media Receiver. Launching it with no media is what lets a Chromecast stick ask
/// the TV to switch input.
pub const DEFAULT_MEDIA_RECEIVER: &str = "CC1AD845";
/// The backdrop slideshow. Nothing is playing.
pub const BACKDROP: &str = "E8C28D3C";

const TEXT_MAX: usize = 255;

/// What one receiver is doing, as last reported.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snap {
    pub app_id: Option<String>,
    pub app_name: Option<String>,
    pub session_id: Option<String>,
    pub transport_id: Option<String>,
    pub media_session_id: Option<i64>,
    pub player_state: Option<PlayerState>,
    pub volume: Option<u8>,
    pub muted: Option<bool>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub content_type: Option<String>,
    pub duration: Option<f64>,
    pub position: Option<f64>,
    pub stand_by: Option<bool>,
    pub active_input: Option<bool>,
}

/// What the media namespace last said. An idle word, or one this protocol doesn't use, is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlayerState {
    Playing,
    Paused,
    Buffering,
}

impl PlayerState {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "PLAYING" => Some(Self::Playing),
            "PAUSED" => Some(Self::Paused),
            "BUFFERING" | "LOADING" => Some(Self::Buffering),
            _ => None,
        }
    }
}

impl Snap {
    fn clear_media(&mut self) {
        self.media_session_id = None;
        self.player_state = None;
        self.title = None;
        self.artist = None;
        self.album = None;
        self.content_type = None;
        self.duration = None;
        self.position = None;
    }
}

/// A TV unless the model names a speaker, a group, or a smart speaker.
pub fn device_class(model: &str) -> MediaPlayerClass {
    let model = model.to_ascii_lowercase();
    if ["group", "audio", "home", "mini", "speaker"]
        .iter()
        .any(|word| model.contains(word))
    {
        MediaPlayerClass::Speaker
    } else {
        MediaPlayerClass::Tv
    }
}

pub fn capabilities(class: MediaPlayerClass) -> MediaPlayerCapabilities {
    MediaPlayerCapabilities {
        device_class: Some(class),
        volume: true,
        mute: true,
        seek: true,
        play_media: true,
        queue: true,
        turn_on: class == MediaPlayerClass::Tv,
        turn_off: true,
    }
}

pub fn descriptions(found: &Found) -> Result<(DeviceDescription, EntityDescription), String> {
    let class = device_class(&found.model);
    let device_id = UniqueId::try_from(found.uuid.as_str()).map_err(|error| error.to_string())?;
    let entity_id =
        UniqueId::try_from(format!("media:{}", found.uuid)).map_err(|error| error.to_string())?;
    let device = DeviceDescription {
        unique_id: device_id.clone(),
        name: device_name(&found.name),
        manufacturer: Some("Google".to_owned()),
        model: blank_to_none(&found.model),
        sw_version: blank_to_none(&found.version),
        hw_version: None,
        suggested_area: None,
        via_device_unique_id: None,
    };
    let entity = EntityDescription {
        unique_id: entity_id,
        name: None,
        device_unique_id: Some(device_id),
        suggested_object_id: None,
        capabilities: Capabilities::MediaPlayer(capabilities(class)),
        entity_category: None,
    };
    Ok((device, entity))
}

fn device_name(raw: &str) -> Name {
    let trimmed = raw.trim();
    let source = if trimmed.is_empty() { "Cast" } else { trimmed };
    let mut short: String = source.chars().take(100).collect();
    short = short.trim_end().to_owned();
    if short.is_empty() {
        short = "Cast".to_owned();
    }
    Name::try_from(short).expect("the name was trimmed to fit")
}

fn blank_to_none(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.chars().take(TEXT_MAX).collect())
    }
}

/// Fold one receiver-status object into `snap`. Absent HDMI flags become unknown. A missing
/// `applications` field leaves the running app alone; an empty list means there is no app.
pub fn apply_receiver(snap: &mut Snap, message: &serde_json::Value) {
    let Some(status) = message.get("status") else {
        return;
    };
    let Ok(status) = serde_json::from_value::<ReceiverStatus>(status.clone()) else {
        return;
    };
    if let Some(level) = status.volume.as_ref().and_then(|volume| volume.level) {
        snap.volume = volume_percent(level);
    }
    if let Some(muted) = status.volume.as_ref().and_then(|volume| volume.muted) {
        snap.muted = Some(muted);
    }
    snap.stand_by = status.is_stand_by;
    snap.active_input = status.is_active_input;
    let Apps::Present(apps) = status.applications else {
        return;
    };
    let app = apps.first();
    let app_id = app.and_then(|app| app.app_id.clone());
    if app_id != snap.app_id {
        snap.clear_media();
    }
    snap.app_id = app_id;
    snap.app_name = app
        .and_then(|app| app.display_name.as_deref())
        .and_then(|text| clip(text, TEXT_MAX));
    snap.session_id = app.and_then(|app| app.session_id.clone());
    snap.transport_id = app.and_then(|app| app.transport_id.clone());
    if snap.app_id.is_none() {
        snap.clear_media();
    }
}

/// Fold one media-status object into `snap`.
pub fn apply_media(snap: &mut Snap, message: &serde_json::Value) {
    let Ok(parsed) = serde_json::from_value::<MediaMessage>(message.clone()) else {
        snap.clear_media();
        return;
    };
    let Some(entry) = parsed.status.as_ref().and_then(|entries| entries.first()) else {
        snap.clear_media();
        return;
    };
    snap.media_session_id = entry.media_session_id;
    snap.player_state = entry.player_state.as_deref().and_then(PlayerState::parse);
    snap.position = entry.current_time.and_then(seconds);
    if let Some(media) = &entry.media {
        snap.content_type = media
            .content_type
            .as_deref()
            .and_then(|text| clip(text, TEXT_MAX));
        snap.duration = media.duration.and_then(seconds);
        if let Some(metadata) = &media.metadata {
            snap.title = metadata
                .title
                .as_deref()
                .and_then(|text| clip(text, TEXT_MAX));
            snap.artist = metadata
                .artist
                .as_deref()
                .and_then(|text| clip(text, TEXT_MAX))
                .or_else(|| {
                    metadata
                        .subtitle
                        .as_deref()
                        .and_then(|text| clip(text, TEXT_MAX))
                });
            snap.album = metadata
                .album_name
                .as_deref()
                .and_then(|text| clip(text, TEXT_MAX));
        }
    }
    if let (Some(position), Some(duration)) = (snap.position, snap.duration)
        && position > duration + 1.0
    {
        snap.position = None;
    }
}

/// `applications` missing leaves the app alone. Null and `[]` both mean there is no app.
#[derive(Deserialize)]
struct ReceiverStatus {
    #[serde(default)]
    volume: Option<VolumeLevel>,
    #[serde(default, rename = "isStandBy")]
    is_stand_by: Option<bool>,
    #[serde(default, rename = "isActiveInput")]
    is_active_input: Option<bool>,
    #[serde(default, deserialize_with = "applications")]
    applications: Apps,
}

#[derive(Deserialize)]
struct VolumeLevel {
    #[serde(default)]
    level: Option<f64>,
    #[serde(default)]
    muted: Option<bool>,
}

#[derive(Deserialize)]
struct CastApp {
    #[serde(default, rename = "appId")]
    app_id: Option<String>,
    #[serde(default, rename = "displayName")]
    display_name: Option<String>,
    #[serde(default, rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default, rename = "transportId")]
    transport_id: Option<String>,
}

#[derive(Default)]
enum Apps {
    #[default]
    Absent,
    Present(Vec<CastApp>),
}

fn applications<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Apps, D::Error> {
    match Option::<Vec<CastApp>>::deserialize(deserializer)? {
        Some(apps) => Ok(Apps::Present(apps)),
        None => Ok(Apps::Present(Vec::new())),
    }
}

#[derive(Deserialize)]
struct MediaMessage {
    #[serde(default)]
    status: Option<Vec<MediaEntry>>,
}

#[derive(Deserialize)]
struct MediaEntry {
    #[serde(default, rename = "mediaSessionId")]
    media_session_id: Option<i64>,
    #[serde(default, rename = "playerState")]
    player_state: Option<String>,
    #[serde(default, rename = "currentTime")]
    current_time: Option<f64>,
    #[serde(default)]
    media: Option<MediaInfo>,
}

#[derive(Deserialize)]
struct MediaInfo {
    #[serde(default, rename = "contentType")]
    content_type: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    metadata: Option<MediaMetadata>,
}

#[derive(Deserialize)]
struct MediaMetadata {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    artist: Option<String>,
    #[serde(default)]
    subtitle: Option<String>,
    #[serde(default, rename = "albumName")]
    album_name: Option<String>,
}

fn volume_percent(level: f64) -> Option<u8> {
    if !level.is_finite() {
        return None;
    }
    let percent = (level * 100.0).round();
    if (0.0..=100.0).contains(&percent) {
        Some(percent as u8)
    } else {
        None
    }
}

fn seconds(value: f64) -> Option<f64> {
    if value.is_finite() && value >= 0.0 {
        Some(value)
    } else {
        None
    }
}

fn clip(value: &str, max: usize) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut text: String = trimmed.chars().take(max).collect();
    text = text.trim_end().to_owned();
    if text.is_empty() { None } else { Some(text) }
}

/// The player Irori should show. HDMI flags apply only to a video device that isn't ignored,
/// and only when the receiver actually sent them.
pub fn playback(snap: &Snap, speaker: bool, ignore_cec: bool) -> MediaPlayerState {
    let playback = if !speaker && !ignore_cec {
        if snap.stand_by == Some(true) {
            Playback::Standby
        } else if snap.active_input == Some(false) {
            Playback::Off
        } else {
            from_app(snap)
        }
    } else {
        from_app(snap)
    };
    let media = matches!(
        playback,
        Playback::Playing | Playback::Paused | Playback::Buffering
    );
    let mut state = MediaPlayerState {
        state: playback,
        volume: snap.volume,
        muted: snap.muted,
        title: media.then(|| snap.title.clone()).flatten(),
        artist: media.then(|| snap.artist.clone()).flatten(),
        album: media.then(|| snap.album.clone()).flatten(),
        app: app_name(snap, playback),
        content_type: media.then(|| snap.content_type.clone()).flatten(),
        duration: media.then_some(snap.duration).flatten(),
        position: media.then_some(snap.position).flatten(),
    };
    if state.validate().is_err() {
        state.position = None;
        state.duration = None;
    }
    state
}

/// Backdrop and a receiver with no app are idle, whatever the last player word was.
fn from_app(snap: &Snap) -> Playback {
    if snap.app_id.is_none() || snap.app_id.as_deref() == Some(BACKDROP) {
        return Playback::Idle;
    }
    match snap.player_state {
        Some(PlayerState::Playing) => Playback::Playing,
        Some(PlayerState::Paused) => Playback::Paused,
        Some(PlayerState::Buffering) => Playback::Buffering,
        None => Playback::Idle,
    }
}

fn app_name(snap: &Snap, playback: Playback) -> Option<String> {
    if matches!(playback, Playback::Standby | Playback::Off | Playback::Idle) {
        None
    } else {
        snap.app_name.clone()
    }
}

/// What to send for one service call. The session replies once the last write is accepted.
#[derive(Debug)]
pub enum Outcome {
    /// Already there. Reply straight away.
    Ready,
    Fail(String),
    Do(Command),
}

/// The three sequences a receiver is asked for. A single message is answered when the write
/// succeeds. Turn on answers when the launch is written. Play answers when the load is written,
/// after a launch has a transport.
#[derive(Debug)]
pub enum Command {
    Once(Once),
    TurnOn {
        quit: bool,
    },
    Play {
        quit: bool,
        launch: bool,
        body: LoadBody,
    },
}

/// One message, answered as soon as it is written.
#[derive(Debug, Clone, Copy)]
pub enum Once {
    Quit,
    Volume {
        level: Option<f64>,
        muted: Option<bool>,
    },
    Media {
        action: &'static str,
        position: Option<f64>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadBody {
    pub content_id: String,
    pub content_type: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub image_url: Option<String>,
}

impl LoadBody {
    fn from_media(media: &PlayMedia) -> Self {
        Self {
            content_id: media.content_id.clone(),
            content_type: media.content_type.clone(),
            title: media.title.clone(),
            artist: media.artist.clone(),
            album: media.album.clone(),
            image_url: media.image_url.clone(),
        }
    }

    pub fn media_json(&self) -> serde_json::Value {
        let mut media = serde_json::json!({
            "contentId": self.content_id,
            "contentType": self.content_type,
            "streamType": "BUFFERED",
        });
        let mut metadata = serde_json::Map::new();
        if let Some(title) = &self.title {
            metadata.insert("title".into(), serde_json::Value::String(title.clone()));
        }
        if let Some(artist) = &self.artist {
            metadata.insert("artist".into(), serde_json::Value::String(artist.clone()));
        }
        if let Some(album) = &self.album {
            metadata.insert("albumName".into(), serde_json::Value::String(album.clone()));
        }
        if let Some(url) = &self.image_url {
            metadata.insert("images".into(), serde_json::json!([{ "url": url }]));
        }
        if !metadata.is_empty() {
            metadata.insert("metadataType".into(), serde_json::json!(0));
            media["metadata"] = serde_json::Value::Object(metadata);
        }
        media
    }
}

pub fn plan(service: &Service, snap: &Snap) -> Outcome {
    match service {
        Service::MediaPlayerTurnOn => turn_on(snap),
        Service::MediaPlayerTurnOff => {
            if snap.session_id.is_some() {
                Outcome::Do(Command::Once(Once::Quit))
            } else {
                Outcome::Ready
            }
        }
        Service::MediaPlayerVolumeSet(volume) => Outcome::Do(Command::Once(Once::Volume {
            level: Some(f64::from(volume.volume) / 100.0),
            muted: None,
        })),
        Service::MediaPlayerVolumeMute(mute) => Outcome::Do(Command::Once(Once::Volume {
            level: None,
            muted: Some(mute.mute),
        })),
        Service::MediaPlayerPlay => media_step(snap, "PLAY", None),
        Service::MediaPlayerPause => media_step(snap, "PAUSE", None),
        Service::MediaPlayerPlayPause => {
            let action = if snap.player_state == Some(PlayerState::Playing) {
                "PAUSE"
            } else {
                "PLAY"
            };
            media_step(snap, action, None)
        }
        Service::MediaPlayerStop => media_step(snap, "STOP", None),
        Service::MediaPlayerSeek(seek) => media_step(snap, "SEEK", Some(seek.position)),
        Service::MediaPlayerNextTrack => media_step(snap, "QUEUE_NEXT", None),
        Service::MediaPlayerPreviousTrack => media_step(snap, "QUEUE_PREV", None),
        Service::MediaPlayerPlayMedia(media) => play(snap, media),
        _ => Outcome::Fail("this Cast device can't do that".into()),
    }
}

fn turn_on(snap: &Snap) -> Outcome {
    if snap.app_id.as_deref() == Some(DEFAULT_MEDIA_RECEIVER) {
        Outcome::Ready
    } else {
        Outcome::Do(Command::TurnOn {
            quit: snap.session_id.is_some(),
        })
    }
}

fn play(snap: &Snap, media: &PlayMedia) -> Outcome {
    if !is_http(&media.content_id) {
        return Outcome::Fail("only an http or https address can be played".into());
    }
    let launch = snap.app_id.as_deref() != Some(DEFAULT_MEDIA_RECEIVER);
    Outcome::Do(Command::Play {
        quit: launch && snap.session_id.is_some(),
        launch,
        body: LoadBody::from_media(media),
    })
}

fn media_step(snap: &Snap, action: &'static str, position: Option<f64>) -> Outcome {
    if snap.media_session_id.is_none() || snap.transport_id.is_none() {
        Outcome::Fail("nothing is playing".into())
    } else {
        Outcome::Do(Command::Once(Once::Media { action, position }))
    }
}

fn is_http(url: &str) -> bool {
    let url = url.trim().to_ascii_lowercase();
    url.starts_with("https://") || url.starts_with("http://")
}

/// The typed value, for tests that only care about the playback word.
#[cfg(test)]
fn word(snap: &Snap, speaker: bool, ignore_cec: bool) -> Playback {
    playback(snap, speaker, ignore_cec).state
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> Snap {
        Snap::default()
    }

    #[test]
    fn model_names_pick_the_class() {
        assert_eq!(device_class("Chromecast"), MediaPlayerClass::Tv);
        assert_eq!(device_class(""), MediaPlayerClass::Tv);
        assert_eq!(device_class("Google Home Mini"), MediaPlayerClass::Speaker);
        assert_eq!(device_class("Chromecast Audio"), MediaPlayerClass::Speaker);
        assert_eq!(device_class("Family group"), MediaPlayerClass::Speaker);
        assert_eq!(device_class("Nest Speaker"), MediaPlayerClass::Speaker);
        assert!(capabilities(MediaPlayerClass::Tv).turn_on);
        assert!(!capabilities(MediaPlayerClass::Speaker).turn_on);
    }

    #[test]
    fn hdmi_flags_only_change_a_video_device_when_they_were_sent() {
        let mut video = snap();
        assert_eq!(word(&video, false, false), Playback::Idle);

        video.stand_by = Some(true);
        video.active_input = Some(false);
        assert_eq!(word(&video, false, false), Playback::Standby);

        video.stand_by = Some(false);
        assert_eq!(word(&video, false, false), Playback::Off);

        video.active_input = Some(true);
        assert_eq!(word(&video, false, false), Playback::Idle);

        video.stand_by = Some(true);
        assert_eq!(word(&video, true, false), Playback::Idle, "a speaker");
        assert_eq!(word(&video, false, true), Playback::Idle, "ignored");
    }

    #[test]
    fn a_missing_hdmi_flag_is_not_standby() {
        let message = serde_json::json!({
            "type": "RECEIVER_STATUS",
            "status": { "applications": [], "volume": { "level": 0.25, "muted": false } }
        });
        let mut parsed = snap();
        apply_receiver(&mut parsed, &message);
        assert_eq!(parsed.stand_by, None);
        assert_eq!(parsed.active_input, None);
        assert_eq!(parsed.volume, Some(25));
        assert_eq!(word(&parsed, false, false), Playback::Idle);

        let mut kept = snap();
        kept.app_id = Some(DEFAULT_MEDIA_RECEIVER.to_owned());
        kept.session_id = Some("session".to_owned());
        apply_receiver(
            &mut kept,
            &serde_json::json!({
                "status": { "volume": { "level": 0.5 } }
            }),
        );
        assert_eq!(kept.app_id.as_deref(), Some(DEFAULT_MEDIA_RECEIVER));
        assert_eq!(kept.session_id.as_deref(), Some("session"));
        assert_eq!(kept.volume, Some(50));
    }

    #[test]
    fn standby_comes_from_the_field_the_receiver_sends() {
        let message = serde_json::json!({
            "type": "RECEIVER_STATUS",
            "status": { "applications": [], "isStandBy": true, "isActiveInput": false }
        });
        let mut parsed = snap();
        apply_receiver(&mut parsed, &message);
        assert_eq!(word(&parsed, false, false), Playback::Standby);
    }

    #[test]
    fn backdrop_and_playback_words() {
        let mut playing = snap();
        playing.app_id = Some("CC1AD845".to_owned());
        playing.app_name = Some("Default Media Receiver".to_owned());
        playing.player_state = Some(PlayerState::Playing);
        playing.title = Some("The evening news".to_owned());
        playing.position = Some(12.0);
        let state = playback(&playing, false, false);
        assert_eq!(state.state, Playback::Playing);
        assert_eq!(state.title.as_deref(), Some("The evening news"));
        assert_eq!(state.position, Some(12.0));

        playing.player_state = Some(PlayerState::Buffering);
        assert_eq!(word(&playing, false, false), Playback::Buffering);
        playing.player_state = Some(PlayerState::Paused);
        assert_eq!(word(&playing, false, false), Playback::Paused);

        let mut loading = snap();
        apply_media(
            &mut loading,
            &serde_json::json!({
                "status": [{
                    "mediaSessionId": 1,
                    "playerState": "LOADING",
                    "media": { "contentType": "video" }
                }]
            }),
        );
        loading.app_id = Some("CC1AD845".to_owned());
        assert_eq!(loading.player_state, Some(PlayerState::Buffering));
        assert_eq!(word(&loading, false, false), Playback::Buffering);

        playing.player_state = Some(PlayerState::Playing);
        playing.app_id = Some(BACKDROP.to_owned());
        let idle = playback(&playing, false, false);
        assert_eq!(idle.state, Playback::Idle);
        assert!(idle.title.is_none());
    }

    #[test]
    fn turn_on_launches_and_does_not_load() {
        match turn_on(&snap()) {
            Outcome::Do(Command::TurnOn { quit: false }) => {}
            other => panic!("expected a launch, got {other:?}"),
        }
        let mut busy = snap();
        busy.app_id = Some("YouTube".to_owned());
        busy.session_id = Some("s".to_owned());
        match turn_on(&busy) {
            Outcome::Do(Command::TurnOn { quit: true }) => {}
            other => panic!("expected quit then launch, got {other:?}"),
        }
    }

    #[test]
    fn only_an_http_address_can_be_played() {
        let media = PlayMedia {
            content_type: "video".to_owned(),
            content_id: "file:///tmp/clip.mp4".to_owned(),
            title: None,
            artist: None,
            album: None,
            image_url: None,
        };
        match plan(&Service::MediaPlayerPlayMedia(Box::new(media)), &snap()) {
            Outcome::Fail(message) => {
                assert_eq!(message, "only an http or https address can be played");
                assert!(!message.contains("file"));
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn nothing_playing_cannot_be_paused() {
        match plan(&Service::MediaPlayerPause, &snap()) {
            Outcome::Fail(message) => assert_eq!(message, "nothing is playing"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}
