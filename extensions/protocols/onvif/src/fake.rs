//! A fake ONVIF camera on a loopback port, for tests.
//!
//! It checks logins the way a camera does (the WS-Security digest on SOAP, HTTP Digest on the
//! picture) and counts every attempt, so a test can see exactly how many times a login was
//! spent.
//!
//! The answers are written from the ONVIF specification's examples, in the shapes the cameras
//! on the user's network use. They are synthetic, and get replaced by captured ones once a real
//! camera has been logged into.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use md5::{Digest as _, Md5};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::soap::{find, password_digest, text};

/// A picture small enough to read: a JPEG's start and end markers around nothing much.
pub const JPEG: &[u8] = &[
    0xff, 0xd8, 0xff, 0xe0, 0, 16, b'J', b'F', b'I', b'F', 0, 0xff, 0xd9,
];

const REALM: &str = "fake camera";
const NONCE: &str = "6b1a2c3d";

/// What the camera was asked, for a test to look at.
#[derive(Debug, Default, Clone)]
pub struct Seen {
    /// Requests that carried a login, of any kind.
    pub logins: u32,
    /// Those the camera turned down.
    pub refused: u32,
    /// SOAP actions, in order, by their element name.
    pub actions: Vec<String>,
    pub pictures: u32,
    /// Each `ContinuousMove`'s pan and tilt speeds, as sent.
    pub moves: Vec<String>,
    pub stops: u32,
    /// The presets it was sent to.
    pub went_to: Vec<String>,
}

/// Something the camera notices: a topic, the value's name, and whether it's on.
type Notice = (&'static str, &'static str, bool);

#[derive(Debug, Clone)]
pub struct FakeCamera {
    pub port: u16,
    seen: Arc<Mutex<Seen>>,
    noticed: Arc<Mutex<VecDeque<Notice>>>,
}

impl FakeCamera {
    /// Starts one that takes `username` and `password`. `pictures` is whether it has any.
    pub async fn start(username: &str, password: &str, pictures: bool) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("a port");
        let port = listener.local_addr().expect("an address").port();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let noticed = Arc::new(Mutex::new(VecDeque::new()));
        let camera = Arc::new(Camera {
            port,
            username: username.to_owned(),
            password: password.to_owned(),
            pictures,
            seen: Arc::clone(&seen),
            noticed: Arc::clone(&noticed),
        });
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let camera = Arc::clone(&camera);
                tokio::spawn(async move { camera.serve(stream).await });
            }
        });
        Self {
            port,
            seen,
            noticed,
        }
    }

    pub fn seen(&self) -> Seen {
        self.seen.lock().expect("seen").clone()
    }

    /// Makes the camera notice something; the next pull of its events carries it.
    pub fn notice(&self, topic: &'static str, item: &'static str, on: bool) {
        self.noticed
            .lock()
            .expect("noticed")
            .push_back((topic, item, on));
    }
}

struct Camera {
    port: u16,
    username: String,
    password: String,
    pictures: bool,
    seen: Arc<Mutex<Seen>>,
    noticed: Arc<Mutex<VecDeque<Notice>>>,
}

pub struct Asked {
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
    pub body: String,
}

impl Camera {
    async fn serve(&self, mut stream: TcpStream) {
        let Some(asked) = read(&mut stream).await else {
            return;
        };
        let (status, extra, content_type, body) = self.answer(&asked).await;
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes()).await;
        let _ = stream.write_all(&body).await;
    }

    async fn answer(&self, asked: &Asked) -> (&'static str, String, &'static str, Vec<u8>) {
        if asked.method == "GET" && asked.path == "/snap.jpg" {
            return self.picture(asked);
        }
        if let Some(answer) = self.events_and_turns(asked).await {
            return answer;
        }
        let Ok(doc) = roxmltree::Document::parse(&asked.body) else {
            return ("400 Bad Request", String::new(), "text/plain", Vec::new());
        };
        let Some(action) = find(doc.root(), "Body")
            .and_then(|body| body.children().find(roxmltree::Node::is_element))
            .map(|node| node.tag_name().name().to_owned())
        else {
            return ("400 Bad Request", String::new(), "text/plain", Vec::new());
        };
        self.seen.lock().expect("seen").actions.push(action.clone());
        if action != "GetSystemDateAndTime" {
            match self.token_ok(&doc) {
                Some(true) => self.seen.lock().expect("seen").logins += 1,
                Some(false) => {
                    let mut seen = self.seen.lock().expect("seen");
                    seen.logins += 1;
                    seen.refused += 1;
                    return soap("400 Bad Request", NOT_AUTHORIZED.to_owned());
                }
                None => return soap("400 Bad Request", NOT_AUTHORIZED.to_owned()),
            }
        }
        let base = format!("http://127.0.0.1:{}", self.port);
        let body = match action.as_str() {
            "GetSystemDateAndTime" => date_and_time(),
            "GetDeviceInformation" => DEVICE_INFORMATION.to_owned(),
            "GetCapabilities" => CAPABILITIES.replace("{base}", &base),
            "GetProfiles" => PROFILES.to_owned(),
            "GetStreamUri" => {
                let token = text(
                    find(doc.root(), "GetStreamUri").expect("asked"),
                    &["ProfileToken"],
                )
                .unwrap_or_default();
                format!(
                    "<trt:GetStreamUriResponse><trt:MediaUri>\
                     <tt:Uri>rtsp://192.168.68.200:554/{token}</tt:Uri>\
                     </trt:MediaUri></trt:GetStreamUriResponse>"
                )
            }
            "GetSnapshotUri" if self.pictures => format!(
                "<trt:GetSnapshotUriResponse><trt:MediaUri><tt:Uri>{base}/snap.jpg</tt:Uri>\
                 </trt:MediaUri></trt:GetSnapshotUriResponse>"
            ),
            _ => return soap("500 Internal Server Error", NOT_SUPPORTED.to_owned()),
        };
        soap("200 OK", body)
    }

    /// The events and PTZ services, once the login is checked; `None` for anything else.
    async fn events_and_turns(
        &self,
        asked: &Asked,
    ) -> Option<(&'static str, String, &'static str, Vec<u8>)> {
        let doc = roxmltree::Document::parse(&asked.body).ok()?;
        let request = find(doc.root(), "Body")?
            .children()
            .find(roxmltree::Node::is_element)?;
        let action = request.tag_name().name().to_owned();
        let known = [
            "GetEventProperties",
            "CreatePullPointSubscription",
            "PullMessages",
            "Renew",
            "Unsubscribe",
            "GetNodes",
            "GetPresets",
            "ContinuousMove",
            "Stop",
            "GotoPreset",
        ];
        if !known.contains(&action.as_str()) {
            return None;
        }
        {
            let mut seen = self.seen.lock().expect("seen");
            seen.actions.push(action.clone());
            seen.logins += 1;
        }
        if self.token_ok(&doc) != Some(true) {
            self.seen.lock().expect("seen").refused += 1;
            return Some(soap("400 Bad Request", NOT_AUTHORIZED.to_owned()));
        }
        let base = format!("http://127.0.0.1:{}", self.port);
        let body = match action.as_str() {
            "GetEventProperties" => EVENT_PROPERTIES.to_owned(),
            "CreatePullPointSubscription" => format!(
                "<tev:CreatePullPointSubscriptionResponse><tev:SubscriptionReference>\
                 <wsa:Address>{base}/onvif/subscription?Idx=1</wsa:Address>\
                 </tev:SubscriptionReference></tev:CreatePullPointSubscriptionResponse>"
            ),
            "PullMessages" => {
                // A long poll, kept short: an answer as soon as something's noticed.
                let mut noticed = Vec::new();
                for _ in 0..10 {
                    noticed.extend(self.noticed.lock().expect("noticed").drain(..));
                    if !noticed.is_empty() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                let messages: String = noticed
                    .into_iter()
                    .map(|(topic, item, on)| {
                        format!(
                            "<wsnt:NotificationMessage><wsnt:Topic Dialect=\"x\">{topic}</wsnt:Topic>\
                             <wsnt:Message><tt:Message UtcTime=\"2026-11-01T10:00:00Z\" \
                             PropertyOperation=\"Changed\"><tt:Source><tt:SimpleItem \
                             Name=\"Source\" Value=\"000\"/></tt:Source><tt:Data><tt:SimpleItem \
                             Name=\"{item}\" Value=\"{on}\"/></tt:Data></tt:Message></wsnt:Message>\
                             </wsnt:NotificationMessage>"
                        )
                    })
                    .collect();
                format!("<tev:PullMessagesResponse>{messages}</tev:PullMessagesResponse>")
            }
            "Renew" => "<wsnt:RenewResponse/>".to_owned(),
            "Unsubscribe" => "<wsnt:UnsubscribeResponse/>".to_owned(),
            "GetNodes" => NODES.to_owned(),
            "GetPresets" => PRESETS.to_owned(),
            "ContinuousMove" => {
                let speeds = find(request, "PanTilt")
                    .map(|node| {
                        format!(
                            "{},{}",
                            node.attribute("x").unwrap_or_default(),
                            node.attribute("y").unwrap_or_default()
                        )
                    })
                    .unwrap_or_default();
                self.seen.lock().expect("seen").moves.push(speeds);
                "<tptz:ContinuousMoveResponse/>".to_owned()
            }
            "Stop" => {
                self.seen.lock().expect("seen").stops += 1;
                "<tptz:StopResponse/>".to_owned()
            }
            _ => {
                let preset = text(request, &["PresetToken"]).unwrap_or_default();
                self.seen.lock().expect("seen").went_to.push(preset);
                "<tptz:GotoPresetResponse/>".to_owned()
            }
        };
        Some(soap("200 OK", body))
    }

    /// Whether the WS-Security token is right; `None` when there isn't one.
    fn token_ok(&self, doc: &roxmltree::Document) -> Option<bool> {
        let token = find(doc.root(), "UsernameToken")?;
        let username = text(token, &["Username"])?;
        let digest = text(token, &["Password"])?;
        let nonce = base64::engine::general_purpose::STANDARD
            .decode(text(token, &["Nonce"])?)
            .ok()?;
        let created = text(token, &["Created"])?;
        Some(
            username == self.username
                && digest == password_digest(&nonce, &created, &self.password),
        )
    }

    fn picture(&self, asked: &Asked) -> (&'static str, String, &'static str, Vec<u8>) {
        let challenge = format!(
            "WWW-Authenticate: Digest realm=\"{REALM}\", qop=\"auth\", nonce=\"{NONCE}\", algorithm=MD5\r\n"
        );
        let Some(authorization) = &asked.authorization else {
            return ("401 Unauthorized", challenge, "text/plain", Vec::new());
        };
        let ok = self.digest_ok(authorization, &asked.method, &asked.path);
        let mut seen = self.seen.lock().expect("seen");
        seen.logins += 1;
        if !ok {
            seen.refused += 1;
            return ("401 Unauthorized", challenge, "text/plain", Vec::new());
        }
        seen.pictures += 1;
        ("200 OK", String::new(), "image/jpeg", JPEG.to_vec())
    }

    fn digest_ok(&self, header: &str, method: &str, uri: &str) -> bool {
        let Some(rest) = header.strip_prefix("Digest ") else {
            return false;
        };
        let params = crate::http::params(rest);
        let get = |key: &str| {
            params
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
                .unwrap_or_default()
        };
        let md5 = |text: String| crate::http::hex(&Md5::digest(text.as_bytes()));
        let ha1 = md5(format!("{}:{REALM}:{}", self.username, self.password));
        let ha2 = md5(format!("{method}:{uri}"));
        let expected = md5(format!(
            "{ha1}:{NONCE}:{}:{}:auth:{ha2}",
            get("nc"),
            get("cnonce")
        ));
        get("username") == self.username && get("uri") == uri && get("response") == expected
    }
}

fn soap(status: &'static str, body: String) -> (&'static str, String, &'static str, Vec<u8>) {
    let envelope = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <s:Envelope xmlns:s=\"http://www.w3.org/2003/05/soap-envelope\" \
         xmlns:tds=\"http://www.onvif.org/ver10/device/wsdl\" \
         xmlns:trt=\"http://www.onvif.org/ver10/media/wsdl\" \
         xmlns:tt=\"http://www.onvif.org/ver10/schema\" \
         xmlns:tev=\"http://www.onvif.org/ver10/events/wsdl\" \
         xmlns:tptz=\"http://www.onvif.org/ver20/ptz/wsdl\" \
         xmlns:wsnt=\"http://docs.oasis-open.org/wsn/b-2\" \
         xmlns:wstop=\"http://docs.oasis-open.org/wsn/t-1\" \
         xmlns:tns1=\"http://www.onvif.org/ver10/topics\" \
         xmlns:wsa=\"http://www.w3.org/2005/08/addressing\" \
         xmlns:ter=\"http://www.onvif.org/ver10/error\"><s:Body>{body}</s:Body></s:Envelope>"
    );
    (
        status,
        String::new(),
        "application/soap+xml; charset=utf-8",
        envelope.into_bytes(),
    )
}

pub async fn read<S: tokio::io::AsyncRead + Unpin>(stream: &mut S) -> Option<Asked> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(at) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    let mut length = 0usize;
    let mut authorization = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().unwrap_or(0);
        } else if name.eq_ignore_ascii_case("authorization") {
            authorization = Some(value.trim().to_owned());
        }
    }
    while buffer.len() < head_end + length {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body = String::from_utf8_lossy(&buffer[head_end..]).into_owned();
    Some(Asked {
        method,
        path,
        authorization,
        body,
    })
}

fn date_and_time() -> String {
    let now = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC);
    format!(
        "<tds:GetSystemDateAndTimeResponse><tds:SystemDateAndTime>\
         <tt:DateTimeType>NTP</tt:DateTimeType><tt:DaylightSavings>false</tt:DaylightSavings>\
         <tt:UTCDateTime><tt:Time><tt:Hour>{}</tt:Hour><tt:Minute>{}</tt:Minute>\
         <tt:Second>{}</tt:Second></tt:Time><tt:Date><tt:Year>{}</tt:Year>\
         <tt:Month>{}</tt:Month><tt:Day>{}</tt:Day></tt:Date></tt:UTCDateTime>\
         </tds:SystemDateAndTime></tds:GetSystemDateAndTimeResponse>",
        now.hour(),
        now.minute(),
        now.second(),
        now.year(),
        now.month(),
        now.day()
    )
}

const NOT_AUTHORIZED: &str = "<s:Fault><s:Code><s:Value>s:Sender</s:Value><s:Subcode>\
    <s:Value>ter:NotAuthorized</s:Value></s:Subcode></s:Code><s:Reason>\
    <s:Text xml:lang=\"en\">Sender not Authorized</s:Text></s:Reason></s:Fault>";

const NOT_SUPPORTED: &str = "<s:Fault><s:Code><s:Value>s:Receiver</s:Value><s:Subcode>\
    <s:Value>ter:ActionNotSupported</s:Value></s:Subcode></s:Code><s:Reason>\
    <s:Text xml:lang=\"en\">Optional Action Not Implemented</s:Text></s:Reason></s:Fault>";

const DEVICE_INFORMATION: &str = "<tds:GetDeviceInformationResponse>\
    <tds:Manufacturer>Reolink</tds:Manufacturer><tds:Model>Reolink Duo 2V PoE</tds:Model>\
    <tds:FirmwareVersion>v3.0.0.0000_00000000</tds:FirmwareVersion>\
    <tds:SerialNumber>00000000000000</tds:SerialNumber><tds:HardwareId>IPC_000</tds:HardwareId>\
    </tds:GetDeviceInformationResponse>";

const CAPABILITIES: &str = "<tds:GetCapabilitiesResponse><tds:Capabilities>\
    <tt:Device><tt:XAddr>{base}/onvif/device_service</tt:XAddr></tt:Device>\
    <tt:Events><tt:XAddr>{base}/onvif/event_service</tt:XAddr></tt:Events>\
    <tt:Media><tt:XAddr>{base}/onvif/media_service</tt:XAddr></tt:Media>\
    <tt:PTZ><tt:XAddr>{base}/onvif/ptz_service</tt:XAddr></tt:PTZ>\
    </tds:Capabilities></tds:GetCapabilitiesResponse>";

const PROFILES: &str = "<trt:GetProfilesResponse>\
    <trt:Profiles token=\"000\" fixed=\"true\"><tt:Name>MainStream</tt:Name>\
    <tt:VideoEncoderConfiguration token=\"000\"><tt:Name>main</tt:Name>\
    <tt:Encoding>H264</tt:Encoding><tt:Resolution><tt:Width>4608</tt:Width>\
    <tt:Height>1728</tt:Height></tt:Resolution></tt:VideoEncoderConfiguration>\
    <tt:AudioEncoderConfiguration token=\"000\"><tt:Encoding>AAC</tt:Encoding>\
    </tt:AudioEncoderConfiguration><tt:PTZConfiguration token=\"p0\"/></trt:Profiles>\
    <trt:Profiles token=\"001\" fixed=\"true\"><tt:Name>SubStream</tt:Name>\
    <tt:VideoEncoderConfiguration token=\"001\"><tt:Name>sub</tt:Name>\
    <tt:Encoding>H264</tt:Encoding><tt:Resolution><tt:Width>896</tt:Width>\
    <tt:Height>336</tt:Height></tt:Resolution></tt:VideoEncoderConfiguration>\
    </trt:Profiles></trt:GetProfilesResponse>";

/// Motion, the way a Tapo names it, and people, the way a Reolink does.
const EVENT_PROPERTIES: &str = "<tev:GetEventPropertiesResponse><wstop:TopicSet>\
    <tns1:RuleEngine><CellMotionDetector><Motion wstop:topic=\"true\"><tt:MessageDescription>\
    <tt:Data><tt:SimpleItemDescription Name=\"IsMotion\" Type=\"xs:boolean\"/></tt:Data>\
    </tt:MessageDescription></Motion></CellMotionDetector>\
    <MyRuleDetector><PeopleDetect wstop:topic=\"true\"><tt:MessageDescription><tt:Data>\
    <tt:SimpleItemDescription Name=\"State\" Type=\"xs:boolean\"/></tt:Data>\
    </tt:MessageDescription></PeopleDetect></MyRuleDetector></tns1:RuleEngine>\
    </wstop:TopicSet></tev:GetEventPropertiesResponse>";

const NODES: &str = "<tptz:GetNodesResponse><tptz:PTZNode token=\"node\">\
    <tt:SupportedPTZSpaces><tt:ContinuousPanTiltVelocitySpace><tt:URI>x</tt:URI>\
    </tt:ContinuousPanTiltVelocitySpace></tt:SupportedPTZSpaces>\
    <tt:MaximumNumberOfPresets>8</tt:MaximumNumberOfPresets></tptz:PTZNode>\
    </tptz:GetNodesResponse>";

const PRESETS: &str = "<tptz:GetPresetsResponse>\
    <tptz:Preset token=\"1\"><tt:Name>Driveway</tt:Name></tptz:Preset>\
    <tptz:Preset token=\"2\"><tt:Name>Gate</tt:Name></tptz:Preset>\
    </tptz:GetPresetsResponse>";
