//! ESPHome's `media_player`: a speaker on a board, which plays what it's given a link to.
//!
//! It says what it's doing, how loud it is and whether it's muted, and nothing about what's
//! playing. It can't seek or skip.

use esphome_client::types::{
    ListEntitiesMediaPlayerResponse, MediaPlayerCommandRequest, MediaPlayerStateResponse,
};
use irori_protocol::ProtocolError;
use irori_protocol::types::{
    Capabilities, EntityDescription, EntityKind, MediaPlayerCapabilities, MediaPlayerClass,
    MediaPlayerState, Name, Playback, Service, State, UniqueId,
};

use super::{category, entity_id};

/// ESPHome's `feature_flags` are Home Assistant's `MediaPlayerEntityFeature` bits.
const VOLUME_SET: u32 = 4;
const VOLUME_MUTE: u32 = 8;
const TURN_ON: u32 = 128;
const TURN_OFF: u32 = 256;
const PLAY_MEDIA: u32 = 512;

/// ESPHome's `media_player`.
pub fn describe(
    device: &UniqueId,
    entity: &ListEntitiesMediaPlayerResponse,
) -> Result<EntityDescription, ProtocolError> {
    // Firmware from before the flags says nothing; such a player sets its volume, mutes and
    // plays a link, which is what Home Assistant assumes of it too.
    let flags = match entity.feature_flags {
        0 => VOLUME_SET | VOLUME_MUTE | PLAY_MEDIA,
        flags => flags,
    };
    let has = |bit: u32| flags & bit != 0;
    Ok(EntityDescription {
        unique_id: entity_id(device, EntityKind::MediaPlayer, entity.key)?,
        name: Some(Name::try_from(entity.name.as_str())?),
        device_unique_id: Some(device.clone()),
        suggested_object_id: None,
        capabilities: Capabilities::MediaPlayer(MediaPlayerCapabilities {
            device_class: Some(MediaPlayerClass::Speaker),
            volume: has(VOLUME_SET),
            mute: has(VOLUME_MUTE),
            seek: false,
            play_media: has(PLAY_MEDIA),
            queue: false,
            turn_on: has(TURN_ON),
            turn_off: has(TURN_OFF),
        }),
        entity_category: category(entity.entity_category),
    })
}

/// ESPHome's `MediaPlayerState` (api.proto); 0 is "none", which says nothing. An announcement
/// is something playing, and `on` is on with nothing playing.
pub fn state(state: &MediaPlayerStateResponse, known: &MediaPlayerCapabilities) -> Option<State> {
    let playback = match state.state {
        1 | 6 => Playback::Idle,
        2 | 4 => Playback::Playing,
        3 => Playback::Paused,
        5 => Playback::Off,
        _ => return None,
    };
    // 0 to 1 on the device, 0 to 100 here.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let volume = (known.volume && state.volume.is_finite())
        .then(|| (state.volume.clamp(0.0, 1.0) * 100.0).round() as u8);
    Some(State::MediaPlayer(MediaPlayerState {
        state: playback,
        volume,
        muted: known.mute.then_some(state.muted),
        title: None,
        artist: None,
        album: None,
        app: None,
        content_type: None,
        duration: None,
        position: None,
    }))
}

/// A player command (`MediaPlayerCommand`). `Err` for what an ESPHome player can't do.
pub fn command(key: u32, service: &Service) -> Result<MediaPlayerCommandRequest, String> {
    let told = |command: i32| MediaPlayerCommandRequest {
        key,
        has_command: true,
        command,
        ..Default::default()
    };
    Ok(match service {
        Service::MediaPlayerPlay => told(0),
        Service::MediaPlayerPause => told(1),
        Service::MediaPlayerStop => told(2),
        Service::MediaPlayerVolumeMute(data) => told(if data.mute { 3 } else { 4 }),
        Service::MediaPlayerPlayPause => told(5),
        Service::MediaPlayerTurnOn => told(12),
        Service::MediaPlayerTurnOff => told(13),
        Service::MediaPlayerVolumeSet(data) => MediaPlayerCommandRequest {
            key,
            has_volume: true,
            volume: f32::from(data.volume) / 100.0,
            ..Default::default()
        },
        // It plays a link and nothing else: what kind of thing it is is the device's to find.
        Service::MediaPlayerPlayMedia(media) => MediaPlayerCommandRequest {
            key,
            has_media_url: true,
            media_url: media.content_id.clone(),
            ..Default::default()
        },
        service => {
            return Err(format!("an ESPHome player can't do `{}`", service.name()));
        }
    })
}

#[cfg(test)]
mod tests {
    use irori_protocol::types::{VolumeMute, VolumeSet};

    use super::*;

    #[test]
    fn a_speaker_says_what_it_does_and_how_loud_and_is_told_by_command() {
        let device = UniqueId::try_from("00:11:22:33:44:55").expect("valid");
        let listed = ListEntitiesMediaPlayerResponse {
            key: 8,
            name: "Kitchen speaker".into(),
            ..Default::default()
        };
        let described = describe(&device, &listed).expect("valid");
        let Capabilities::MediaPlayer(caps) = &described.capabilities else {
            panic!("a player");
        };
        // No flags: volume, mute and a link to play, and no way to seek or skip.
        assert!(caps.volume && caps.mute && caps.play_media);
        assert!(!caps.seek && !caps.queue && !caps.turn_off);

        let playing = MediaPlayerStateResponse {
            key: 8,
            state: 4,
            volume: 0.35,
            muted: false,
            ..Default::default()
        };
        let Some(State::MediaPlayer(now)) = state(&playing, caps) else {
            panic!("a player's state");
        };
        assert_eq!(
            (now.state, now.volume, now.muted),
            (Playback::Playing, Some(35), Some(false))
        );
        let nothing = MediaPlayerStateResponse {
            state: 0,
            ..playing
        };
        assert_eq!(state(&nothing, caps), None);

        let quieter =
            command(8, &Service::MediaPlayerVolumeSet(VolumeSet { volume: 20 })).expect("sets");
        assert!(quieter.has_volume && !quieter.has_command);
        assert!((quieter.volume - 0.2).abs() < 1e-6);
        let mute = command(
            8,
            &Service::MediaPlayerVolumeMute(VolumeMute { mute: true }),
        )
        .expect("mutes");
        assert_eq!((mute.has_command, mute.command), (true, 3));
        assert!(command(8, &Service::MediaPlayerNextTrack).is_err());
    }
}
