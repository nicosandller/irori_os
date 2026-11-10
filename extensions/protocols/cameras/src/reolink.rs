//! Reolink's own local API: what ONVIF doesn't reach, like the spotlight, the siren and the IR
//! lights.
//!
//! It's JSON over HTTPS on the camera (`/api.cgi`), with the same login as ONVIF. Logging in
//! sends the password inside the request body, so it only ever goes over HTTPS, and never in an
//! address, since addresses end up in logs. A login gives a token good for about an hour; it's
//! kept and used until it runs out or the camera says to log in again, then logged out of when
//! the camera's tasks stop. A camera only holds a few of them.
//!
//! A login turned down here is never tried again in this run: Reolink locks an account after a
//! few failures. It doesn't mark the ONVIF login refused, since ONVIF just accepted it: more
//! likely this account isn't allowed the API, or the camera has HTTPS turned off.

use std::time::Duration;

use irori_protocol::{IncomingCall, ServiceError};
use irori_types::Service;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::http::{Camera, HttpError};
use crate::settings::Login;

/// Where the API listens: HTTPS's own port, which Reolinks keep on by default.
pub const PORT: u16 = 443;
/// How long before a token runs out it's replaced.
const EARLY: Duration = Duration::from_secs(60);
/// What Reolink answers when a token is unknown or has run out.
const LOG_IN_AGAIN: i64 = -6;

/// What went wrong asking the camera something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// The login was turned down. Not to be tried again in this run.
    Refused(String),
    /// It couldn't be reached, or stopped answering.
    Unreachable(String),
    /// It answered, but not with what was asked for.
    Failed(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(why) => write!(f, "the camera's API turned its login down ({why})"),
            Self::Unreachable(why) | Self::Failed(why) => f.write_str(why),
        }
    }
}

impl From<HttpError> for ApiError {
    fn from(error: HttpError) -> Self {
        match error {
            HttpError::Refused => Self::Refused("HTTP login".into()),
            HttpError::Unreachable(why) => Self::Unreachable(why),
            HttpError::Failed(why) => Self::Failed(why),
        }
    }
}

struct Token {
    name: String,
    until: Instant,
}

/// One camera's API, logged in when first asked something.
pub struct Api {
    camera: Camera,
    port: u16,
    login: Login,
    token: Option<Token>,
}

impl Api {
    pub fn new(camera: Camera, port: u16, login: Login) -> Self {
        Self {
            camera,
            port,
            login,
            token: None,
        }
    }

    /// Runs one command and returns its `value`. Logs in first if there's no token or it's
    /// about to run out, and once more if the camera says the token is no good.
    pub async fn call(&mut self, cmd: &str, action: u8, param: Value) -> Result<Value, ApiError> {
        for fresh in [false, true] {
            if fresh
                || self
                    .token
                    .as_ref()
                    .is_none_or(|token| Instant::now() + EARLY >= token.until)
            {
                self.log_in().await?;
            }
            let Some(token) = &self.token else {
                return Err(ApiError::Failed("the camera gave no token".into()));
            };
            let body = json!([{ "cmd": cmd, "action": action, "param": param }]);
            let path = format!("/api.cgi?cmd={cmd}&token={}", token.name);
            let answer = self.send(&path, &body).await?;
            match answer_of(&answer, cmd) {
                Err(Answered::Code(LOG_IN_AGAIN, _)) if !fresh => {
                    self.token = None;
                }
                Err(Answered::Code(code, detail)) => {
                    return Err(ApiError::Failed(format!(
                        "the camera said no: {detail} ({code})"
                    )));
                }
                Err(Answered::Unreadable) => {
                    return Err(ApiError::Failed("the camera's answer can't be read".into()));
                }
                Ok(value) => return Ok(value),
            }
        }
        Err(ApiError::Failed(
            "the camera keeps asking to log in again".into(),
        ))
    }

    /// Gives the token back, so it doesn't hold one of the camera's few places.
    pub async fn log_out(&mut self) {
        let Some(token) = self.token.take() else {
            return;
        };
        let body = json!([{ "cmd": "Logout", "param": {} }]);
        let path = format!("/api.cgi?cmd=Logout&token={}", token.name);
        let _ = self.send(&path, &body).await;
    }

    async fn log_in(&mut self) -> Result<(), ApiError> {
        self.token = None;
        let body = json!([{
            "cmd": "Login",
            "param": { "User": {
                "Version": "0",
                "userName": self.login.username,
                "password": self.login.password,
            }}
        }]);
        let answer = self.send("/api.cgi?cmd=Login", &body).await?;
        let value = match answer_of(&answer, "Login") {
            Ok(value) => value,
            Err(Answered::Code(code, detail)) => {
                return Err(ApiError::Refused(format!("{detail}, {code}")));
            }
            Err(Answered::Unreadable) => {
                return Err(ApiError::Failed(
                    "the camera's login answer can't be read".into(),
                ));
            }
        };
        let token = &value["Token"];
        let name = token["name"]
            .as_str()
            .filter(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric()))
            .ok_or_else(|| ApiError::Failed("the camera gave no token".into()))?;
        let lease = token["leaseTime"]
            .as_u64()
            .unwrap_or(3600)
            .clamp(120, 24 * 3600);
        self.token = Some(Token {
            name: name.to_owned(),
            until: Instant::now() + Duration::from_secs(lease),
        });
        Ok(())
    }

    async fn send(&self, path: &str, body: &Value) -> Result<Value, ApiError> {
        let bytes = serde_json::to_vec(body).unwrap_or_default();
        let response = self.camera.post_tls(self.port, path, bytes).await?;
        if response.status == 401 {
            return Err(ApiError::Refused("HTTP 401".into()));
        }
        if !(200..300).contains(&response.status) {
            return Err(ApiError::Failed(format!(
                "the camera's API answered HTTP {}",
                response.status
            )));
        }
        serde_json::from_slice(&response.body)
            .map_err(|_| ApiError::Failed("the camera's answer isn't JSON".into()))
    }
}

/// Why an answer wasn't a value.
#[derive(Debug, PartialEq, Eq)]
pub enum Answered {
    /// The camera's error code and what it said.
    Code(i64, String),
    Unreadable,
}

/// The `value` of the answer to `cmd`, from the list the camera sends back.
pub fn answer_of(answer: &Value, cmd: &str) -> Result<Value, Answered> {
    let item = answer
        .as_array()
        .and_then(|items| items.iter().find(|item| item["cmd"] == cmd))
        .ok_or(Answered::Unreadable)?;
    match item["code"].as_i64() {
        Some(0) => Ok(item["value"].clone()),
        Some(_) => {
            let error = &item["error"];
            let code = error["rspCode"].as_i64().unwrap_or(0);
            let detail: String = error["detail"]
                .as_str()
                .unwrap_or("no reason given")
                .chars()
                .filter(|c| !c.is_control())
                .take(120)
                .collect();
            Err(Answered::Code(code, detail))
        }
        None => Err(Answered::Unreadable),
    }
}

/// One of the extras a Reolink can have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Part {
    Spotlight,
    Siren,
    /// Its infrared lights, on (`Auto`: when it's dark) or off.
    Infrared,
    /// The light on its front that says it's on.
    StatusLight,
}

impl Part {
    pub const ALL: [Self; 4] = [
        Self::Spotlight,
        Self::Siren,
        Self::Infrared,
        Self::StatusLight,
    ];

    /// The start of its entity's unique id.
    pub fn key(self) -> &'static str {
        match self {
            Self::Spotlight => "spotlight",
            Self::Siren => "siren",
            Self::Infrared => "infrared",
            Self::StatusLight => "status_light",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Spotlight => "Spotlight",
            Self::Siren => "Siren",
            Self::Infrared => "Infrared lights at night",
            Self::StatusLight => "Status light",
        }
    }
}

/// What a camera has, from its abilities and what it answers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Offers {
    pub parts: Vec<Part>,
    /// The spotlight can be dimmed.
    pub dimmable: bool,
    /// On a cable (`true`) or Wi-Fi, as the camera's own API says: ONVIF doesn't, on a Reolink.
    pub wired: Option<bool>,
}

/// What one part is doing now, as the camera says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Now {
    /// On or off, and how bright from 1 to 255 when it can be dimmed.
    Light {
        on: bool,
        brightness: Option<u8>,
    },
    On(bool),
}

/// What the worker tells the camera's loop.
#[derive(Debug)]
pub enum Report {
    Offers(Offers),
    Now(Part, Now),
}

/// How often what the lights are doing is read again, for changes made elsewhere.
const POLL: Duration = Duration::from_secs(30);
/// How long the siren sounds when not told, and the longest it's ever left on.
const SIREN: Duration = Duration::from_secs(10);
const SIREN_MOST: Duration = Duration::from_secs(5 * 60);
/// Waits between tries while the API can't be reached.
const AGAIN: [u64; 4] = [30, 60, 120, 300];

/// Whether an ability is granted: `{"permit": n, "ver": n}` with a permit.
fn granted(abilities: &Value, name: &str) -> bool {
    abilities[name]["permit"]
        .as_u64()
        .is_some_and(|permit| permit > 0)
}

/// What `GetAbility` says the first channel and the device can do.
fn from_abilities(answer: &Value) -> (bool, bool, bool) {
    let all = &answer["Ability"];
    let channel = &all["abilityChn"][0];
    let spotlight = granted(channel, "supportFLswitch");
    let dimmable = granted(channel, "supportFLBrightness");
    let siren = granted(all, "supportAudioAlarm") || granted(channel, "supportAudioAlarm");
    (spotlight, dimmable, siren)
}

/// Reolink's 0-100 brightness as Irori's 1-255.
pub fn to_brightness(percent: u64) -> u8 {
    u8::try_from((percent.min(100) * 255 + 50) / 100)
        .unwrap_or(u8::MAX)
        .max(1)
}

/// Irori's 1-255 brightness as Reolink's 1-100.
pub fn to_percent(brightness: u8) -> u64 {
    ((u64::from(brightness) * 100 + 127) / 255).clamp(1, 100)
}

/// Reads one part as the camera has it now.
async fn read(api: &mut Api, part: Part, dimmable: bool) -> Result<Now, ApiError> {
    let channel = json!({ "channel": 0 });
    match part {
        Part::Spotlight => {
            let value = api.call("GetWhiteLed", 0, channel).await?;
            let led = &value["WhiteLed"];
            let on = led["state"].as_u64().ok_or_else(unreadable)? == 1;
            let brightness = dimmable
                .then(|| led["bright"].as_u64().map(to_brightness))
                .flatten();
            Ok(Now::Light { on, brightness })
        }
        Part::Infrared => {
            let value = api.call("GetIrLights", 0, channel).await?;
            let state = value["IrLights"]["state"].as_str().ok_or_else(unreadable)?;
            Ok(Now::On(state != "Off"))
        }
        Part::StatusLight => {
            let value = api.call("GetPowerLed", 0, channel).await?;
            let state = value["PowerLed"]["state"].as_str().ok_or_else(unreadable)?;
            Ok(Now::On(state != "Off"))
        }
        // The camera can't say whether it's sounding: it's what was last asked.
        Part::Siren => Err(ApiError::Failed("a siren can't be read".into())),
    }
}

fn unreadable() -> ApiError {
    ApiError::Failed("the camera's answer can't be read".into())
}

/// Asks a camera what it has: its abilities, then each light that answers.
async fn offers(api: &mut Api) -> Result<Offers, ApiError> {
    let username = api.login.username.clone();
    let abilities = api
        .call("GetAbility", 0, json!({ "User": { "userName": username } }))
        .await?;
    let (spotlight, dimmable, siren) = from_abilities(&abilities);
    let mut parts = Vec::new();
    if spotlight && read(api, Part::Spotlight, dimmable).await.is_ok() {
        parts.push(Part::Spotlight);
    }
    if siren {
        parts.push(Part::Siren);
    }
    for part in [Part::Infrared, Part::StatusLight] {
        if read(api, part, false).await.is_ok() {
            parts.push(part);
        }
    }
    let wired = match api.call("GetLocalLink", 0, json!({})).await {
        Ok(value) => link_wired(&value),
        Err(error) => {
            tracing::debug!("a Reolink didn't say how it's connected: {error}");
            None
        }
    };
    Ok(Offers {
        parts,
        dimmable,
        wired,
    })
}

/// Whether `GetLocalLink` says the camera is on its cable: `activeLink` is `LAN` or `Wifi`.
fn link_wired(value: &Value) -> Option<bool> {
    let link = value["LocalLink"]["activeLink"]
        .as_str()?
        .to_ascii_lowercase();
    tracing::info!(%link, "a Reolink's active link");
    if link.contains("wifi") || link.contains("wlan") {
        Some(false)
    } else if link == "lan" || link.contains("eth") {
        Some(true)
    } else {
        None
    }
}

/// Runs one camera's extras until `calls` closes: says what it has, keeps what each part is
/// doing up to date, and carries out calls for them. A login turned down ends it for this run.
pub async fn run(
    mut api: Api,
    mut calls: mpsc::Receiver<(Part, IncomingCall)>,
    reports: mpsc::Sender<Report>,
) {
    let ip = api.camera.ip;
    let mut attempt = 0usize;
    let offers = loop {
        match offers(&mut api).await {
            Ok(offers) => break offers,
            Err(ApiError::Refused(why)) => {
                tracing::warn!(%ip, "a Reolink's own API turned its login down, so its spotlight, siren and lights are left out: {why}");
                return;
            }
            Err(error) => {
                tracing::info!(%ip, "a Reolink's own API couldn't be asked what it has: {error}");
                let wait = AGAIN[attempt.min(AGAIN.len() - 1)];
                attempt += 1;
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(wait)) => {}
                    () = reports.closed() => return,
                }
            }
        }
    };
    if offers.parts.is_empty() && offers.wired.is_none() {
        api.log_out().await;
        return;
    }
    let dimmable = offers.dimmable;
    let parts = offers.parts.clone();
    if reports.send(Report::Offers(offers)).await.is_err() {
        api.log_out().await;
        return;
    }
    let mut known: Vec<(Part, Now)> = Vec::new();
    let mut siren_until: Option<Instant> = None;
    let mut poll_at = Instant::now();
    loop {
        if Instant::now() >= poll_at {
            poll_at = Instant::now() + POLL;
            for part in parts.iter().copied().filter(|part| *part != Part::Siren) {
                match read(&mut api, part, dimmable).await {
                    Ok(now) => {
                        if !tell(&reports, &mut known, part, now).await {
                            break;
                        }
                    }
                    Err(ApiError::Refused(why)) => {
                        tracing::warn!(%ip, "a Reolink's own API turned its login down: {why}");
                        return;
                    }
                    Err(error) => {
                        tracing::debug!(%ip, "a Reolink's lights couldn't be read: {error}")
                    }
                }
            }
            if parts.contains(&Part::Siren) && known.iter().all(|(part, _)| *part != Part::Siren) {
                tell(&reports, &mut known, Part::Siren, Now::On(false)).await;
            }
        }
        let next = tokio::select! {
            next = calls.recv() => next,
            () = sleep_until(siren_until) => {
                siren_until = None;
                if siren(&mut api, false).await.is_ok() {
                    tell(&reports, &mut known, Part::Siren, Now::On(false)).await;
                }
                continue;
            }
            () = tokio::time::sleep_until(poll_at) => continue,
        };
        let Some((part, call)) = next else {
            if siren_until.is_some() {
                let _ = siren(&mut api, false).await;
            }
            api.log_out().await;
            return;
        };
        let result = carry_out(&mut api, part, &call.call.service, &mut siren_until).await;
        if matches!(result, Err(ApiError::Refused(_))) {
            call.reply(Err(ServiceError::unavailable(
                "the camera turned its login down",
            )));
            tracing::warn!(%ip, "a Reolink's own API turned its login down");
            return;
        }
        // What it's doing now goes out before the answer, so the page's switch lands as the
        // command comes back rather than at the next poll.
        match &result {
            Ok(Some(now)) => {
                tell(&reports, &mut known, part, *now).await;
            }
            Ok(None) => {
                let wanted = asked_on(&call.call.service);
                match read_back(&mut api, part, dimmable, wanted).await {
                    Ok(now) => {
                        tell(&reports, &mut known, part, now).await;
                    }
                    Err(error) => {
                        tracing::debug!(%ip, "a Reolink's light couldn't be read back: {error}")
                    }
                }
            }
            Err(_) => {}
        }
        call.reply(result.map(|_| ()).map_err(|error| match error {
            ApiError::Refused(_) => ServiceError::unavailable("the camera turned its login down"),
            ApiError::Unreachable(why) => ServiceError::unavailable(why),
            ApiError::Failed(why) => ServiceError::failed(why),
        }));
    }
}

/// Whether a call asks for on or off.
fn asked_on(service: &Service) -> Option<bool> {
    match service {
        Service::LightTurnOn(_) | Service::SwitchTurnOn => Some(true),
        Service::LightTurnOff | Service::SwitchTurnOff => Some(false),
        _ => None,
    }
}

/// How long after a change it's read again, while the camera still says what it was: a Reolink
/// can take a moment to say what it's just been told.
const READ_BACK: [Duration; 4] = [
    Duration::from_millis(0),
    Duration::from_millis(400),
    Duration::from_millis(900),
    Duration::from_millis(1700),
];

/// Reads a part after changing it, again for a few seconds until it says what was asked, and
/// what it says in the end.
async fn read_back(
    api: &mut Api,
    part: Part,
    dimmable: bool,
    wanted: Option<bool>,
) -> Result<Now, ApiError> {
    let mut last = Err(ApiError::Failed("not read".into()));
    for wait in READ_BACK {
        tokio::time::sleep(wait).await;
        last = read(api, part, dimmable).await;
        let on = match &last {
            Ok(Now::Light { on, .. } | Now::On(on)) => Some(*on),
            Err(_) => None,
        };
        tracing::info!(part = part.key(), ?wanted, ?on, "a Reolink part read back");
        if wanted.is_none() || on == wanted {
            break;
        }
    }
    last
}

/// Sends what a part is doing, if it changed. False once nobody listens.
async fn tell(
    reports: &mpsc::Sender<Report>,
    known: &mut Vec<(Part, Now)>,
    part: Part,
    now: Now,
) -> bool {
    match known.iter_mut().find(|(known, _)| *known == part) {
        Some((_, was)) if *was == now => return true,
        Some((_, was)) => *was = now,
        None => known.push((part, now)),
    }
    reports.send(Report::Now(part, now)).await.is_ok()
}

/// Does what a call asks. `Some` is what the part is doing now when it can't be read back.
async fn carry_out(
    api: &mut Api,
    part: Part,
    service: &Service,
    siren_until: &mut Option<Instant>,
) -> Result<Option<Now>, ApiError> {
    let unsupported = || Err(ApiError::Failed("it can't do that".into()));
    match (part, service) {
        (Part::Spotlight, Service::LightTurnOn(on)) => {
            let mut led = json!({ "channel": 0, "state": 1 });
            if let Some(brightness) = on.brightness {
                led["bright"] = to_percent(brightness).into();
            }
            api.call("SetWhiteLed", 0, json!({ "WhiteLed": led }))
                .await?;
            Ok(None)
        }
        (Part::Spotlight, Service::LightTurnOff) => {
            api.call(
                "SetWhiteLed",
                0,
                json!({ "WhiteLed": { "channel": 0, "state": 0 } }),
            )
            .await?;
            Ok(None)
        }
        (Part::Infrared, Service::SwitchTurnOn | Service::SwitchTurnOff) => {
            let state = if matches!(service, Service::SwitchTurnOn) {
                "Auto"
            } else {
                "Off"
            };
            api.call(
                "SetIrLights",
                0,
                json!({ "IrLights": { "channel": 0, "state": state } }),
            )
            .await?;
            Ok(None)
        }
        (Part::StatusLight, Service::SwitchTurnOn | Service::SwitchTurnOff) => {
            let state = if matches!(service, Service::SwitchTurnOn) {
                "On"
            } else {
                "Off"
            };
            api.call(
                "SetPowerLed",
                0,
                json!({ "PowerLed": { "channel": 0, "state": state } }),
            )
            .await?;
            Ok(None)
        }
        (Part::Siren, Service::SirenTurnOn(on)) => {
            siren(api, true).await?;
            let lasts = on
                .duration
                .map_or(SIREN, |seconds| Duration::from_secs(u64::from(seconds)))
                .min(SIREN_MOST);
            *siren_until = Some(Instant::now() + lasts);
            Ok(Some(Now::On(true)))
        }
        (Part::Siren, Service::SirenTurnOff) => {
            *siren_until = None;
            siren(api, false).await?;
            Ok(Some(Now::On(false)))
        }
        _ => unsupported(),
    }
}

async fn siren(api: &mut Api, on: bool) -> Result<(), ApiError> {
    let param = json!({ "channel": 0, "alarm_mode": "manul", "manual_switch": u8::from(on) });
    api.call("AudioAlarmPlay", 0, param).await.map(|_| ())
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_gives_its_value_or_why_not() {
        let fine = json!([{ "cmd": "GetIrLights", "code": 0, "value": { "IrLights": { "state": "Auto" } } }]);
        assert_eq!(
            answer_of(&fine, "GetIrLights"),
            Ok(json!({ "IrLights": { "state": "Auto" } }))
        );
        let again = json!([{ "cmd": "GetIrLights", "code": 1, "error": { "detail": "please login first", "rspCode": -6 } }]);
        assert_eq!(
            answer_of(&again, "GetIrLights"),
            Err(Answered::Code(-6, "please login first".into()))
        );
        assert_eq!(answer_of(&fine, "GetPowerLed"), Err(Answered::Unreadable));
        assert_eq!(
            answer_of(&json!({}), "GetIrLights"),
            Err(Answered::Unreadable)
        );
    }
}
