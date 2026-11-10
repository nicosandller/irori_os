//! Getting to know a camera over ONVIF, once it has a login: what it is, which services it
//! has, its video profiles, and where its pictures and streams are.
//!
//! The first request that carries the login is `GetDeviceInformation`. If the camera turns the
//! login down there, nothing else is sent: one refused login costs the account one failure.

use hyper::Method;

use crate::discover::Found;
use crate::events::{self, Detects};
use crate::http::{Camera, HttpError, Target};
use crate::ptz::{self, Abilities};
use crate::settings::Login;
use crate::soap::{self, ClockOffset, Soap, attribute, find, find_all, text};

const DEVICE: &str = "http://www.onvif.org/ver10/device/wsdl";
const MEDIA: &str = "http://www.onvif.org/ver10/media/wsdl";

/// The most profiles looked at. Cameras have two or three; this bounds a strange one.
const MAX_PROFILES: usize = 8;

/// What a camera told us about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Info {
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub firmware: Option<String>,
    pub serial: Option<String>,
    pub media: Option<Target>,
    pub events: Option<Target>,
    pub ptz_service: Option<Target>,
    /// What it says it can notice.
    pub detects: Vec<Detects>,
    /// How it turns, and with which profile, if it does.
    pub turns: Option<Turning>,
    /// Video profiles, largest first.
    pub profiles: Vec<Profile>,
    /// Where a picture comes from: the smallest profile that has one.
    pub snapshot: Option<Target>,
    /// How its speaker is reached, when it has one that can be talked through.
    pub talk: Option<crate::talk::TalkTrack>,
    /// Whether it's on a cable (`true`) or Wi-Fi, when it says.
    pub wired: Option<bool>,
}

impl Info {
    /// Its full-size stream.
    pub fn high(&self) -> Option<&Profile> {
        self.playable().next()
    }

    /// Its small stream, when it has one apart from the full-size one.
    pub fn low(&self) -> Option<&Profile> {
        let mut playable = self.playable();
        let high = playable.next()?;
        playable.last().filter(|low| low.token != high.token)
    }

    /// The profiles whose video a browser can be sent: H.264 and H.265, largest first. A
    /// camera's JPEG profile (a Tapo has one) isn't live video.
    fn playable(&self) -> impl Iterator<Item = &Profile> {
        self.profiles.iter().filter(|profile| profile.plays())
    }
}

/// A camera that turns: the profile its moves go through, and what it can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turning {
    pub token: String,
    pub abilities: Abilities,
}

/// One way the camera can send its video.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    pub token: String,
    pub name: String,
    /// `H264`, `H265`, `JPEG`… as the camera writes it.
    pub encoding: String,
    pub width: u32,
    pub height: u32,
    /// Its sound, if it sends any: `AAC`, `G711`…
    pub audio: Option<String>,
    /// It can be turned (it has a PTZ configuration).
    pub ptz: bool,
    /// Its RTSP address, as the camera gave it.
    pub stream: Option<String>,
}

impl Profile {
    /// Whether it's H.264 or H.265, however the camera spells it.
    pub fn plays(&self) -> bool {
        let encoding: String = self
            .encoding
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_uppercase();
        matches!(encoding.as_str(), "H264" | "H265" | "HEVC")
    }
}

/// Measures the camera's clock. Needs no login, so it's also how a camera is checked to be
/// there before a login is spent on it.
pub async fn clock(camera: &Camera, device: &Target) -> Result<ClockOffset, HttpError> {
    let soap = Soap {
        camera: camera.clone(),
        login: None,
        offset: ClockOffset::default(),
    };
    let answer = soap
        .call(
            device,
            &format!("{DEVICE}/GetSystemDateAndTime"),
            "<tds:GetSystemDateAndTime/>",
        )
        .await?;
    // A camera that doesn't say its time gets ours; the token may still be accepted.
    Ok(soap::clock_offset(&answer).unwrap_or_default())
}

/// Logs in and reads everything this extension needs from the camera.
/// Says which question a failure came from. A refused login stays just that.
fn at(step: &'static str) -> impl Fn(HttpError) -> HttpError {
    move |error| match error {
        HttpError::Refused => HttpError::Refused,
        HttpError::Unreachable(why) => HttpError::Unreachable(format!("{step}: {why}")),
        HttpError::Failed(why) => HttpError::Failed(format!("{step}: {why}")),
    }
}

pub async fn connect(found: &Found, login: &Login) -> Result<(Soap, Info), HttpError> {
    let camera = Camera { ip: found.ip };
    let offset = clock(&camera, &found.device_service).await?;
    let soap = Soap {
        camera,
        login: Some(login.clone()),
        offset,
    };
    let device = &found.device_service;
    // The first request with the login. A refusal ends it here.
    let answer = soap
        .call(
            device,
            &format!("{DEVICE}/GetDeviceInformation"),
            "<tds:GetDeviceInformation/>",
        )
        .await
        .map_err(at("device information"))?;
    let mut info = device_information(&answer);

    let answer = soap
        .call(
            device,
            &format!("{DEVICE}/GetCapabilities"),
            "<tds:GetCapabilities><tds:Category>All</tds:Category></tds:GetCapabilities>",
        )
        .await
        .map_err(at("capabilities"))?;
    let (media, events, ptz) = capabilities(&answer);
    info.media = media;
    info.events = events;
    info.ptz_service = ptz;

    // For the network map; a camera that won't say is just drawn without knowing.
    info.wired = soap
        .call(
            device,
            &format!("{DEVICE}/GetNetworkInterfaces"),
            "<tds:GetNetworkInterfaces/>",
        )
        .await
        .ok()
        .and_then(|answer| wired(&answer));

    if let Some(media) = info.media.clone() {
        let answer = soap
            .call(
                &media,
                &format!("{MEDIA}/GetProfiles"),
                "<trt:GetProfiles/>",
            )
            .await
            .map_err(at("profiles"))?;
        let mut profiles = profiles(&answer);
        for profile in &mut profiles {
            profile.stream = stream_uri(&soap, &media, &profile.token)
                .await
                .map_err(at("stream address"))?;
        }
        // Smallest first, for the picture.
        for profile in profiles.iter().rev() {
            if let Some(snapshot) = snapshot_uri(&soap, &media, &profile.token)
                .await
                .map_err(at("picture address"))?
            {
                info.snapshot = Some(snapshot);
                break;
            }
        }
        info.profiles = profiles;
        // Never a reason to fail: a camera that won't say just can't be talked through.
        if let Some(stream) = info.high().and_then(|profile| profile.stream.clone()) {
            info.talk = crate::talk::ask(&stream, soap.camera.ip, login).await;
        }
    }
    if let Some(events) = info.events.clone() {
        info.detects = events::properties(&soap, &events)
            .await
            .map_err(at("events"))?;
    }
    if let (Some(service), Some(profile)) = (
        info.ptz_service.clone(),
        info.profiles.iter().find(|profile| profile.ptz),
    ) {
        let abilities = ptz::ask_abilities(&soap, &service)
            .await
            .map_err(at("turning"))?;
        if abilities.pan_tilt || abilities.zoom || abilities.presets {
            info.turns = Some(Turning {
                token: profile.token.clone(),
                abilities,
            });
        }
    }
    Ok((soap, info))
}

/// Takes a picture. JPEG is what ONVIF cameras send.
pub async fn picture(soap: &Soap, snapshot: &Target) -> Result<irori_protocol::Picture, HttpError> {
    let response = soap
        .camera
        .send(Method::GET, snapshot, None, soap.login.as_ref())
        .await?;
    if !(200..300).contains(&response.status) {
        return Err(HttpError::Failed(format!(
            "the camera answered {} when asked for a picture",
            response.status
        )));
    }
    let content_type = response
        .content_type
        .as_deref()
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_ascii_lowercase());
    let content_type = match content_type.as_deref() {
        Some(kind) if irori_protocol::PICTURE_TYPES.contains(&kind) => kind.to_owned(),
        // Some cameras leave it out or say `application/octet-stream`; a JPEG says so itself.
        _ if response.body.starts_with(&[0xff, 0xd8, 0xff]) => "image/jpeg".to_owned(),
        _ => {
            return Err(HttpError::Failed(
                "the camera's picture isn't a picture".into(),
            ));
        }
    };
    let picture = irori_protocol::Picture {
        content_type,
        bytes: response.body,
    };
    picture.check().map_err(HttpError::Failed)?;
    Ok(picture)
}

async fn stream_uri(soap: &Soap, media: &Target, token: &str) -> Result<Option<String>, HttpError> {
    let body = format!(
        "<trt:GetStreamUri><trt:StreamSetup><tt:Stream>RTP-Unicast</tt:Stream>\
         <tt:Transport><tt:Protocol>RTSP</tt:Protocol></tt:Transport></trt:StreamSetup>\
         <trt:ProfileToken>{}</trt:ProfileToken></trt:GetStreamUri>",
        soap::escape(token)
    );
    match soap
        .call(media, &format!("{MEDIA}/GetStreamUri"), &body)
        .await
    {
        Ok(answer) => Ok(uri_in(&answer)),
        Err(HttpError::Refused) => Err(HttpError::Refused),
        Err(_) => Ok(None),
    }
}

async fn snapshot_uri(
    soap: &Soap,
    media: &Target,
    token: &str,
) -> Result<Option<Target>, HttpError> {
    let body = format!(
        "<trt:GetSnapshotUri><trt:ProfileToken>{}</trt:ProfileToken></trt:GetSnapshotUri>",
        soap::escape(token)
    );
    match soap
        .call(media, &format!("{MEDIA}/GetSnapshotUri"), &body)
        .await
    {
        Ok(answer) => Ok(uri_in(&answer).and_then(|uri| Target::parse(&uri).ok())),
        Err(HttpError::Refused) => Err(HttpError::Refused),
        // Plenty of cameras have no pictures: they say so with a fault.
        Err(_) => Ok(None),
    }
}

fn uri_in(answer: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(answer).ok()?;
    let media_uri = find(doc.root(), "MediaUri")?;
    text(media_uri, &["Uri"])
}

/// Whether a camera is on a cable or Wi-Fi, from its network interfaces: the first one switched
/// on. Wi-Fi when it has 802.11 settings, its type is 71 (IANA's `ieee80211`) or its name is a
/// wireless one; a cable when its type is 6 (`ethernetCsmacd`) or it's called `eth…`.
pub fn wired(answer: &str) -> Option<bool> {
    let doc = roxmltree::Document::parse(answer).ok()?;
    let response = find(doc.root(), "GetNetworkInterfacesResponse")?;
    let interfaces = find_all(response, "NetworkInterfaces");
    let mut first = None;
    for interface in interfaces {
        if text(interface, &["Enabled"]).as_deref() == Some("false") {
            continue;
        }
        let name = text(interface, &["Info", "Name"])
            .or_else(|| attribute(interface, "token"))
            .unwrap_or_default()
            .to_ascii_lowercase();
        let types: Vec<String> = interface
            .descendants()
            .filter(|node| node.tag_name().name() == "InterfaceType")
            .filter_map(|node| node.text().map(|text| text.trim().to_owned()))
            .collect();
        let wifi = find(interface, "Dot11").is_some()
            || types.iter().any(|kind| kind == "71")
            || ["wlan", "wl", "ra", "wifi"]
                .iter()
                .any(|start| name.starts_with(start));
        let cable = types.iter().any(|kind| kind == "6") || name.starts_with("eth");
        tracing::info!(%name, ?types, wifi, cable, "a camera's network interface");
        let said = if wifi {
            Some(false)
        } else if cable {
            Some(true)
        } else {
            None
        };
        if first.is_none() {
            first = said;
        }
    }
    first
}

pub fn device_information(answer: &str) -> Info {
    let Ok(doc) = roxmltree::Document::parse(answer) else {
        return Info::default();
    };
    let Some(response) = find(doc.root(), "GetDeviceInformationResponse") else {
        return Info::default();
    };
    let field = |name: &str| text(response, &[name]).map(|value| value.chars().take(100).collect());
    Info {
        manufacturer: field("Manufacturer"),
        model: field("Model"),
        firmware: field("FirmwareVersion"),
        serial: field("SerialNumber"),
        ..Info::default()
    }
}

/// The media, events and PTZ services' addresses.
pub fn capabilities(answer: &str) -> (Option<Target>, Option<Target>, Option<Target>) {
    let Ok(doc) = roxmltree::Document::parse(answer) else {
        return (None, None, None);
    };
    let Some(capabilities) = find(doc.root(), "Capabilities") else {
        return (None, None, None);
    };
    let service = |name: &str| {
        capabilities
            .children()
            .find(|child| child.is_element() && child.tag_name().name() == name)
            .and_then(|node| text(node, &["XAddr"]))
            .and_then(|url| Target::parse(&url).ok())
    };
    (service("Media"), service("Events"), service("PTZ"))
}

/// The profiles that send video, largest first.
pub fn profiles(answer: &str) -> Vec<Profile> {
    let Ok(doc) = roxmltree::Document::parse(answer) else {
        return Vec::new();
    };
    let mut profiles: Vec<Profile> = find_all(doc.root(), "Profiles")
        .take(MAX_PROFILES)
        .filter_map(|node| {
            let token = attribute(node, "token")?;
            let video = node
                .children()
                .find(|child| child.tag_name().name() == "VideoEncoderConfiguration")?;
            let resolution = find(video, "Resolution")?;
            let number = |name: &str| text(resolution, &[name])?.parse::<u32>().ok();
            let audio = node
                .children()
                .find(|child| child.tag_name().name() == "AudioEncoderConfiguration")
                .and_then(|audio| text(audio, &["Encoding"]));
            Some(Profile {
                name: text(node, &["Name"]).unwrap_or_else(|| token.clone()),
                token,
                encoding: text(video, &["Encoding"]).unwrap_or_default(),
                width: number("Width")?,
                height: number("Height")?,
                audio,
                ptz: node
                    .children()
                    .any(|child| child.tag_name().name() == "PTZConfiguration"),
                stream: None,
            })
        })
        .collect();
    profiles.sort_by_key(|profile| {
        std::cmp::Reverse(u64::from(profile.width) * u64::from(profile.height))
    });
    profiles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_camera_says_whether_its_wired_or_on_wifi() {
        let interfaces = |inside: &str| {
            format!(
                "<s:Envelope xmlns:s=\"s\" xmlns:tds=\"d\" xmlns:tt=\"t\"><s:Body>\
                 <tds:GetNetworkInterfacesResponse>{inside}</tds:GetNetworkInterfacesResponse>\
                 </s:Body></s:Envelope>"
            )
        };
        let cable = interfaces(
            "<tds:NetworkInterfaces token=\"eth0\"><tt:Enabled>true</tt:Enabled>\
             <tt:Info><tt:Name>eth0</tt:Name></tt:Info>\
             <tt:Link><tt:InterfaceType>6</tt:InterfaceType></tt:Link></tds:NetworkInterfaces>",
        );
        assert_eq!(wired(&cable), Some(true));
        let wifi = interfaces(
            "<tds:NetworkInterfaces token=\"wlan0\"><tt:Enabled>true</tt:Enabled>\
             <tt:Extension><tt:InterfaceType>71</tt:InterfaceType><tt:Dot11/></tt:Extension>\
             </tds:NetworkInterfaces>",
        );
        assert_eq!(wired(&wifi), Some(false));
        let off_then_wifi = interfaces(
            "<tds:NetworkInterfaces token=\"eth0\"><tt:Enabled>false</tt:Enabled></tds:NetworkInterfaces>\
             <tds:NetworkInterfaces token=\"ra0\"><tt:Enabled>true</tt:Enabled></tds:NetworkInterfaces>",
        );
        assert_eq!(wired(&off_then_wifi), Some(false));
        assert_eq!(wired("not xml"), None);
    }

    #[test]
    fn profiles_are_sorted_into_full_size_and_small() {
        let answer = r#"<Envelope><Body><GetProfilesResponse>
          <Profiles token="000" fixed="true"><Name>sub</Name>
            <VideoEncoderConfiguration token="v1"><Encoding>H264</Encoding>
              <Resolution><Width>640</Width><Height>360</Height></Resolution></VideoEncoderConfiguration>
            <AudioEncoderConfiguration token="a1"><Encoding>AAC</Encoding></AudioEncoderConfiguration>
          </Profiles>
          <Profiles token="001"><Name>main</Name>
            <VideoEncoderConfiguration token="v0"><Encoding>H265</Encoding>
              <Resolution><Width>4608</Width><Height>1728</Height></Resolution></VideoEncoderConfiguration>
            <PTZConfiguration token="p0"/>
          </Profiles>
          <Profiles token="002"><Name>audio only</Name></Profiles>
        </GetProfilesResponse></Body></Envelope>"#;
        let profiles = profiles(answer);
        assert_eq!(profiles.len(), 2, "the one without video is left out");
        let info = Info {
            profiles,
            ..Info::default()
        };
        let high = info.high().expect("a full-size stream");
        assert_eq!(
            (high.token.as_str(), high.width, high.encoding.as_str()),
            ("001", 4608, "H265")
        );
        assert!(high.ptz);
        let low = info.low().expect("a small stream");
        assert_eq!(
            (low.token.as_str(), low.audio.as_deref()),
            ("000", Some("AAC"))
        );

        let one = Info {
            profiles: vec![low.clone()],
            ..Info::default()
        };
        assert!(
            one.low().is_none(),
            "one profile is only the full-size stream"
        );
    }

    /// A Tapo C210's three profiles: the smallest is JPEG, which isn't live video.
    #[test]
    fn a_jpeg_profile_is_never_the_small_stream() {
        let answer = r#"<Envelope><Body><GetProfilesResponse>
          <Profiles token="profile_1"><VideoEncoderConfiguration token="v1"><Encoding>H264</Encoding>
            <Resolution><Width>2304</Width><Height>1296</Height></Resolution></VideoEncoderConfiguration></Profiles>
          <Profiles token="profile_2"><VideoEncoderConfiguration token="v2"><Encoding>H264</Encoding>
            <Resolution><Width>640</Width><Height>360</Height></Resolution></VideoEncoderConfiguration></Profiles>
          <Profiles token="profile_3"><VideoEncoderConfiguration token="v3"><Encoding>JPEG</Encoding>
            <Resolution><Width>320</Width><Height>240</Height></Resolution></VideoEncoderConfiguration></Profiles>
        </GetProfilesResponse></Body></Envelope>"#;
        let info = Info {
            profiles: profiles(answer),
            ..Info::default()
        };
        assert_eq!(info.high().map(|p| p.token.as_str()), Some("profile_1"));
        assert_eq!(info.low().map(|p| p.token.as_str()), Some("profile_2"));
        let jpeg_only = Info {
            profiles: info.profiles[2..].to_vec(),
            ..Info::default()
        };
        assert!(jpeg_only.high().is_none(), "no live video at all");
    }

    #[test]
    fn the_services_are_read_from_the_capabilities() {
        let answer = r#"<Envelope><Body><GetCapabilitiesResponse><Capabilities>
          <Device><XAddr>http://192.168.68.64:2020/onvif/device_service</XAddr></Device>
          <Events><XAddr>http://192.168.68.64:2020/onvif/service</XAddr></Events>
          <Media><XAddr>http://192.168.68.64:2020/onvif/service</XAddr></Media>
          <PTZ><XAddr>http://192.168.68.64:2020/onvif/service</XAddr></PTZ>
        </Capabilities></GetCapabilitiesResponse></Body></Envelope>"#;
        let (media, events, ptz) = capabilities(answer);
        let service = Target {
            port: 2020,
            path: "/onvif/service".into(),
        };
        assert_eq!(media.as_ref(), Some(&service));
        assert_eq!(events.as_ref(), Some(&service));
        assert_eq!(ptz.as_ref(), Some(&service));
    }
}
