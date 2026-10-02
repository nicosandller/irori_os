//! Receiver status and media status, turned into one media player, and a service call turned
//! into the Cast messages that ask for it.
//!
//! HDMI standby is whatever the receiver status already says (`isStandBy`, `isActiveInput`).
//! Nothing here speaks CEC on the cable. A missing flag is left unknown: it is not standby.

use irori_types::{
    Capabilities, DeviceDescription, EntityDescription, MediaPlayerCapabilities, MediaPlayerClass,
    MediaPlayerState, Name, PlayMedia, Playback, Service, UniqueId,
};

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
    pub player_state: Option<String>,
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

/// Fold one receiver-status object into `snap`. Absent HDMI flags become unknown, not standby.
pub fn apply_receiver(snap: &mut Snap, message: &serde_json::Value) {
    let Some(status) = message.get("status") else {
        return;
    };
    if let Some(volume) = status.get("volume") {
        if let Some(level) = volume.get("level").and_then(serde_json::Value::as_f64) {
            snap.volume = volume_percent(level);
        }
        if let Some(muted) = volume.get("muted").and_then(serde_json::Value::as_bool) {
            snap.muted = Some(muted);
        }
    }
    snap.stand_by = flag(status, "isStandBy");
    snap.active_input = flag(status, "isActiveInput");
    if status.get("applications").is_none() {
        return;
    }
    let app = status
        .get("applications")
        .and_then(serde_json::Value::as_array)
        .and_then(|apps| apps.first());
    let app_id = app
        .and_then(|app| app.get("appId"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    if app_id != snap.app_id {
        snap.clear_media();
    }
    snap.app_id = app_id;
    snap.app_name = app
        .and_then(|app| app.get("displayName"))
        .and_then(serde_json::Value::as_str)
        .and_then(|text| clip(text, TEXT_MAX));
    snap.session_id = app
        .and_then(|app| app.get("sessionId"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    snap.transport_id = app
        .and_then(|app| app.get("transportId"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    if snap.app_id.is_none() {
        snap.clear_media();
    }
}

/// Fold one media-status object into `snap`.
pub fn apply_media(snap: &mut Snap, message: &serde_json::Value) {
    let entry = message
        .get("status")
        .and_then(serde_json::Value::as_array)
        .and_then(|entries| entries.first());
    let Some(entry) = entry else {
        snap.clear_media();
        return;
    };
    snap.media_session_id = entry
        .get("mediaSessionId")
        .and_then(serde_json::Value::as_i64);
    snap.player_state = entry
        .get("playerState")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    snap.position = entry
        .get("currentTime")
        .and_then(serde_json::Value::as_f64)
        .and_then(seconds);
    if let Some(media) = entry.get("media") {
        snap.content_type = media
            .get("contentType")
            .and_then(serde_json::Value::as_str)
            .and_then(|text| clip(text, TEXT_MAX));
        snap.duration = media
            .get("duration")
            .and_then(serde_json::Value::as_f64)
            .and_then(seconds);
        if let Some(metadata) = media.get("metadata") {
            snap.title = text_field(metadata, "title");
            snap.artist =
                text_field(metadata, "artist").or_else(|| text_field(metadata, "subtitle"));
            snap.album = text_field(metadata, "albumName");
        }
    }
    if let (Some(position), Some(duration)) = (snap.position, snap.duration)
        && position > duration + 1.0
    {
        snap.position = None;
    }
}

fn text_field(metadata: &serde_json::Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
        .and_then(|text| clip(text, TEXT_MAX))
}

/// `Some` only when the field is present and a boolean. Null and a missing field are unknown.
fn flag(status: &serde_json::Value, key: &str) -> Option<bool> {
    status.get(key).and_then(serde_json::Value::as_bool)
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

fn from_app(snap: &Snap) -> Playback {
    match snap.player_state.as_deref() {
        Some("PLAYING") => Playback::Playing,
        Some("PAUSED") => Playback::Paused,
        Some("BUFFERING" | "LOADING") => Playback::Buffering,
        _ => Playback::Idle,
    }
}

fn app_name(snap: &Snap, playback: Playback) -> Option<String> {
    if matches!(playback, Playback::Standby | Playback::Off | Playback::Idle) {
        return None;
    }
    if snap.app_id.as_deref() == Some(BACKDROP) {
        return None;
    }
    snap.app_name.clone()
}

/// What to send for one service call. The session replies once the last write is accepted.
#[derive(Debug)]
pub enum Outcome {
    /// Already there. Reply straight away.
    Ready,
    Fail(String),
    Steps(Vec<Step>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Quit,
    Launch,
    Load(LoadBody),
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
                Outcome::Steps(vec![Step::Quit])
            } else {
                Outcome::Ready
            }
        }
        Service::MediaPlayerVolumeSet(volume) => Outcome::Steps(vec![Step::Volume {
            level: Some(f64::from(volume.volume) / 100.0),
            muted: None,
        }]),
        Service::MediaPlayerVolumeMute(mute) => Outcome::Steps(vec![Step::Volume {
            level: None,
            muted: Some(mute.mute),
        }]),
        Service::MediaPlayerPlay => media_step(snap, "PLAY", None),
        Service::MediaPlayerPause => media_step(snap, "PAUSE", None),
        Service::MediaPlayerPlayPause => {
            let action = if snap.player_state.as_deref() == Some("PLAYING") {
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
    } else if snap.session_id.is_some() {
        Outcome::Steps(vec![Step::Quit, Step::Launch])
    } else {
        Outcome::Steps(vec![Step::Launch])
    }
}

fn play(snap: &Snap, media: &PlayMedia) -> Outcome {
    if !is_http(&media.content_id) {
        return Outcome::Fail("only an http or https address can be played".into());
    }
    let mut steps = Vec::new();
    if snap.app_id.as_deref() != Some(DEFAULT_MEDIA_RECEIVER) {
        if snap.session_id.is_some() {
            steps.push(Step::Quit);
        }
        steps.push(Step::Launch);
    }
    steps.push(Step::Load(LoadBody::from_media(media)));
    Outcome::Steps(steps)
}

fn media_step(snap: &Snap, action: &'static str, position: Option<f64>) -> Outcome {
    if snap.media_session_id.is_none() || snap.transport_id.is_none() {
        Outcome::Fail("nothing is playing".into())
    } else {
        Outcome::Steps(vec![Step::Media { action, position }])
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
        playing.player_state = Some("PLAYING".to_owned());
        playing.title = Some("The evening news".to_owned());
        playing.position = Some(12.0);
        let state = playback(&playing, false, false);
        assert_eq!(state.state, Playback::Playing);
        assert_eq!(state.title.as_deref(), Some("The evening news"));
        assert_eq!(state.position, Some(12.0));

        playing.player_state = Some("BUFFERING".to_owned());
        assert_eq!(word(&playing, false, false), Playback::Buffering);
        playing.player_state = Some("LOADING".to_owned());
        assert_eq!(word(&playing, false, false), Playback::Buffering);
        playing.player_state = Some("PAUSED".to_owned());
        assert_eq!(word(&playing, false, false), Playback::Paused);

        playing.player_state = None;
        playing.app_id = Some(BACKDROP.to_owned());
        let idle = playback(&playing, false, false);
        assert_eq!(idle.state, Playback::Idle);
        assert!(idle.title.is_none());
    }

    #[test]
    fn turn_on_launches_and_does_not_load() {
        match turn_on(&snap()) {
            Outcome::Steps(steps) => assert_eq!(steps, vec![Step::Launch]),
            other => panic!("expected a launch, got {other:?}"),
        }
        let mut busy = snap();
        busy.app_id = Some("YouTube".to_owned());
        busy.session_id = Some("s".to_owned());
        match turn_on(&busy) {
            Outcome::Steps(steps) => assert_eq!(steps, vec![Step::Quit, Step::Launch]),
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
