//! SwitchBot's cloud API, v1.1: the device list, a device's status, and commands. Every call is
//! signed (`sign`) and counted by the caller against the day's budget.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use irori_cloud::reqwest::{self, StatusCode};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::map::Command;
use crate::settings::Account;
use crate::sign;

/// Where SwitchBot's API lives.
pub const API: &str = "https://api.switch-bot.com";

/// A hub's id when a device has none: it is a hub, or it's on Wi-Fi itself.
const NO_HUB: &str = "000000000000";

/// What went wrong talking to SwitchBot, in terms of what Irori does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudError {
    /// HTTP 401. Either the token and secret are wrong, or the account's calls for the day are
    /// used up: SwitchBot answers both the same way.
    Unauthorized,
    /// HTTP 429, with how long SwitchBot asked to be left alone, when it said.
    TooMany(Option<Duration>),
    /// The device is offline (161).
    DeviceOffline,
    /// The hub it's reached through is offline (171).
    HubOffline,
    /// SwitchBot has no such device any more (152).
    NotFound,
    /// SwitchBot refused the command (151, 160) or failed (190), with its words.
    Refused { code: i64, message: String },
    /// Couldn't reach SwitchBot, or it answered with an HTTP error.
    Unreachable(String),
}

impl CloudError {
    /// A sentence for a person, never repeating a credential.
    pub fn describe(&self) -> String {
        match self {
            CloudError::Unauthorized => "SwitchBot refused the token and secret key".into(),
            CloudError::TooMany(_) => "SwitchBot asked Irori to slow down".into(),
            CloudError::DeviceOffline => "the device is offline".into(),
            CloudError::HubOffline => "the hub it's reached through is offline".into(),
            CloudError::NotFound => "SwitchBot no longer has this device".into(),
            CloudError::Refused { code, message } if message.is_empty() => {
                format!("SwitchBot refused it (code {code})")
            }
            CloudError::Refused { code, message } => {
                format!("SwitchBot refused it: {message} (code {code})")
            }
            CloudError::Unreachable(why) => format!("couldn't reach SwitchBot: {why}"),
        }
    }
}

/// A device in the account's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// Its MAC address, upper case, no colons: the same id it has over Bluetooth.
    pub id: String,
    pub name: String,
    pub device_type: String,
    /// The hub it's reached through, if any.
    pub hub: Option<String>,
}

/// An infrared remote the account's hub has learned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub id: String,
    pub name: String,
    pub remote_type: String,
    pub hub: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceList {
    pub devices: Vec<Listed>,
    pub remotes: Vec<Remote>,
}

/// The account, signed in.
#[derive(Debug, Clone)]
pub struct Cloud {
    http: reqwest::Client,
    base: String,
    account: Account,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "statusCode")]
    status_code: i64,
    #[serde(default)]
    body: Value,
    #[serde(default)]
    message: String,
}

impl Cloud {
    pub fn new(http: reqwest::Client, base: impl Into<String>, account: Account) -> Self {
        Self {
            http,
            base: base.into(),
            account,
        }
    }

    pub async fn devices(&self) -> Result<DeviceList, CloudError> {
        let body = self
            .call(reqwest::Method::GET, "/v1.1/devices", None)
            .await?;
        let mut list = DeviceList::default();
        let entries = |key: &str| {
            body.get(key)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        for entry in entries("deviceList") {
            let (Some(id), Some(device_type)) =
                (string(&entry, "deviceId"), string(&entry, "deviceType"))
            else {
                continue;
            };
            list.devices.push(Listed {
                name: string(&entry, "deviceName").unwrap_or_else(|| device_type.clone()),
                hub: hub(&entry, &id),
                id: id.to_ascii_uppercase(),
                device_type,
            });
        }
        for entry in entries("infraredRemoteList") {
            let (Some(id), Some(remote_type)) =
                (string(&entry, "deviceId"), string(&entry, "remoteType"))
            else {
                continue;
            };
            list.remotes.push(Remote {
                name: string(&entry, "deviceName").unwrap_or_else(|| remote_type.clone()),
                hub: hub(&entry, &id),
                id,
                remote_type,
            });
        }
        Ok(list)
    }

    pub async fn status(&self, id: &str) -> Result<Map<String, Value>, CloudError> {
        let body = self
            .call(
                reqwest::Method::GET,
                &format!("/v1.1/devices/{id}/status"),
                None,
            )
            .await?;
        match body {
            Value::Object(status) => Ok(status),
            _ => Err(CloudError::Unreachable("SwitchBot sent no status".into())),
        }
    }

    pub async fn command(&self, id: &str, command: &Command) -> Result<(), CloudError> {
        let body = json!({
            "commandType": "command",
            "command": command.command,
            "parameter": command.parameter,
        });
        self.call(
            reqwest::Method::POST,
            &format!("/v1.1/devices/{id}/commands"),
            Some(body),
        )
        .await
        .map(drop)
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, CloudError> {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        let signed = sign::sign(&self.account, millis, &sign::nonce());
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("Authorization", self.account.token.expose())
            .header("t", signed.t)
            .header("nonce", signed.nonce)
            .header("sign", signed.sign)
            .header("Content-Type", "application/json; charset=utf8");
        if let Some(body) = body {
            request = request.body(body.to_string());
        }
        let response = request.send().await.map_err(|error| {
            CloudError::Unreachable(if error.is_timeout() {
                "it didn't answer in time".into()
            } else {
                "no connection".into()
            })
        })?;
        let status = response.status();
        match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(CloudError::Unauthorized);
            }
            StatusCode::TOO_MANY_REQUESTS => {
                return Err(CloudError::TooMany(irori_cloud::retry_after(
                    response.headers(),
                )));
            }
            _ if !status.is_success() => {
                return Err(CloudError::Unreachable(format!("HTTP {}", status.as_u16())));
            }
            _ => {}
        }
        let envelope: Envelope = response
            .json()
            .await
            .map_err(|_| CloudError::Unreachable("its answer wasn't readable".into()))?;
        match envelope.status_code {
            100 => Ok(envelope.body),
            152 => Err(CloudError::NotFound),
            161 => Err(CloudError::DeviceOffline),
            171 => Err(CloudError::HubOffline),
            code => Err(CloudError::Refused {
                code,
                message: envelope.message.chars().take(200).collect(),
            }),
        }
    }
}

fn string(entry: &Value, key: &str) -> Option<String> {
    entry
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn hub(entry: &Value, id: &str) -> Option<String> {
    string(entry, "hubDeviceId")
        .filter(|hub| hub != NO_HUB && !hub.eq_ignore_ascii_case(id))
        .map(|hub| hub.to_ascii_uppercase())
}
