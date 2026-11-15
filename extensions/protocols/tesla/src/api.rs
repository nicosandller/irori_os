//! Tesla's Fleet API, every call of it metered.
//!
//! All requests go through [`send`], which writes the call into the usage ledger as soon as an
//! answer arrives — before the answer is read, so a reply that can't be read, or a refusal, is
//! counted just as Tesla bills it. Each endpoint's wrapper then makes sense of what came back:
//! the same status means different things on different endpoints (a 404 from the cached-data
//! endpoint is "nothing cached yet", not "this car is gone").
//!
//! Before a call leaves, it waits for room under Tesla's per-car limits (60 data requests, 3
//! wakes and 30 commands a minute), so Irori never pays for being told to slow down.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use irori_cloud::reqwest::{self, Method};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::time::Instant;

use prost::Message as _;

use crate::auth::Session;
use crate::signed::proto::RoutableMessage;
use crate::usage::price::{Category, Endpoint};
use crate::usage::{Usage, Why};

/// The usage ledger, shared by every call in flight and the loop that reports it.
pub type Meter = Arc<Mutex<Usage>>;

/// The time, in seconds since 1970. Tests fix it.
pub type Clock = fn() -> u64;

/// Where Tesla's servers are. Tests point them at a fake on a local port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hosts {
    /// Tesla's sign-in server, where tokens come from.
    pub auth: String,
    pub na: String,
    pub eu: String,
}

impl Hosts {
    pub fn tesla() -> Self {
        Self {
            auth: "https://fleet-auth.prd.vn.cloud.tesla.com".to_owned(),
            na: "https://fleet-api.prd.na.vn.cloud.tesla.com".to_owned(),
            eu: "https://fleet-api.prd.eu.vn.cloud.tesla.com".to_owned(),
        }
    }

    pub fn of(&self, region: Region) -> &str {
        match region {
            Region::Na => &self.na,
            Region::Eu => &self.eu,
        }
    }

    pub fn token_url(&self) -> String {
        format!("{}/oauth2/v3/token", self.auth)
    }
}

/// Which of Tesla's servers an account lives on. China has its own, which Irori doesn't reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Region {
    /// North America and Asia-Pacific.
    Na,
    /// Europe, the Middle East and Africa.
    Eu,
}

impl Region {
    pub const ALL: [Region; 2] = [Region::Na, Region::Eu];

    /// From the code Tesla's sign-in hands back, which starts with the region: `NA_…`, `EU_…`.
    pub fn from_code(code: &str) -> Option<Region> {
        Self::from_label(code.split('_').next()?)
    }

    /// From an access token's `ou_code`.
    pub fn from_token(token: &str) -> Option<Region> {
        use base64::Engine as _;
        let payload = token.split('.').nth(1)?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload.trim_end_matches('='))
            .ok()?;
        let claims: Value = serde_json::from_slice(&bytes).ok()?;
        Self::from_label(claims.get("ou_code")?.as_str()?)
    }

    fn from_label(label: &str) -> Option<Region> {
        match label.to_ascii_lowercase().as_str() {
            "na" => Some(Region::Na),
            "eu" => Some(Region::Eu),
            _ => None,
        }
    }

    pub fn other(self) -> Region {
        match self {
            Region::Na => Region::Eu,
            Region::Eu => Region::Na,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Region::Na => "North America",
            Region::Eu => "Europe",
        }
    }
}

/// What went wrong, in terms of what Irori does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// 408: the car is asleep or out of reach. Billed, and not a fault.
    Asleep,
    /// 429, with how long Tesla asked to be left alone.
    RateLimited(Option<Duration>),
    /// 401, or a token Tesla won't refresh: someone has to sign in again.
    SignedOut(String),
    /// 402, or a refusal that talks about billing: Tesla has suspended the API for this
    /// account until the billing limit is raised or the month turns.
    Billing(String),
    /// 403 for another reason: a permission (scope) the sign-in didn't grant, or a car that
    /// doesn't allow it.
    Forbidden(String),
    /// 404.
    NotFound,
    /// 412: on most endpoints, the app isn't registered with Tesla in this region.
    Precondition(String),
    /// 421: the account lives in the other region.
    WrongRegion,
    /// Any other refusal.
    Refused { status: u16, message: String },
    /// Tesla's side failed (5xx). Not billed.
    Server(u16),
    /// No answer at all.
    Unreachable(String),
}

impl ApiError {
    /// A sentence for a person, never repeating a credential.
    pub fn describe(&self) -> String {
        match self {
            ApiError::Asleep => "the car is asleep".into(),
            ApiError::RateLimited(_) => "Tesla asked Irori to slow down".into(),
            ApiError::SignedOut(why) if why.is_empty() => "Tesla signed Irori out".into(),
            ApiError::SignedOut(why) => format!("Tesla signed Irori out: {why}"),
            ApiError::Billing(why) => format!("Tesla suspended the API for this account: {why}"),
            ApiError::Forbidden(why) => format!("Tesla doesn't allow it: {why}"),
            ApiError::NotFound => "Tesla has no such car or site".into(),
            ApiError::Precondition(why) => format!("Tesla refused it: {why}"),
            ApiError::WrongRegion => "the account lives on Tesla's other server".into(),
            ApiError::Refused { status, message } if message.is_empty() => {
                format!("Tesla refused it (HTTP {status})")
            }
            ApiError::Refused { status, message } => {
                format!("Tesla refused it: {message} (HTTP {status})")
            }
            ApiError::Server(status) => format!("Tesla's servers failed (HTTP {status})"),
            ApiError::Unreachable(why) => format!("couldn't reach Tesla: {why}"),
        }
    }
}

/// Room under Tesla's per-car, per-minute limits.
#[derive(Debug, Default)]
pub struct Limits {
    sent: BTreeMap<(String, Category), VecDeque<Instant>>,
}

const LIMIT_WINDOW: Duration = Duration::from_secs(60);

impl Limits {
    fn most(category: Category) -> Option<usize> {
        match category {
            Category::Data => Some(60),
            Category::Wake => Some(3),
            Category::Command => Some(30),
            Category::Free => None,
        }
    }

    /// Takes a place for a call now, or says how long until there's one.
    pub fn take(&mut self, target: &str, category: Category, now: Instant) -> Option<Duration> {
        let most = Self::most(category)?;
        let sent = self.sent.entry((target.to_owned(), category)).or_default();
        while sent
            .front()
            .is_some_and(|at| now.duration_since(*at) >= LIMIT_WINDOW)
        {
            sent.pop_front();
        }
        if sent.len() >= most {
            let oldest = *sent.front()?;
            return Some(LIMIT_WINDOW.saturating_sub(now.duration_since(oldest)));
        }
        sent.push_back(now);
        None
    }
}

/// One call, as the ledger sees it.
#[derive(Debug, Clone)]
pub struct Ask {
    /// The car's VIN, the site's id, or "account".
    pub target: String,
    pub endpoint: Endpoint,
    pub why: Why,
}

impl Ask {
    pub fn new(target: impl Into<String>, endpoint: Endpoint, why: Why) -> Self {
        Self {
            target: target.into(),
            endpoint,
            why,
        }
    }
}

/// The target a call about the account (not one car or site) is counted under.
pub const ACCOUNT: &str = "account";

/// What Tesla answered.
#[derive(Debug, Clone)]
pub struct Answer {
    pub status: u16,
    pub body: Value,
    pub retry_after: Option<Duration>,
}

/// Sends one request and meters it. Every request to Tesla, sign-in included, comes through
/// here.
pub async fn send(
    request: reqwest::RequestBuilder,
    ask: &Ask,
    meter: &Meter,
    now: Clock,
) -> Result<Answer, ApiError> {
    let response = request.send().await.map_err(|error| {
        ApiError::Unreachable(if error.is_timeout() {
            "it didn't answer in time".into()
        } else {
            "no connection".into()
        })
    })?;
    let status = response.status().as_u16();
    // Billed or not, it's on the meter before anything can go wrong reading it.
    if let Ok(mut usage) = meter.lock() {
        usage.record(now(), &ask.target, ask.endpoint, status, ask.why.clone());
    }
    let retry_after = irori_cloud::retry_after(response.headers());
    let text = response.text().await.unwrap_or_default();
    let body = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok(Answer {
        status,
        body,
        retry_after,
    })
}

/// Tesla's words for a refusal, shortened: `error_description`, else `error`.
pub fn words(body: &Value) -> String {
    let text = ["error_description", "error", "message"]
        .iter()
        .filter_map(|key| body.get(key).and_then(Value::as_str))
        .find(|text| !text.trim().is_empty())
        .unwrap_or_default();
    text.chars().take(200).collect()
}

/// Whether a refusal is about money rather than permission.
fn about_billing(body: &Value) -> bool {
    let all = format!(
        "{} {}",
        body.get("error")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        body.get("error_description")
            .and_then(Value::as_str)
            .unwrap_or_default()
    )
    .to_ascii_lowercase();
    [
        "billing",
        "payment",
        "credit",
        "suspend",
        "spend limit",
        "usage limit",
    ]
    .iter()
    .any(|word| all.contains(word))
}

/// The usual meaning of an answer. Endpoints for which a status means something else look at
/// [`Answer::status`] first.
pub fn classify(answer: Answer) -> Result<Value, ApiError> {
    let Answer {
        status,
        body,
        retry_after,
    } = answer;
    let words = words(&body);
    match status {
        200..=299 => Ok(body.get("response").cloned().unwrap_or(body)),
        401 => Err(ApiError::SignedOut(words)),
        402 => Err(ApiError::Billing(if words.is_empty() {
            "payment required".into()
        } else {
            words
        })),
        403 if about_billing(&body) => Err(ApiError::Billing(words)),
        403 => Err(ApiError::Forbidden(words)),
        404 => Err(ApiError::NotFound),
        408 => Err(ApiError::Asleep),
        412 => Err(ApiError::Precondition(words)),
        421 => Err(ApiError::WrongRegion),
        429 => Err(ApiError::RateLimited(retry_after)),
        500.. => Err(ApiError::Server(status)),
        _ => Err(ApiError::Refused {
            status,
            message: words,
        }),
    }
}

/// A car as `/products` and `/vehicles` list it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Car {
    pub vin: String,
    pub name: String,
    /// "online", "asleep" or "offline".
    pub state: String,
}

/// An energy site (Powerwall, solar) as `/products` lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Site {
    pub id: String,
    pub name: String,
    pub battery: bool,
    pub solar: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Products {
    pub cars: Vec<Car>,
    pub sites: Vec<Site>,
}

/// Whether a car's key is on it, from `fleet_status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyStatus {
    pub paired: Vec<String>,
    pub unpaired: Vec<String>,
    /// Cars that take only Tesla's older, unsigned commands (Model S and X from before 2021),
    /// by VIN. A car Tesla says nothing about is taken to need signed ones, as every newer car
    /// does.
    pub unsigned: Vec<String>,
}

/// The parts of `vehicle_data` Irori reads. Location is asked for separately: it lights the
/// location-sharing icon in the car.
pub const READ_PARTS: [&str; 7] = [
    "charge_state",
    "climate_state",
    "closures_state",
    "drive_state",
    "gui_settings",
    "vehicle_config",
    "vehicle_state",
];

/// The account, signed in.
#[derive(Clone)]
pub struct Api {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    hosts: Hosts,
    session: Session,
    meter: Meter,
    limits: Mutex<Limits>,
    now: Clock,
}

impl std::fmt::Debug for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Api")
            .field("hosts", &self.inner.hosts)
            .finish_non_exhaustive()
    }
}

impl Api {
    pub fn new(
        http: reqwest::Client,
        hosts: Hosts,
        session: Session,
        meter: Meter,
        now: Clock,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                http,
                hosts,
                session,
                meter,
                limits: Mutex::new(Limits::default()),
                now,
            }),
        }
    }

    pub fn session(&self) -> &Session {
        &self.inner.session
    }

    pub fn meter(&self) -> &Meter {
        &self.inner.meter
    }

    /// A call to the account's region, signed in as the person, retried once with a fresh
    /// token if Tesla says the token has expired, and once on the other region if Tesla says
    /// the account isn't on this one.
    async fn call(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
        ask: &Ask,
    ) -> Result<Answer, ApiError> {
        let mut refreshed = false;
        let mut moved = false;
        loop {
            // Every try is billed, the one after a refresh or a move too, so each waits for room.
            self.wait_for_room(ask).await;
            let (token, region) = self.inner.session.access(&self.inner.meter).await?;
            let url = format!("{}{path}", self.inner.hosts.of(region));
            let mut request = self
                .inner
                .http
                .request(method.clone(), url)
                .bearer_auth(token.expose());
            if !query.is_empty() {
                request = request.query(query);
            }
            if let Some(body) = &body {
                request = request.json(body);
            }
            let answer = send(request, ask, &self.inner.meter, self.inner.now).await?;
            match answer.status {
                401 if !refreshed => {
                    refreshed = true;
                    self.inner
                        .session
                        .refresh_after(&token, &self.inner.meter)
                        .await?;
                }
                421 if !moved => {
                    moved = true;
                    self.inner.session.move_to(region.other()).await;
                }
                _ => return Ok(answer),
            }
        }
    }

    async fn wait_for_room(&self, ask: &Ask) {
        let (category, _) = ask.endpoint.category();
        loop {
            let wait = self
                .inner
                .limits
                .lock()
                .ok()
                .and_then(|mut limits| limits.take(&ask.target, category, Instant::now()));
            match wait {
                None => return,
                Some(wait) => tokio::time::sleep(wait).await,
            }
        }
    }

    pub async fn products(&self, why: Why) -> Result<Products, ApiError> {
        let ask = Ask::new(ACCOUNT, Endpoint::Products, why);
        let answer = self
            .call(Method::GET, "/api/1/products", &[], None, &ask)
            .await?;
        let list = classify(answer)?;
        let mut products = Products::default();
        for entry in list.as_array().into_iter().flatten() {
            if let Some(vin) = text(entry, "vin") {
                products.cars.push(Car {
                    name: text(entry, "display_name").unwrap_or_else(|| vin.clone()),
                    state: text(entry, "state").unwrap_or_else(|| "offline".into()),
                    vin,
                });
            } else if let Some(id) = entry.get("energy_site_id").and_then(|id| {
                id.as_u64()
                    .map(|id| id.to_string())
                    .or_else(|| text(entry, "energy_site_id"))
            }) {
                let components = entry.get("components").cloned().unwrap_or(Value::Null);
                let has = |key: &str| components.get(key).and_then(Value::as_bool) == Some(true);
                let battery = has("battery");
                let solar = has("solar");
                // A site with neither (a Wall Connector on its own) has nothing to read.
                if battery || solar {
                    products.sites.push(Site {
                        name: text(entry, "site_name").unwrap_or_else(|| "Energy site".into()),
                        id,
                        battery,
                        solar,
                    });
                }
            }
        }
        Ok(products)
    }

    /// Every car's online/asleep state, in one call.
    pub async fn vehicles(&self, why: Why) -> Result<Vec<Car>, ApiError> {
        let ask = Ask::new(ACCOUNT, Endpoint::Vehicles, why);
        let answer = self
            .call(Method::GET, "/api/1/vehicles", &[], None, &ask)
            .await?;
        let list = classify(answer)?;
        Ok(list
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let vin = text(entry, "vin")?;
                Some(Car {
                    name: text(entry, "display_name").unwrap_or_else(|| vin.clone()),
                    state: text(entry, "state").unwrap_or_else(|| "offline".into()),
                    vin,
                })
            })
            .collect())
    }

    pub async fn fleet_status(&self, vins: &[String], why: Why) -> Result<KeyStatus, ApiError> {
        let ask = Ask::new(ACCOUNT, Endpoint::FleetStatus, why);
        let answer = self
            .call(
                Method::POST,
                "/api/1/vehicles/fleet_status",
                &[],
                Some(json!({ "vins": vins })),
                &ask,
            )
            .await?;
        let status = classify(answer)?;
        let list = |key: &str| -> Vec<String> {
            status
                .get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|vin| vin.as_str().map(str::to_owned))
                .collect()
        };
        let unsigned = status
            .get("vehicle_info")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(_, info)| {
                info.get("vehicle_command_protocol_required")
                    .and_then(Value::as_bool)
                    == Some(false)
            })
            .map(|(vin, _)| vin.clone())
            .collect();
        Ok(KeyStatus {
            paired: list("key_paired_vins"),
            unpaired: list("unpaired_vins"),
            unsigned,
        })
    }

    /// The car's data, live. Answers [`ApiError::Asleep`] (billed) when it's asleep.
    pub async fn vehicle_data(
        &self,
        vin: &str,
        location: bool,
        why: Why,
    ) -> Result<Value, ApiError> {
        let ask = Ask::new(vin, Endpoint::VehicleData, why);
        let mut parts: Vec<&str> = READ_PARTS.to_vec();
        if location {
            parts.push("location_data");
        }
        let answer = self
            .call(
                Method::GET,
                &format!("/api/1/vehicles/{vin}/vehicle_data"),
                &[("endpoints", parts.join(";"))],
                None,
                &ask,
            )
            .await?;
        classify(answer)
    }

    /// What the car last reported, without waking it: no location or driving in it, and parts
    /// may be hours old. `None` when Tesla has nothing cached or the car can't do it.
    pub async fn vehicle_data_cached(
        &self,
        vin: &str,
        why: Why,
    ) -> Result<Option<Value>, ApiError> {
        let ask = Ask::new(vin, Endpoint::VehicleDataCached, why);
        let answer = self
            .call(
                Method::GET,
                &format!("/api/1/vehicles/{vin}/vehicle_data_cached"),
                &[],
                None,
                &ask,
            )
            .await?;
        if matches!(answer.status, 404 | 412) {
            return Ok(None);
        }
        classify(answer).map(Some)
    }

    /// Asks the car to wake. It answers before the car is awake; [`Api::vehicles`] says when it
    /// is.
    pub async fn wake_up(&self, vin: &str, why: Why) -> Result<String, ApiError> {
        let ask = Ask::new(vin, Endpoint::WakeUp, why);
        let answer = self
            .call(
                Method::POST,
                &format!("/api/1/vehicles/{vin}/wake_up"),
                &[],
                None,
                &ask,
            )
            .await?;
        let car = classify(answer)?;
        Ok(text(&car, "state").unwrap_or_else(|| "offline".into()))
    }

    /// A signed message for a car (`crate::signed`), and the car's answer. Answers
    /// [`ApiError::Asleep`] (billed) when the car is asleep, and it never got the message.
    pub async fn signed_command(
        &self,
        vin: &str,
        message: &RoutableMessage,
        why: Why,
    ) -> Result<RoutableMessage, ApiError> {
        use base64::Engine as _;
        use base64::engine::general_purpose::STANDARD;
        let ask = Ask::new(vin, Endpoint::SignedCommand, why);
        let answer = self
            .call(
                Method::POST,
                &format!("/api/1/vehicles/{vin}/signed_command"),
                &[],
                Some(json!({ "routable_message": STANDARD.encode(message.encode_to_vec()) })),
                &ask,
            )
            .await?;
        let status = answer.status;
        let garbled = || ApiError::Refused {
            status,
            message: "Tesla's answer couldn't be read".into(),
        };
        let response = classify(answer)?;
        let bytes = response
            .as_str()
            .and_then(|text| STANDARD.decode(text).ok())
            .ok_or_else(garbled)?;
        RoutableMessage::decode(bytes.as_slice()).map_err(|_| ApiError::Refused {
            status,
            message: "the car's answer couldn't be read".into(),
        })
    }

    /// A change to an energy site's settings: `endpoint` is what follows
    /// `/api/1/energy_sites/{site}/`.
    pub async fn energy_command(
        &self,
        site: &str,
        endpoint: &str,
        body: Value,
        why: Why,
    ) -> Result<(), ApiError> {
        let ask = Ask::new(site, Endpoint::EnergyCommand, why);
        let answer = self
            .call(
                Method::POST,
                &format!("/api/1/energy_sites/{site}/{endpoint}"),
                &[],
                Some(body),
                &ask,
            )
            .await?;
        classify(answer).map(|_| ())
    }

    pub async fn site_info(&self, site: &str, why: Why) -> Result<Value, ApiError> {
        let ask = Ask::new(site, Endpoint::SiteInfo, why);
        let answer = self
            .call(
                Method::GET,
                &format!("/api/1/energy_sites/{site}/site_info"),
                &[],
                None,
                &ask,
            )
            .await?;
        classify(answer)
    }

    pub async fn live_status(&self, site: &str, why: Why) -> Result<Value, ApiError> {
        let ask = Ask::new(site, Endpoint::LiveStatus, why);
        let answer = self
            .call(
                Method::GET,
                &format!("/api/1/energy_sites/{site}/live_status"),
                &[],
                None,
                &ask,
            )
            .await?;
        classify(answer)
    }

    /// The site's day so far, in buckets: its own day, in its own time zone, as Tesla answers
    /// without dates.
    pub async fn energy_history(&self, site: &str, why: Why) -> Result<Value, ApiError> {
        let ask = Ask::new(site, Endpoint::EnergyHistory, why);
        let answer = self
            .call(
                Method::GET,
                &format!("/api/1/energy_sites/{site}/calendar_history"),
                &[("kind", "energy".to_owned()), ("period", "day".to_owned())],
                None,
                &ask,
            )
            .await?;
        classify(answer)
    }

    /// Registers the app with Tesla in one region, as the app itself (not the person).
    pub async fn register(&self, region: Region, domain: &str) -> Result<(), ApiError> {
        let token = self
            .inner
            .session
            .partner_token(region, &self.inner.meter)
            .await?;
        let ask = Ask::new(ACCOUNT, Endpoint::RegisterPartner, Why::Setup);
        let request = self
            .inner
            .http
            .post(format!(
                "{}/api/1/partner_accounts",
                self.inner.hosts.of(region)
            ))
            .bearer_auth(token.expose())
            .json(&json!({ "domain": domain }));
        let answer = send(request, &ask, &self.inner.meter, self.inner.now).await?;
        classify(answer).map(drop)
    }

    /// The public key Tesla has on file for the app's domain in one region, as Tesla gives it
    /// (hex of the key's point), or `None` when it has none.
    pub async fn registered_key(
        &self,
        region: Region,
        domain: &str,
    ) -> Result<Option<String>, ApiError> {
        let token = self
            .inner
            .session
            .partner_token(region, &self.inner.meter)
            .await?;
        let ask = Ask::new(ACCOUNT, Endpoint::PartnerPublicKey, Why::Setup);
        let request = self
            .inner
            .http
            .get(format!(
                "{}/api/1/partner_accounts/public_key",
                self.inner.hosts.of(region)
            ))
            .bearer_auth(token.expose())
            .query(&[("domain", domain)]);
        let answer = send(request, &ask, &self.inner.meter, self.inner.now).await?;
        if matches!(answer.status, 404 | 412) {
            return Ok(None);
        }
        let found = classify(answer)?;
        Ok(text(&found, "public_key"))
    }
}

pub fn text(entry: &Value, key: &str) -> Option<String> {
    entry
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_region_comes_from_the_code_or_the_token() {
        assert_eq!(Region::from_code("NA_abc123"), Some(Region::Na));
        assert_eq!(Region::from_code("EU_abc123"), Some(Region::Eu));
        assert_eq!(Region::from_code("CN_abc123"), None);
        assert_eq!(Region::from_code("abc"), None);
        let token = crate::fake::token("EU", 3600);
        assert_eq!(Region::from_token(&token), Some(Region::Eu));
        assert_eq!(Region::from_token("not.a.token"), None);
    }

    #[test]
    fn each_refusal_means_what_tesla_says_it_means() {
        let answer = |status: u16, body: Value| Answer {
            status,
            body,
            retry_after: Some(Duration::from_secs(30)),
        };
        assert_eq!(
            classify(answer(200, json!({ "response": [1] }))),
            Ok(json!([1]))
        );
        assert_eq!(classify(answer(408, Value::Null)), Err(ApiError::Asleep));
        assert_eq!(
            classify(answer(429, Value::Null)),
            Err(ApiError::RateLimited(Some(Duration::from_secs(30))))
        );
        assert!(matches!(
            classify(answer(402, Value::Null)),
            Err(ApiError::Billing(_))
        ));
        assert!(matches!(
            classify(answer(
                403,
                json!({ "error": "Account suspended: billing limit reached" })
            )),
            Err(ApiError::Billing(_))
        ));
        assert_eq!(
            classify(answer(
                403,
                json!({ "error": "invalid_scope", "error_description": "missing vehicle_location" })
            )),
            Err(ApiError::Forbidden("missing vehicle_location".into()))
        );
        assert_eq!(
            classify(answer(421, Value::Null)),
            Err(ApiError::WrongRegion)
        );
        assert_eq!(
            classify(answer(503, Value::Null)),
            Err(ApiError::Server(503))
        );
    }

    #[test]
    fn a_car_gets_three_wakes_a_minute_and_then_waits() {
        let mut limits = Limits::default();
        let start = Instant::now();
        for _ in 0..3 {
            assert_eq!(limits.take("VIN1", Category::Wake, start), None);
        }
        let wait = limits
            .take("VIN1", Category::Wake, start + Duration::from_secs(10))
            .expect("full");
        assert_eq!(wait, Duration::from_secs(50));
        assert_eq!(
            limits.take("VIN2", Category::Wake, start),
            None,
            "another car has its own"
        );
        assert_eq!(
            limits.take("VIN1", Category::Wake, start + Duration::from_secs(60)),
            None,
            "a minute later there's room"
        );
        for _ in 0..1000 {
            assert_eq!(limits.take("VIN1", Category::Free, start), None);
        }
    }
}
