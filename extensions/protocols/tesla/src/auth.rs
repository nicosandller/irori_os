//! Signing in to Tesla, and staying signed in.
//!
//! Tesla's refresh tokens work once: refreshing hands back a new one and retires the old. Home
//! Assistant lost sign-ins when two refreshes raced or the new token wasn't saved before a
//! restart. Here one lock covers the whole refresh, a caller that waited on it uses the token the
//! first one got instead of spending the refresh token again, and the new tokens are saved to the
//! extension's store before anyone uses them.

use std::collections::BTreeMap;
use std::sync::Arc;

use irori_cloud::Secret;
use irori_cloud::reqwest::{self, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::api::{self, ACCOUNT, ApiError, Ask, Clock, Hosts, Meter, Region};
use crate::settings::Settings;
use crate::usage::Why;
use crate::usage::price::Endpoint;

/// Where a person signs in.
pub const AUTHORIZE: &str = "https://auth.tesla.com/oauth2/v3/authorize";

/// What Irori asks to be allowed: reading and commanding cars (charging included), their
/// location, and reading and commanding energy sites.
pub const SCOPES: &str = "openid offline_access vehicle_device_data vehicle_location \
                          vehicle_cmds vehicle_charging_cmds energy_device_data energy_cmds";

/// The store key the tokens are kept under.
pub const STORED: &str = "tokens";

/// A token is refreshed this long (seconds) before it runs out.
const EARLY: u64 = 300;

/// The person's tokens.
#[derive(Clone)]
pub struct Tokens {
    pub access: Secret,
    pub refresh: Secret,
    pub expires_at: u64,
    pub region: Region,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("expires_at", &self.expires_at)
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

/// The tokens as the store keeps them.
#[derive(Clone, Serialize, Deserialize)]
pub struct Kept {
    access: String,
    refresh: String,
    expires_at: u64,
    region: Region,
}

impl From<&Tokens> for Kept {
    fn from(tokens: &Tokens) -> Self {
        Self {
            access: tokens.access.expose().to_owned(),
            refresh: tokens.refresh.expose().to_owned(),
            expires_at: tokens.expires_at,
            region: tokens.region,
        }
    }
}

impl From<Kept> for Tokens {
    fn from(kept: Kept) -> Self {
        Self {
            access: Secret::from(kept.access),
            refresh: Secret::from(kept.refresh),
            expires_at: kept.expires_at,
            region: kept.region,
        }
    }
}

/// New tokens for the loop to save. It answers whether it did.
pub struct Keep {
    pub tokens: Kept,
    pub done: oneshot::Sender<bool>,
}

/// The sign-in, shared by every call.
#[derive(Clone)]
pub struct Session {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    hosts: Hosts,
    client_id: String,
    client_secret: Secret,
    redirect_uri: String,
    tokens: Mutex<Option<Tokens>>,
    /// The app's own tokens, for registering, by region, with when they run out.
    partner: Mutex<BTreeMap<Region, (Secret, u64)>>,
    keep: mpsc::UnboundedSender<Keep>,
    now: Clock,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("client_id", &self.inner.client_id)
            .finish_non_exhaustive()
    }
}

/// What Tesla's token endpoint answers.
#[derive(Deserialize)]
struct Granted {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: u64,
}

impl Session {
    pub fn new(
        http: reqwest::Client,
        hosts: Hosts,
        settings: &Settings,
        kept: Option<Kept>,
        keep: mpsc::UnboundedSender<Keep>,
        now: Clock,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                http,
                hosts,
                client_id: settings.client_id.clone(),
                client_secret: settings.client_secret.clone(),
                redirect_uri: settings.redirect_uri(),
                tokens: Mutex::new(kept.map(Tokens::from)),
                partner: Mutex::new(BTreeMap::new()),
                keep,
                now,
            }),
        }
    }

    pub async fn signed_in(&self) -> bool {
        self.inner.tokens.lock().await.is_some()
    }

    #[cfg(test)]
    pub async fn region(&self) -> Option<Region> {
        self.inner.tokens.lock().await.as_ref().map(|t| t.region)
    }

    /// Where to send the person to sign in. `state` comes back on the address they land on.
    pub fn authorize_url(&self, state: &str) -> String {
        Url::parse_with_params(
            AUTHORIZE,
            [
                ("response_type", "code"),
                ("client_id", self.inner.client_id.as_str()),
                ("redirect_uri", self.inner.redirect_uri.as_str()),
                ("scope", SCOPES),
                ("state", state),
                ("prompt_missing_scopes", "true"),
            ],
        )
        .map(String::from)
        .unwrap_or_default()
    }

    /// A token to call the person's account with, refreshed first if it's about to run out.
    pub async fn access(&self, meter: &Meter) -> Result<(Secret, Region), ApiError> {
        let mut tokens = self.inner.tokens.lock().await;
        let Some(current) = tokens.as_ref() else {
            return Err(ApiError::SignedOut("not signed in yet".into()));
        };
        if current.expires_at > (self.inner.now)() + EARLY {
            return Ok((current.access.clone(), current.region));
        }
        let fresh = self.refresh(current, meter).await;
        self.settle(&mut tokens, fresh).await
    }

    /// After Tesla refused `stale`: a new token, unless another call got one meanwhile.
    pub async fn refresh_after(&self, stale: &Secret, meter: &Meter) -> Result<(), ApiError> {
        let mut tokens = self.inner.tokens.lock().await;
        let Some(current) = tokens.as_ref() else {
            return Err(ApiError::SignedOut("not signed in yet".into()));
        };
        if current.access != *stale {
            return Ok(());
        }
        let fresh = self.refresh(current, meter).await;
        self.settle(&mut tokens, fresh).await.map(drop)
    }

    /// The account turned out to be on the other server.
    pub async fn move_to(&self, region: Region) {
        let mut tokens = self.inner.tokens.lock().await;
        if let Some(current) = tokens.as_mut() {
            current.region = region;
            let kept = Kept::from(&*current);
            self.keep(kept).await;
        }
    }

    async fn settle(
        &self,
        tokens: &mut Option<Tokens>,
        fresh: Result<Tokens, ApiError>,
    ) -> Result<(Secret, Region), ApiError> {
        match fresh {
            Ok(fresh) => {
                // Saved before it's used: the old refresh token is spent.
                self.keep(Kept::from(&fresh)).await;
                let answer = (fresh.access.clone(), fresh.region);
                *tokens = Some(fresh);
                Ok(answer)
            }
            Err(error @ ApiError::SignedOut(_)) => {
                *tokens = None;
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    async fn keep(&self, tokens: Kept) {
        let (done, saved) = oneshot::channel();
        let sent = self.inner.keep.send(Keep { tokens, done });
        if sent.is_err() || saved.await != Ok(true) {
            // Still used: dropping them would sign the person out at the next refresh.
            tracing::warn!("couldn't save Tesla's new sign-in; it's kept until Irori restarts");
        }
    }

    async fn refresh(&self, current: &Tokens, meter: &Meter) -> Result<Tokens, ApiError> {
        let form = [
            ("grant_type", "refresh_token"),
            ("client_id", self.inner.client_id.as_str()),
            ("refresh_token", current.refresh.expose()),
        ];
        let granted = self.token(&form, meter).await?;
        Ok(Tokens {
            region: Region::from_token(&granted.access_token).unwrap_or(current.region),
            refresh: granted
                .refresh_token
                .map_or_else(|| current.refresh.clone(), Secret::from),
            access: Secret::from(granted.access_token),
            expires_at: (self.inner.now)() + granted.expires_in,
        })
    }

    /// Trades the code from the address the person landed on for their tokens.
    pub async fn exchange(&self, code: &str, meter: &Meter) -> Result<Region, ApiError> {
        let region = Region::from_code(code).unwrap_or(Region::Na);
        let form = [
            ("grant_type", "authorization_code"),
            ("client_id", self.inner.client_id.as_str()),
            ("client_secret", self.inner.client_secret.expose()),
            ("code", code),
            ("audience", self.inner.hosts.of(region)),
            ("redirect_uri", self.inner.redirect_uri.as_str()),
        ];
        let granted = self.token(&form, meter).await?;
        let Some(refresh) = granted.refresh_token else {
            return Err(ApiError::SignedOut(
                "Tesla gave no refresh token: the app needs the offline_access scope".into(),
            ));
        };
        let tokens = Tokens {
            region: Region::from_token(&granted.access_token).unwrap_or(region),
            access: Secret::from(granted.access_token),
            refresh: Secret::from(refresh),
            expires_at: (self.inner.now)() + granted.expires_in,
        };
        let region = tokens.region;
        let mut held = self.inner.tokens.lock().await;
        self.keep(Kept::from(&tokens)).await;
        *held = Some(tokens);
        Ok(region)
    }

    /// The app's own token in a region, for registering it.
    pub async fn partner_token(&self, region: Region, meter: &Meter) -> Result<Secret, ApiError> {
        let mut partner = self.inner.partner.lock().await;
        if let Some((token, until)) = partner.get(&region)
            && *until > (self.inner.now)() + EARLY
        {
            return Ok(token.clone());
        }
        let form = [
            ("grant_type", "client_credentials"),
            ("client_id", self.inner.client_id.as_str()),
            ("client_secret", self.inner.client_secret.expose()),
            ("audience", self.inner.hosts.of(region)),
            ("scope", "openid"),
        ];
        let granted = self
            .token(&form, meter)
            .await
            .map_err(|error| match error {
                ApiError::SignedOut(why) => ApiError::Refused {
                    status: 401,
                    message: if why.is_empty() {
                        "Tesla refused the Client ID and secret".into()
                    } else {
                        format!("Tesla refused the Client ID and secret: {why}")
                    },
                },
                other => other,
            })?;
        let token = Secret::from(granted.access_token);
        partner.insert(
            region,
            (token.clone(), (self.inner.now)() + granted.expires_in),
        );
        Ok(token)
    }

    async fn token(&self, form: &[(&str, &str)], meter: &Meter) -> Result<Granted, ApiError> {
        let request = self
            .inner
            .http
            .post(self.inner.hosts.token_url())
            .form(form);
        let ask = Ask::new(ACCOUNT, Endpoint::Token, Why::Setup);
        let answer = api::send(request, &ask, meter, self.inner.now).await?;
        match answer.status {
            200..=299 => serde_json::from_value(answer.body).map_err(|_| {
                ApiError::Unreachable("Tesla's sign-in answer wasn't readable".into())
            }),
            400 | 401 | 403 => Err(ApiError::SignedOut(api::words(&answer.body))),
            _ => api::classify(answer).and_then(|_: Value| {
                Err(ApiError::Unreachable(
                    "Tesla's sign-in answer wasn't readable".into(),
                ))
            }),
        }
    }
}

/// The code in the address the person landed on after signing in, checked against the `state`
/// Irori sent them with.
pub fn parse_landed(address: &str, state: &str) -> Result<String, String> {
    let address = address.trim();
    let url = Url::parse(address).map_err(|_| {
        "paste the whole address from the browser's address bar, starting with https://".to_owned()
    })?;
    let param = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    if let Some(error) = param("error") {
        let why = param("error_description").unwrap_or(error);
        return Err(format!("Tesla didn't sign you in: {why}"));
    }
    if param("state").as_deref() != Some(state) {
        return Err(
            "that address is from an older sign-in: open the sign-in link again and paste the \
             address you land on"
                .to_owned(),
        );
    }
    param("code")
        .filter(|code| !code.is_empty())
        .ok_or_else(|| "that address has no sign-in code in it".to_owned())
}

/// A random `state` for a sign-in.
pub fn new_state() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_landed_address_gives_its_code_only_for_this_sign_in() {
        let landed =
            "https://tesla.example.com/irori/tesla?locale=en-US&code=NA_abc&state=s1&issuer=x";
        assert_eq!(parse_landed(landed, "s1"), Ok("NA_abc".into()));
        assert!(
            parse_landed(landed, "s2")
                .expect_err("old")
                .contains("older sign-in")
        );
        assert!(
            parse_landed("NA_abc", "s1")
                .expect_err("bare")
                .contains("whole address")
        );
        let denied = "https://tesla.example.com/irori/tesla?error=access_denied&error_description=The+user+denied&state=s1";
        assert!(
            parse_landed(denied, "s1")
                .expect_err("denied")
                .contains("The user denied")
        );
    }

    #[test]
    fn the_sign_in_link_asks_for_every_scope_and_carries_the_state() {
        let (keep, _) = mpsc::unbounded_channel();
        let session = Session::new(
            reqwest::Client::new(),
            Hosts::tesla(),
            &crate::settings::for_tests(),
            None,
            keep,
            || 0,
        );
        let url = Url::parse(&session.authorize_url("s1")).expect("a url");
        let pairs: BTreeMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs["state"], "s1");
        assert_eq!(pairs["client_id"], "the-client");
        assert_eq!(
            pairs["redirect_uri"],
            "https://tesla.example.com/irori/tesla"
        );
        assert!(pairs["scope"].contains("offline_access"));
        assert!(pairs["scope"].contains("energy_cmds"));
        assert_eq!(new_state().len(), 32);
        assert_ne!(new_state(), new_state());
    }
}
