//! The extension against a fake Tesla on a local port.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;

use crate::api::{Api, ApiError, Meter, Region};
use crate::auth::{Keep, Kept, Session};
use crate::fake::{Answer, Fake};
use crate::usage::Why;
use crate::usage::price::Category;
use crate::usage::tests::{NOV_10, usage};

const VIN: &str = "5YJ3E1EA7KF000001";

fn nov_10() -> u64 {
    NOV_10
}

/// An account signed in to the fake, and every save of new tokens it asks for.
struct Account {
    fake: Fake,
    api: Api,
    meter: Meter,
    kept: Arc<Mutex<Vec<Kept>>>,
}

async fn account(tokens: Option<Kept>) -> Account {
    let fake = Fake::start().await;
    let tokens = tokens.or_else(|| Some(fake.signed_in()));
    let (keep, mut to_keep) = mpsc::unbounded_channel::<Keep>();
    let kept = Arc::new(Mutex::new(Vec::new()));
    let saved = Arc::clone(&kept);
    tokio::spawn(async move {
        while let Some(Keep { tokens, done }) = to_keep.recv().await {
            saved.lock().expect("kept").push(tokens);
            let _ = done.send(true);
        }
    });
    let http = irori_cloud::client("irori-tesla-test").expect("a client");
    let session = Session::new(
        http.clone(),
        fake.hosts(),
        &crate::settings::for_tests(),
        tokens,
        keep,
        nov_10,
    );
    let meter: Meter = Arc::new(Mutex::new(usage()));
    let api = Api::new(http, fake.hosts(), session, Arc::clone(&meter), nov_10);
    Account {
        fake,
        api,
        meter,
        kept,
    }
}

fn expired(fake: &Fake) -> Kept {
    let mut kept = serde_json::to_value(fake.signed_in()).expect("json");
    kept["expires_at"] = json!(0);
    serde_json::from_value(kept).expect("kept")
}

#[tokio::test]
async fn the_meter_counts_every_answer_tesla_bills_and_nothing_else() {
    let account = account(None).await;
    let route = format!("GET /na/api/1/vehicles/{VIN}/vehicle_data");
    account.fake.answer(
        &route,
        vec![
            Answer::ok(json!({ "vin": VIN })),
            Answer::status(408),
            Answer::status(429).header("retry-after", "30"),
            Answer::with(402, json!({ "error": "payment required" })),
            Answer::with(403, json!({ "error": "invalid_scope" })),
            Answer::status(500),
            Answer::hang_up(),
        ],
    );
    let mut errors = Vec::new();
    for _ in 0..7 {
        if let Err(error) = account.api.vehicle_data(VIN, false, Why::Schedule).await {
            errors.push(error);
        }
    }
    assert_eq!(errors[0], ApiError::Asleep);
    assert_eq!(
        errors[1],
        ApiError::RateLimited(Some(Duration::from_secs(30)))
    );
    assert!(matches!(errors[2], ApiError::Billing(_)));
    assert!(matches!(errors[3], ApiError::Forbidden(_)));
    assert_eq!(errors[4], ApiError::Server(500));
    assert!(matches!(errors[5], ApiError::Unreachable(_)));
    let usage = account.meter.lock().expect("meter");
    let total = usage.month.total();
    assert_eq!(total.data, 5, "200, 408, 429, 402 and 403 are billed");
    assert_eq!((total.command, total.wake), (0, 0));
    assert_eq!(usage.calls.len(), 5);
    assert_eq!(
        usage
            .calls
            .iter()
            .map(|call| call.status)
            .collect::<Vec<_>>(),
        [200, 408, 429, 402, 403]
    );
    assert!(usage.calls.iter().all(|call| call.target == VIN));
    assert_eq!(usage.spent(), 5 * 2_000, "five data requests at $0.002");
}

#[tokio::test]
async fn vehicle_data_asks_for_its_parts_and_location_only_when_wanted() {
    let account = account(None).await;
    let route = format!("GET /na/api/1/vehicles/{VIN}/vehicle_data");
    account.fake.answer(&route, vec![Answer::ok(json!({}))]);
    account
        .api
        .vehicle_data(VIN, false, Why::Schedule)
        .await
        .expect("read");
    account
        .api
        .vehicle_data(VIN, true, Why::Refresh)
        .await
        .expect("read");
    let sent = account.fake.sent("/vehicle_data");
    let parts = |at: usize| sent[at].query["endpoints"].clone();
    assert!(parts(0).contains("charge_state;climate_state"));
    assert!(!parts(0).contains("location_data"));
    assert!(parts(1).ends_with(";location_data"));
    assert!(sent[0].headers["authorization"].starts_with("Bearer "));
}

#[tokio::test]
async fn nothing_cached_is_not_a_missing_car() {
    let account = account(None).await;
    let route = format!("GET /na/api/1/vehicles/{VIN}/vehicle_data_cached");
    account.fake.answer(
        &route,
        vec![
            Answer::status(404),
            Answer::status(412),
            Answer::ok(json!({ "charge_state": {} })),
        ],
    );
    for expected in [None, None, Some(json!({ "charge_state": {} }))] {
        let cached = account
            .api
            .vehicle_data_cached(VIN, Why::Start)
            .await
            .expect("an answer");
        assert_eq!(cached, expected);
    }
    let total = account.meter.lock().expect("meter").month.total();
    assert_eq!(
        total.data, 3,
        "each is billed as data until Tesla's docs say otherwise"
    );
}

#[tokio::test]
async fn two_calls_with_an_old_token_refresh_it_once_and_save_it_first() {
    let fake = Fake::start().await;
    let (keep, mut to_keep) = mpsc::unbounded_channel::<Keep>();
    let kept = Arc::new(Mutex::new(Vec::<Kept>::new()));
    let saved = Arc::clone(&kept);
    tokio::spawn(async move {
        while let Some(Keep { tokens, done }) = to_keep.recv().await {
            // Slow to save, so the second call is surely waiting on the lock meanwhile.
            tokio::time::sleep(Duration::from_millis(50)).await;
            saved.lock().expect("kept").push(tokens);
            let _ = done.send(true);
        }
    });
    let http = irori_cloud::client("irori-tesla-test").expect("a client");
    let session = Session::new(
        http.clone(),
        fake.hosts(),
        &crate::settings::for_tests(),
        Some(expired(&fake)),
        keep,
        nov_10,
    );
    let meter: Meter = Arc::new(Mutex::new(usage()));
    let api = Api::new(http, fake.hosts(), session, Arc::clone(&meter), nov_10);
    fake.answer("GET /na/api/1/vehicles", vec![Answer::ok(json!([]))]);

    let (first, second) = tokio::join!(api.vehicles(Why::Schedule), api.vehicles(Why::Schedule));
    first.expect("the first call works");
    second.expect("so does the second");
    let refreshes: Vec<_> = fake
        .sent("/oauth2/v3/token")
        .into_iter()
        .filter(|request| request.body["grant_type"] == "refresh_token")
        .collect();
    assert_eq!(refreshes.len(), 1, "one refresh for both");
    assert_eq!(
        kept.lock().expect("kept").len(),
        1,
        "saved once, before use"
    );
    let used: Vec<String> = fake
        .sent("/api/1/vehicles")
        .iter()
        .map(|request| request.headers["authorization"].clone())
        .collect();
    assert_eq!(used.len(), 2);
    assert_eq!(used[0], used[1], "both used the new token");
    assert!(
        !refreshes[0]
            .body
            .as_object()
            .expect("a form")
            .contains_key("client_secret"),
        "a refresh doesn't send the client secret"
    );
}

#[tokio::test]
async fn a_token_tesla_refuses_is_refreshed_and_the_call_tried_again() {
    let account = account(None).await;
    account.fake.answer(
        "GET /na/api/1/vehicles",
        vec![
            Answer::status(401),
            Answer::ok(json!([{ "vin": VIN, "state": "asleep" }])),
        ],
    );
    let cars = account.api.vehicles(Why::Schedule).await.expect("read");
    assert_eq!(cars[0].state, "asleep");
    assert_eq!(account.kept.lock().expect("kept").len(), 1);
    let total = account.meter.lock().expect("meter").month.total();
    assert_eq!(total.data, 2, "the refused call is billed too");
    assert_eq!(total.free, 1, "the refresh isn't");
}

#[tokio::test]
async fn a_spent_refresh_token_signs_out_rather_than_looping() {
    let fake = Fake::start().await;
    let mut tokens = serde_json::to_value(expired(&fake)).expect("json");
    tokens["refresh"] = json!("already-used");
    let account = account(Some(serde_json::from_value(tokens).expect("kept"))).await;
    let error = account
        .api
        .vehicles(Why::Schedule)
        .await
        .expect_err("signed out");
    assert!(matches!(error, ApiError::SignedOut(ref why) if why.contains("already been used")));
    assert!(!account.api.session().signed_in().await);
    assert!(account.fake.sent("/api/1/vehicles").is_empty());
}

#[tokio::test]
async fn an_account_on_the_other_server_is_followed_there() {
    let account = account(None).await;
    account
        .fake
        .answer("GET /na/api/1/vehicles", vec![Answer::status(421)]);
    account
        .fake
        .answer("GET /eu/api/1/vehicles", vec![Answer::ok(json!([]))]);
    account.api.vehicles(Why::Schedule).await.expect("read");
    assert_eq!(account.api.session().region().await, Some(Region::Eu));
    assert_eq!(account.kept.lock().expect("kept").len(), 1, "remembered");
}

#[tokio::test]
async fn signing_in_reads_the_region_from_the_code_and_saves_the_tokens() {
    let fake = Fake::start().await;
    fake.with(|state| state.region = "EU".into());
    let (keep, mut to_keep) = mpsc::unbounded_channel::<Keep>();
    tokio::spawn(async move {
        while let Some(Keep { done, .. }) = to_keep.recv().await {
            let _ = done.send(true);
        }
    });
    let http = irori_cloud::client("irori-tesla-test").expect("a client");
    let session = Session::new(
        http,
        fake.hosts(),
        &crate::settings::for_tests(),
        None,
        keep,
        nov_10,
    );
    let meter: Meter = Arc::new(Mutex::new(usage()));
    let region = session.exchange("EU_abc", &meter).await.expect("signed in");
    assert_eq!(region, Region::Eu);
    assert!(session.signed_in().await);
    let sent = fake.sent("/oauth2/v3/token");
    let form = &sent[0].body;
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["code"], "EU_abc");
    assert_eq!(form["audience"], fake.hosts().eu);
    assert_eq!(
        form["redirect_uri"],
        "https://tesla.example.com/irori/tesla"
    );
    assert_eq!(form["client_secret"], "the-secret");
    assert_eq!(meter.lock().expect("meter").month.total().free, 1);
}

#[tokio::test]
async fn registering_uses_the_apps_own_token_in_each_region() {
    let account = account(None).await;
    for region in Region::ALL {
        account
            .api
            .register(region, "tesla.example.com")
            .await
            .expect("registered");
    }
    let posts = account.fake.sent("/api/1/partner_accounts");
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0].body["domain"], "tesla.example.com");
    assert!(posts[0].headers["authorization"].contains("partner-"));
    assert!(posts[0].path.starts_with("/na/") && posts[1].path.starts_with("/eu/"));
    assert_eq!(
        account
            .api
            .registered_key(Region::Na, "tesla.example.com")
            .await
            .expect("an answer"),
        Some("registered".into())
    );
    let usage = account.meter.lock().expect("meter");
    let total = usage.month.total();
    assert_eq!(
        total.data, 3,
        "registering is billed as data until confirmed otherwise"
    );
    assert_eq!(total.free, 2, "the app's token, once per region");
    assert!(
        usage
            .calls
            .iter()
            .all(|call| call.endpoint.category().0 == Category::Data)
    );
}

// The whole protocol, through the host's end of its context.

mod protocol {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use irori_cloud::Secret;
    use irori_protocol::Filled;
    use irori_protocol::host;
    use irori_protocol::{Health, ProtocolError};
    use irori_types::Waiting;
    use p256::pkcs8::{EncodePrivateKey, LineEnding};
    use serde_json::{Value, json};
    use tokio::sync::{mpsc, watch};

    use super::{NOV_10, VIN, nov_10};
    use crate::fake::{Answer, Fake};
    use crate::keys::Key;
    use crate::settings::Settings;
    use crate::{Timing, drive};

    #[derive(Debug, Default)]
    pub struct Log {
        pub health: Vec<Health>,
        pub waiting: Vec<Waiting>,
        pub actions: Vec<String>,
        pub stored: BTreeMap<String, Value>,
        pub devices: BTreeMap<String, irori_types::DeviceDescription>,
        pub entities: BTreeMap<String, irori_types::EntityDescription>,
        /// The last state reported for each entity, as JSON, and whether it was replayed.
        pub states: BTreeMap<String, (Value, bool)>,
        /// The call the last report for each entity credited, if any.
        pub caused: BTreeMap<String, Option<String>>,
        /// The map as last drawn.
        pub networks: Vec<irori_types::Network>,
        pub removed: Vec<String>,
    }

    impl Log {
        pub fn state(&self, id: &str) -> Option<Value> {
            self.states.get(id).map(|(state, _)| state.clone())
        }
    }

    pub struct Home {
        pub fake: Fake,
        pub log: Arc<Mutex<Log>>,
        actions: mpsc::Sender<irori_protocol::IncomingAction>,
        /// Kept open: the protocol stops when the core's end of its calls closes.
        calls: mpsc::Sender<irori_protocol::IncomingCall>,
        _stop: watch::Sender<bool>,
        _drive: tokio::task::JoinHandle<Result<(), ProtocolError>>,
    }

    pub fn pasted(key: &Key) -> Settings {
        let mut settings = crate::settings::for_tests();
        let pem = key
            .secret()
            .to_pkcs8_pem(LineEnding::LF)
            .expect("pem")
            .to_string();
        settings.private_key = Some(Secret::from(pem));
        settings
    }

    pub async fn home(fake: Fake, settings: Settings, stored: BTreeMap<String, Value>) -> Home {
        let (ctx, host) = host::connect();
        let log = Arc::new(Mutex::new(Log {
            stored: stored.clone(),
            ..Log::default()
        }));
        let recorded = Arc::clone(&log);
        let mut ops = host.ops;
        tokio::spawn(async move {
            while let Some(op) = ops.recv().await {
                let mut log = recorded.lock().expect("log");
                match op {
                    host::Op::DescribeDevice(device, reply) => {
                        log.devices.insert(device.unique_id.to_string(), device);
                        let _ = reply.send(Ok(()));
                    }
                    host::Op::DescribeEntity(entity, reply) => {
                        log.entities.insert(entity.unique_id.to_string(), entity);
                        let _ = reply.send(Ok(()));
                    }
                    host::Op::RemoveDevice(id, reply) | host::Op::RemoveEntity(id, reply) => {
                        log.removed.push(id.to_string());
                        let _ = reply.send(Ok(()));
                    }
                    host::Op::SetAvailability(_, _, reply) => {
                        let _ = reply.send(Ok(()));
                    }
                    host::Op::SetHealth(health) => log.health.push(health),
                    host::Op::SetWaiting(waiting) => log.waiting = waiting,
                    host::Op::SetAvailableActions(actions) => log.actions = actions,
                    host::Op::SetNetworks(networks) => log.networks = networks,
                    host::Op::SetUnmodeled(_) | host::Op::SetActionOpen(_, _) => {}
                    host::Op::Load(key, reply) => {
                        let _ = reply.send(Ok(log.stored.get(&key).cloned()));
                    }
                    host::Op::Store(key, value, reply) => {
                        match value {
                            Some(value) => log.stored.insert(key, value),
                            None => log.stored.remove(&key),
                        };
                        let _ = reply.send(Ok(()));
                    }
                }
            }
        });
        let reports = host.reports;
        let reported = Arc::clone(&log);
        tokio::spawn(async move {
            loop {
                reports.ready().await;
                let mut log = reported.lock().expect("log");
                for report in reports.drain() {
                    let state = serde_json::to_value(&report.state).expect("a state");
                    log.caused.insert(
                        report.unique_id.to_string(),
                        report.caused_by.as_ref().map(ToString::to_string),
                    );
                    log.states
                        .insert(report.unique_id.to_string(), (state, report.replayed));
                }
            }
        });
        let timing = Timing {
            hosts: fake.hosts(),
            now: nov_10,
            relist: Duration::from_secs(3600),
            save: Duration::from_millis(50),
            idle: Duration::from_secs(3600),
            woken: Duration::from_millis(50),
            patience: crate::sending::Patience {
                deadline: Duration::from_secs(3),
                retry: Duration::from_millis(30),
                busy: Duration::from_millis(10),
            },
        };
        let drive = tokio::spawn(drive(settings, ctx, timing));
        Home {
            fake,
            log,
            actions: host.actions,
            calls: host.calls,
            _stop: host.stop,
            _drive: drive,
        }
    }

    impl Home {
        pub async fn until<T>(&self, what: &str, mut check: impl FnMut(&Log) -> Option<T>) -> T {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(found) = check(&self.log.lock().expect("log")) {
                    return found;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "never saw {what}: {:?}",
                    self.log.lock().expect("log")
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        pub async fn act(
            &self,
            action: &str,
            way: Option<(&str, &str, &str)>,
        ) -> Result<(), String> {
            let filled = Filled {
                way: way.map(|(way, _, _)| way.to_owned()),
                fields: way
                    .map(|(_, field, value)| BTreeMap::from([(field.to_owned(), value.to_owned())]))
                    .unwrap_or_default(),
            };
            let (incoming, answer) = host::incoming_action_with(action.to_owned(), filled);
            self.actions.send(incoming).await.expect("running");
            tokio::time::timeout(Duration::from_secs(5), answer)
                .await
                .expect("an answer in time")
                .expect("answered")
        }

        /// A service call from a person, and its answer.
        pub async fn call(
            &self,
            entity: &str,
            service: irori_types::Service,
        ) -> Result<(), irori_protocol::ServiceError> {
            use irori_types::{Context, ContextId, Origin, ServiceCall, UniqueId, UserId};
            let (incoming, answer) = host::incoming_call(ServiceCall {
                unique_id: UniqueId::try_from(entity).expect("an entity id"),
                service,
                context: Context {
                    id: ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9").expect("valid"),
                    parent_id: None,
                    origin: Origin::User {
                        user_id: UserId::try_from("nico").expect("valid"),
                    },
                },
            });
            self.calls.send(incoming).await.expect("running");
            tokio::time::timeout(Duration::from_secs(5), answer)
                .await
                .expect("an answer in time")
                .expect("answered")
        }

        /// A question from the usage page, asked as the home's owner or as another person.
        pub async fn page(
            &self,
            method: &str,
            params: serde_json::Value,
            owner: bool,
        ) -> Result<serde_json::Value, String> {
            let caller = irori_protocol::engine::Caller { user: None, owner };
            let (incoming, answer) = host::incoming_page(method.to_owned(), params, caller);
            self.actions.send(incoming).await.expect("running");
            tokio::time::timeout(Duration::from_secs(5), answer)
                .await
                .expect("an answer in time")
                .expect("answered")
        }

        pub fn step(&self, starting: &str) -> Option<Waiting> {
            self.log
                .lock()
                .expect("log")
                .waiting
                .iter()
                .find(|step| step.reason.starts_with(starting))
                .cloned()
        }
    }

    #[tokio::test]
    async fn a_new_setup_asks_to_sign_in_and_registers_the_pasted_key() {
        let key = Key::generate().expect("a key");
        let fake = Fake::start().await;
        let hex = key.public_hex();
        fake.with(|state| {
            state.registered.insert("na".into(), hex.clone());
            state.registered.insert("eu".into(), hex.clone());
        });
        let home = home(fake, pasted(&key), BTreeMap::new()).await;
        home.until("the key registered", |log| {
            log.stored
                .get("registered")
                .filter(|value| **value == json!(hex))
                .map(drop)
        })
        .await;
        let step = home
            .until("the sign-in step", |log| {
                log.waiting
                    .iter()
                    .find(|step| step.reason.starts_with("Sign in to Tesla"))
                    .cloned()
            })
            .await;
        let url = step.link.expect("a link").url;
        let state = home
            .until("the sign-in state kept", |log| {
                log.stored
                    .get("sign_in_state")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .await;
        assert!(url.contains(&format!("state={state}")));
        home.until("the actions", |log| {
            (log.actions == ["sign_in", "update_spend"]).then_some(())
        })
        .await;
        assert!(home.step("Put Irori's public key").is_none());
        assert!(
            !home
                .log
                .lock()
                .expect("log")
                .stored
                .contains_key("private_key"),
            "a pasted key isn't copied into the store"
        );
    }

    #[tokio::test]
    async fn a_key_tesla_doesnt_have_is_shown_to_copy_onto_the_website() {
        let fake = Fake::start().await;
        let home = home(fake, crate::settings::for_tests(), BTreeMap::new()).await;
        let step = home
            .until("the key step", |log| {
                log.waiting
                    .iter()
                    .find(|step| step.reason.starts_with("Put Irori's public key"))
                    .cloned()
            })
            .await;
        assert!(
            home.step("Sign in to Tesla").is_none(),
            "signing in waits for the key"
        );
        let kept = home
            .until("Irori's own key kept", |log| {
                log.stored
                    .get("private_key")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .await;
        let key = Key::from_stored(&kept).expect("a key");
        assert_eq!(step.copy.expect("a copy").text, key.public_pem());
        assert!(
            home.log
                .lock()
                .expect("log")
                .actions
                .contains(&"check_key".to_owned())
        );

        // The person puts it on the website; Tesla now has it.
        let hex = key.public_hex();
        home.fake.with(|state| {
            state.registered.insert("na".into(), hex.clone());
            state.registered.insert("eu".into(), hex.clone());
        });
        home.act("check_key", None).await.expect("it matches now");
        home.until("the key step gone", |log| {
            (!log
                .waiting
                .iter()
                .any(|step| step.reason.starts_with("Put Irori's public key")))
            .then_some(())
        })
        .await;
        home.until("signing in comes next", |log| {
            log.waiting
                .iter()
                .any(|step| step.reason.starts_with("Sign in to Tesla"))
                .then_some(())
        })
        .await;
    }

    #[tokio::test]
    async fn signing_in_lists_the_cars_and_asks_for_the_key_on_one_without_it() {
        let key = Key::generate().expect("a key");
        let fake = Fake::start().await;
        fake.answer(
            "GET /na/api/1/products",
            vec![Answer::ok(json!([
                { "vin": VIN, "display_name": "Red car", "state": "asleep" },
                { "energy_site_id": 12345, "site_name": "Home", "components": { "battery": true, "solar": true } },
                { "energy_site_id": 999, "components": { "wall_connectors": [{}] } },
            ]))],
        );
        fake.answer(
            "POST /na/api/1/vehicles/fleet_status",
            vec![Answer::ok(
                json!({ "key_paired_vins": [], "unpaired_vins": [VIN], "vehicle_info": {} }),
            )],
        );
        let home = home(fake, pasted(&key), BTreeMap::new()).await;
        let state = home
            .until("the sign-in state kept", |log| {
                log.stored
                    .get("sign_in_state")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .await;
        let stale = "https://tesla.example.com/irori/tesla?code=NA_abc&state=old";
        assert!(
            home.act("sign_in", Some(("landed", "address", stale)))
                .await
                .expect_err("an old sign-in")
                .contains("older sign-in")
        );
        let landed = format!("https://tesla.example.com/irori/tesla?code=NA_abc&state={state}");
        home.act("sign_in", Some(("landed", "address", &landed)))
            .await
            .expect("signed in");
        let step = home
            .until("the car's key step", |log| {
                log.waiting
                    .iter()
                    .find(|step| step.unique_id.as_str() == VIN)
                    .cloned()
            })
            .await;
        assert_eq!(step.name.as_str(), "Red car");
        assert!(home.step("Sign in to Tesla").is_none());
        let products = home
            .until("the products kept", |log| {
                log.stored.get("products").cloned()
            })
            .await;
        assert_eq!(
            products["sites"].as_array().map(Vec::len),
            Some(1),
            "no Wall Connector"
        );
        assert!(home.log.lock().expect("log").stored.contains_key("tokens"));
        home.until("sign_in no longer offered", |log| {
            (!log.actions.contains(&"sign_in".to_owned())).then_some(())
        })
        .await;
    }

    #[tokio::test]
    async fn a_suspended_account_says_billing_not_sign_in() {
        let key = Key::generate().expect("a key");
        let fake = Fake::start().await;
        let tokens = serde_json::to_value(fake.signed_in()).expect("json");
        fake.answer(
            "GET /na/api/1/products",
            vec![Answer::with(402, json!({ "error": "Payment required" }))],
        );
        let home = home(
            fake,
            pasted(&key),
            BTreeMap::from([("tokens".to_owned(), tokens)]),
        )
        .await;
        home.until("billing health", |log| match log.health.last() {
            Some(Health::Degraded(why)) if why.contains("billing limit") => Some(()),
            _ => None,
        })
        .await;
        let health = home.log.lock().expect("log").health.last().cloned();
        let Some(Health::Degraded(why)) = health else {
            unreachable!()
        };
        assert!(why.contains("December 1"), "{why}");
        assert!(!why.contains("sign in"));
        assert!(home.step("Sign in to Tesla").is_none(), "still signed in");
    }

    #[tokio::test]
    async fn the_dashboard_figure_is_taken_and_kept() {
        let key = Key::generate().expect("a key");
        let fake = Fake::start().await;
        let home = home(fake, pasted(&key), BTreeMap::new()).await;
        home.act("update_spend", Some(("dashboard", "spent", "$3.42")))
            .await
            .expect("taken");
        let points = home
            .until("the point kept", |log| {
                log.stored.get("usage/dashboard").cloned()
            })
            .await;
        assert_eq!(points[0]["account"], 3_420_000);
        assert_eq!(points[0]["at"], NOV_10);
        assert!(
            home.act("update_spend", Some(("dashboard", "spent", "1.00")))
                .await
                .expect_err("lower")
                .contains("only goes up")
        );
    }
}

/// Reading cars and sites: when Tesla is asked, and what's shown from it.
mod reading {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use irori_protocol::Health;
    use serde_json::{Value, json};

    use super::VIN;
    use super::protocol::{Home, home, pasted};
    use crate::fake::{Answer, Fake};
    use crate::keys::Key;

    const OTHER: &str = "5YJ3E1EA7KF000002";
    pub const SITE: &str = "2252000000000001";

    pub fn answer(file: &str) -> Value {
        let text = match file {
            "vehicle_data" => include_str!("../testdata/vehicle_data.json"),
            "live_status" => include_str!("../testdata/live_status.json"),
            "site_info" => include_str!("../testdata/site_info.json"),
            _ => include_str!("../testdata/energy_history.json"),
        };
        serde_json::from_str(text).expect("json")
    }

    pub fn car(vin: &str, name: &str, state: &str) -> Value {
        json!({ "vin": vin, "display_name": name, "state": state })
    }

    pub fn site() -> Value {
        json!({ "energy_site_id": 2_252_000_000_000_001_u64, "site_name": "Home", "components": { "battery": true, "solar": true } })
    }

    /// A home signed in to a fake with these products, the key already registered.
    pub async fn signed_in(fake: Fake, products: Vec<Value>, stored: Vec<(&str, Value)>) -> Home {
        let key = Key::generate().expect("a key");
        let hex = key.public_hex();
        fake.with(|state| {
            state.registered.insert("na".into(), hex.clone());
            state.registered.insert("eu".into(), hex.clone());
        });
        fake.answer("GET /na/api/1/products", vec![Answer::ok(json!(products))]);
        let mut stored: BTreeMap<String, Value> = stored
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();
        stored.insert(
            "tokens".into(),
            serde_json::to_value(fake.signed_in()).expect("json"),
        );
        home(fake, pasted(&key), stored).await
    }

    fn vehicle_data_path(vin: &str) -> String {
        format!("GET /na/api/1/vehicles/{vin}/vehicle_data")
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    #[tokio::test]
    async fn a_sleeping_car_is_never_read_and_shows_what_tesla_last_heard() {
        let fake = Fake::start().await;
        let mut cached = answer("vehicle_data");
        cached["response"]["state"] = json!("asleep");
        cached["response"]
            .as_object_mut()
            .expect("an object")
            .remove("drive_state");
        fake.answer(
            &format!("GET /na/api/1/vehicles/{VIN}/vehicle_data_cached"),
            vec![Answer::with(200, cached)],
        );
        let sites = format!("/na/api/1/energy_sites/{SITE}");
        fake.answer(
            &format!("GET {sites}/live_status"),
            vec![Answer::with(200, answer("live_status"))],
        );
        fake.answer(
            &format!("GET {sites}/site_info"),
            vec![Answer::with(200, answer("site_info"))],
        );
        fake.answer(
            &format!("GET {sites}/calendar_history"),
            vec![Answer::with(200, answer("history"))],
        );
        let home = signed_in(fake, vec![car(VIN, "Red car", "asleep"), site()], vec![]).await;

        let battery = format!("{VIN}:battery");
        home.until("the battery from the cached data", |log| {
            log.state(&battery)
        })
        .await;
        let state = home
            .until("the car's state", |log| log.state(&format!("{VIN}:state")))
            .await;
        assert_eq!(state["value"], "asleep");
        home.until("the solar power", |log| {
            log.state(&format!("{SITE}:solar_power"))
        })
        .await;
        home.until("today's solar", |log| {
            log.state(&format!("{SITE}:solar_today"))
        })
        .await;
        settle().await;
        assert!(
            home.fake.sent("/vehicle_data").is_empty(),
            "a sleeping car isn't read"
        );
        assert_eq!(home.fake.sent("/vehicle_data_cached").len(), 1);
        assert!(
            home.fake.sent("/wake_up").is_empty(),
            "nothing wakes a car to read it"
        );
        let history = home.fake.sent("/calendar_history");
        assert!(
            !history[0].query.contains_key("start_date"),
            "the site's own day: {:?}",
            history[0].query
        );
        let log = home.log.lock().expect("log");
        let device = log.devices.get(VIN).expect("the car");
        assert_eq!(device.model.as_deref(), Some("Model 3"));
        assert!(log.entities.contains_key(&format!("{VIN}:location")));
        assert!(
            !log.states.contains_key(&format!("{VIN}:location")),
            "no position in the cached data"
        );
        assert!(log.stored.contains_key(&format!("car/{VIN}")));
    }

    #[tokio::test]
    async fn what_a_car_said_last_shows_at_start_without_asking_tesla() {
        let fake = Fake::start().await;
        let mut reading = crate::vehicle::CarReading::default();
        reading.merge(&answer("vehicle_data")["response"]);
        let products =
            json!({ "cars": [{ "vin": VIN, "name": "Red car", "state": "asleep" }], "sites": [] });
        // Not signed in: nothing can be asked.
        let home = home(
            fake,
            pasted(&Key::generate().expect("a key")),
            BTreeMap::from([
                ("products".to_owned(), products),
                (
                    format!("car/{VIN}"),
                    serde_json::to_value(&reading).expect("json"),
                ),
            ]),
        )
        .await;
        let (state, replayed) = home
            .until("the battery", |log| {
                log.states.get(&format!("{VIN}:battery")).cloned()
            })
            .await;
        assert_eq!(state["value"], 77.0);
        assert!(replayed, "what it said last, not something new");
        assert!(
            home.until("the position", |log| log.state(&format!("{VIN}:location")))
                .await["latitude"]
                .is_number()
        );
        settle().await;
        assert!(home.fake.sent("/vehicles").is_empty());
        assert!(home.fake.sent("/vehicle_data").is_empty());
    }

    #[tokio::test]
    async fn an_awake_car_is_read_once_then_left_to_fall_asleep() {
        let fake = Fake::start().await;
        fake.answer(
            &vehicle_data_path(VIN),
            vec![Answer::with(200, answer("vehicle_data"))],
        );
        let home = signed_in(fake, vec![car(VIN, "Red car", "online")], vec![]).await;
        home.until("the battery", |log| log.state(&format!("{VIN}:battery")))
            .await;
        let position = home
            .until("the position", |log| log.state(&format!("{VIN}:location")))
            .await;
        assert_eq!(position["latitude"], 35.6581);
        settle().await;
        let reads = home.fake.sent("/vehicle_data");
        assert_eq!(reads.len(), 1, "parked and idle: not read again");
        let parts = &reads[0].query["endpoints"];
        assert!(
            parts.contains("location_data") && parts.contains("charge_state"),
            "{parts}"
        );
    }

    #[tokio::test]
    async fn a_car_tesla_refuses_leaves_the_others_reading() {
        let fake = Fake::start().await;
        fake.answer(
            &vehicle_data_path(VIN),
            vec![Answer::with(
                403,
                json!({ "error": "Unauthorized: vehicle has no key for this app" }),
            )],
        );
        let mut blue = answer("vehicle_data");
        blue["response"]["vin"] = json!(OTHER);
        blue["response"]["charge_state"]["battery_level"] = json!(54);
        fake.answer(&vehicle_data_path(OTHER), vec![Answer::with(200, blue)]);
        let home = signed_in(
            fake,
            vec![
                car(VIN, "Red car", "online"),
                car(OTHER, "Blue car", "online"),
            ],
            vec![],
        )
        .await;
        let battery = home
            .until("the other car's battery", |log| {
                log.state(&format!("{OTHER}:battery"))
            })
            .await;
        assert_eq!(battery["value"], 54.0);
        let why = home
            .until("the refused car on its own line", |log| {
                match log.health.last() {
                    Some(Health::Degraded(why)) if why.contains("Red car") => Some(why.clone()),
                    _ => None,
                }
            })
            .await;
        assert!(
            why.starts_with("Tesla won't let Irori read Red car"),
            "{why}"
        );
        assert!(!why.contains("Blue car"));
    }

    #[tokio::test]
    async fn a_car_that_fell_asleep_is_billed_and_shown_asleep() {
        let fake = Fake::start().await;
        fake.answer(
            &vehicle_data_path(VIN),
            vec![Answer::with(408, json!({ "error": "vehicle unavailable" }))],
        );
        let home = signed_in(fake, vec![car(VIN, "Red car", "online")], vec![]).await;
        home.until("the car asleep", |log| {
            log.state(&format!("{VIN}:state"))
                .filter(|state| state["value"] == "asleep")
        })
        .await;
        let calls = home
            .until("the 408 on the meter", |log| {
                log.stored.get("usage/calls").cloned().filter(|calls| {
                    calls.as_array().is_some_and(|calls| {
                        calls
                            .iter()
                            .any(|call| call["endpoint"] == "vehicle_data" && call["status"] == 408)
                    })
                })
            })
            .await;
        assert!(calls.is_array());
        settle().await;
        assert_eq!(home.fake.sent("/vehicle_data").len(), 1, "not retried");
        assert!(
            matches!(
                home.log.lock().expect("log").health.last(),
                Some(Health::Running) | None
            ),
            "asleep isn't trouble"
        );
    }
}

/// The account device, its controls, and Refresh and Wake.
mod controls {
    use std::time::Duration;

    use irori_protocol::Health;
    use irori_types::{NumberSetValue, SelectOption, Service};
    use serde_json::{Value, json};

    use super::VIN;
    use super::reading::{answer, car, signed_in};
    use crate::fake::{Answer, Fake};

    fn calls_for(home: &super::protocol::Home, endpoint: &str) -> Vec<Value> {
        home.log
            .lock()
            .expect("log")
            .stored
            .get("usage/calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|call| call["endpoint"] == endpoint)
            .cloned()
            .collect()
    }

    #[tokio::test]
    async fn the_account_shows_the_month_and_the_cars_hang_off_it() {
        let fake = Fake::start().await;
        fake.answer(
            &format!("GET /na/api/1/vehicles/{VIN}/vehicle_data"),
            vec![Answer::with(200, answer("vehicle_data"))],
        );
        let home = signed_in(fake, vec![car(VIN, "Red car", "online")], vec![]).await;
        let spent = home
            .until("Irori's spend after the read", |log| {
                log.state("tesla_account:spent_irori")
                    .filter(|spent| spent["value"].as_f64() > Some(0.0))
            })
            .await;
        assert!(spent["value"].as_f64() < Some(0.1), "{spent}");
        let polling = home
            .until("the polling mode", |log| log.state("tesla_account:polling"))
            .await;
        assert_eq!(polling["option"], "Balanced");
        // On the map, the car hangs off Tesla's cloud.
        let car = home
            .until("the car on the map", |log| {
                let map = log.networks.first()?;
                map.links.iter().find(|link| link.from == VIN).cloned()
            })
            .await;
        assert_eq!(car.to, "cloud");
        let log = home.log.lock().expect("log");
        assert_eq!(
            log.devices[VIN]
                .via_device_unique_id
                .as_ref()
                .map(|id| id.as_str()),
            Some("tesla_account")
        );
        let refresh = &log.entities[&format!("{VIN}:refresh")];
        assert_eq!(
            refresh.name.as_ref().map(|name| name.as_str()),
            Some("Refresh (≈$0.002)")
        );
        assert!(
            !log.states.contains_key("tesla_account:spent_account"),
            "unknown until a dashboard figure"
        );
    }

    #[tokio::test]
    async fn refresh_reads_an_awake_car_and_never_wakes_a_sleeping_one() {
        let fake = Fake::start().await;
        let path = format!("GET /na/api/1/vehicles/{VIN}/vehicle_data");
        fake.answer(&path, vec![Answer::with(200, answer("vehicle_data"))]);
        let mut cached = answer("vehicle_data");
        cached["response"]["charge_state"]["battery_level"] = json!(61);
        fake.answer(
            &format!("GET /na/api/1/vehicles/{VIN}/vehicle_data_cached"),
            vec![Answer::with(200, cached)],
        );
        let home = signed_in(fake, vec![car(VIN, "Red car", "online")], vec![]).await;
        home.until("the first read", |log| log.state(&format!("{VIN}:battery")))
            .await;
        home.call(&format!("{VIN}:refresh"), Service::ButtonPress)
            .await
            .expect("refreshed");
        assert_eq!(home.fake.sent("/vehicle_data").len(), 2);
        let refreshes = home
            .until("the refresh on the meter", |log| {
                let calls = log.stored.get("usage/calls")?.as_array()?.clone();
                calls
                    .into_iter()
                    .find(|call| call["why"]["kind"] == "refresh")
            })
            .await;
        assert_eq!(refreshes["endpoint"], "vehicle_data");

        // It falls asleep: Refresh shows what Tesla last heard, and wakes nothing.
        home.fake
            .answer(&path, vec![Answer::with(408, json!({ "error": "asleep" }))]);
        home.call(&format!("{VIN}:refresh"), Service::ButtonPress)
            .await
            .expect("refreshed from the cache");
        let battery = home
            .until("the cached battery", |log| {
                log.state(&format!("{VIN}:battery"))
                    .filter(|battery| battery["value"] == 61.0)
            })
            .await;
        assert_eq!(battery["value"], 61.0);
        assert!(home.fake.sent("/wake_up").is_empty());
        let state = home.log.lock().expect("log").state(&format!("{VIN}:state"));
        assert_eq!(state.expect("a state")["value"], "asleep");
    }

    #[tokio::test]
    async fn wake_is_billed_as_a_wake_you_asked_for() {
        let fake = Fake::start().await;
        fake.answer(
            &format!("POST /na/api/1/vehicles/{VIN}/wake_up"),
            vec![Answer::ok(json!({ "vin": VIN, "state": "asleep" }))],
        );
        fake.answer(
            &format!("GET /na/api/1/vehicles/{VIN}/vehicle_data_cached"),
            vec![Answer::status(404)],
        );
        // Still coming up at the first look, awake at the second.
        fake.answer(
            &format!("GET /na/api/1/vehicles/{VIN}/vehicle_data"),
            vec![
                Answer::with(408, json!({ "error": "asleep" })),
                Answer::with(200, answer("vehicle_data")),
            ],
        );
        let home = signed_in(fake, vec![car(VIN, "Red car", "asleep")], vec![]).await;
        home.until("the car", |log| log.devices.get(VIN).cloned())
            .await;
        home.call(&format!("{VIN}:wake"), Service::ButtonPress)
            .await
            .expect("woken");
        let wakes = home
            .until("the wake on the meter", |log| {
                let calls = log.stored.get("usage/calls")?.as_array()?.clone();
                calls.into_iter().find(|call| call["endpoint"] == "wake_up")
            })
            .await;
        assert_eq!(
            wakes["why"],
            json!({ "kind": "command", "what": "Wake", "by": "you" })
        );
        assert_eq!(wakes["cost"], 20_000);
        assert_eq!(calls_for(&home, "wake_up").len(), 1);
        // Pressed again while it's coming up: the wake under way covers it, nothing is billed.
        home.call(&format!("{VIN}:wake"), Service::ButtonPress)
            .await
            .expect("still waking");
        assert_eq!(
            home.fake.sent("/wake_up").len(),
            1,
            "one wake for two presses"
        );
        home.until("the car shown awake", |log| {
            (log.state(&format!("{VIN}:state"))?["value"] == "online").then_some(())
        })
        .await;
        let looks = home
            .until("both looks on the meter", |log| {
                let calls: Vec<Value> = log
                    .stored
                    .get("usage/calls")?
                    .as_array()?
                    .iter()
                    .filter(|call| call["endpoint"] == "vehicle_data")
                    .cloned()
                    .collect();
                (calls.len() == 2).then_some(calls)
            })
            .await;
        assert!(
            looks.iter().all(|call| call["why"]["what"] == "Wake"),
            "the looks are on the press, not the schedule: {looks:?}"
        );
    }

    #[tokio::test]
    async fn polling_off_and_the_allowance_are_kept_and_change_the_plan() {
        let fake = Fake::start().await;
        fake.answer(
            &format!("GET /na/api/1/vehicles/{VIN}/vehicle_data"),
            vec![Answer::with(200, answer("vehicle_data"))],
        );
        let home = signed_in(fake, vec![car(VIN, "Red car", "online")], vec![]).await;
        home.until("the first read", |log| log.state(&format!("{VIN}:battery")))
            .await;
        home.call(
            "tesla_account:polling",
            Service::SelectSelectOption(SelectOption {
                option: "Off".into(),
            }),
        )
        .await
        .expect("off");
        assert!(
            home.call(
                "tesla_account:polling",
                Service::SelectSelectOption(SelectOption {
                    option: "Sometimes".into()
                }),
            )
            .await
            .is_err()
        );
        home.until("off, kept", |log| {
            (log.stored.get("prefs")?["mode"] == "off").then_some(())
        })
        .await;
        home.until("off, shown", |log| {
            (log.state("tesla_account:polling")?["option"] == "Off").then_some(())
        })
        .await;

        // Nothing left to read with: the account says so, and why.
        home.call(
            "tesla_account:allowance",
            Service::NumberSetValue(NumberSetValue { value: 0.0 }),
        )
        .await
        .expect("set");
        home.until("the allowance kept", |log| {
            (log.stored.get("prefs")?["allowance"] == 0).then_some(())
        })
        .await;
        home.until("the allowance shown", |log| {
            (log.state("tesla_account:allowance")?["value"] == 0.0).then_some(())
        })
        .await;
        home.call(
            "tesla_account:polling",
            Service::SelectSelectOption(SelectOption {
                option: "Balanced".into(),
            }),
        )
        .await
        .expect("balanced");
        let why = home
            .until("the warning", |log| match log.health.last() {
                Some(Health::Degraded(why)) => Some(why.clone()),
                _ => None,
            })
            .await;
        assert!(
            why.starts_with("Nothing is left of the $0.00 allowance"),
            "{why}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            home.fake.sent("/vehicle_data").len(),
            1,
            "nothing read on its own"
        );
    }

    #[tokio::test]
    async fn the_usage_page_reads_the_month_and_changes_it_for_the_owner_only() {
        let fake = Fake::start().await;
        fake.answer(
            &format!("GET /na/api/1/vehicles/{VIN}/vehicle_data"),
            vec![Answer::with(200, answer("vehicle_data"))],
        );
        let home = signed_in(fake, vec![car(VIN, "Red car", "online")], vec![]).await;
        home.until("the first read", |log| log.state(&format!("{VIN}:battery")))
            .await;

        let summary = home
            .page("usage.summary", json!({}), false)
            .await
            .expect("a summary");
        assert_eq!(summary["mode"], "Balanced");
        assert_eq!(summary["can_change"], false);
        assert!(summary["irori"]["micros"].as_u64() > Some(0), "{summary}");
        assert!(summary["others"].is_null());

        let days = home
            .page("usage.days", json!({}), false)
            .await
            .expect("days");
        let targets = days["this"]["targets"].as_array().expect("targets");
        assert!(
            targets
                .iter()
                .any(|t| t["name"] == "Red car" && t["kind"] == "car"),
            "{days}"
        );
        let calls = home
            .page("usage.calls", json!({}), false)
            .await
            .expect("calls");
        assert!(
            calls
                .as_array()
                .expect("calls")
                .iter()
                .any(|call| call["target"] == "Red car" && call["what"] == "Reading the car"),
            "{calls}"
        );

        let refused = home
            .page("usage.set", json!({ "mode": "Saving" }), false)
            .await
            .expect_err("not the owner");
        assert!(refused.contains("owner"), "{refused}");

        let changed = home
            .page(
                "usage.set",
                json!({ "mode": "Saving", "allowance": "12" }),
                true,
            )
            .await
            .expect("changed");
        assert_eq!(changed["mode"], "Saving");
        assert_eq!(changed["allowance"]["micros"], 12_000_000);
        home.until("the select shows it", |log| {
            (log.state("tesla_account:polling")?["option"] == "Saving").then_some(())
        })
        .await;

        let calibrated = home
            .page("usage.calibrate", json!({ "spent": "3.10" }), true)
            .await
            .expect("a figure");
        assert!(calibrated["others"]["micros"].as_u64() > Some(3_000_000));
        assert!(calibrated["checked_at"].is_u64());
        home.until("the account's spend shown", |log| {
            log.state("tesla_account:spent_account")
        })
        .await;

        let unknown = home
            .page("usage.nothing", json!({}), true)
            .await
            .expect_err("no such method");
        assert!(unknown.contains("usage.nothing"));
    }
}

/// Commands: signed for a car, plain for a site; each on the meter, its result shown at once and
/// credited to the call.
mod commands {
    use irori_types::{ClimateHvacMode, HvacMode, LockCode, NumberSetValue, SelectOption, Service};
    use serde_json::{Value, json};

    use super::VIN;
    use super::protocol::Home;
    use super::reading::{SITE, answer, car, signed_in, site};
    use crate::fake::{self, Answer, Fake};

    /// The context id `Home::call` sends.
    const CALL: &str = "01K5B2Q9A1B2C3D4E5F6G7H8J9";

    fn billed(home: &Home, endpoint: &str) -> Vec<Value> {
        home.log
            .lock()
            .expect("log")
            .stored
            .get("usage/calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|call| call["endpoint"] == endpoint)
            .cloned()
            .collect()
    }

    async fn on_the_meter(home: &Home, endpoint: &str, count: usize) -> Vec<Value> {
        home.until(&format!("{count} {endpoint} on the meter"), |log| {
            let calls: Vec<Value> = log
                .stored
                .get("usage/calls")?
                .as_array()?
                .iter()
                .filter(|call| call["endpoint"] == endpoint)
                .cloned()
                .collect();
            (calls.len() == count).then_some(calls)
        })
        .await
    }

    fn the_car(home: &Home) -> fake::Car {
        home.fake
            .state
            .lock()
            .expect("fake")
            .cars
            .get(VIN)
            .cloned()
            .expect("the car")
    }

    /// A home with one car, read once, that takes signed commands.
    async fn with_a_car(state: &str, change: impl FnOnce(&mut fake::Car)) -> Home {
        let fake = Fake::start().await;
        fake.answer(
            &format!("GET /na/api/1/vehicles/{VIN}/vehicle_data"),
            vec![Answer::with(200, answer("vehicle_data"))],
        );
        fake.answer(
            &format!("GET /na/api/1/vehicles/{VIN}/vehicle_data_cached"),
            vec![Answer::with(200, answer("vehicle_data"))],
        );
        let mut car_there = fake::Car::new();
        change(&mut car_there);
        fake.with(|state| {
            state.cars.insert(VIN.into(), car_there);
        });
        let home = signed_in(fake, vec![car(VIN, "Red car", state)], vec![]).await;
        home.until("the lock", |log| log.state(&format!("{VIN}:lock")))
            .await;
        home
    }

    /// The trunk opens with a toggle. A car Irori has only the cached, possibly old, reading of
    /// is read live before the toggle goes: shut, it opens; opened since (by Home Assistant, the
    /// app, a hand), nothing is sent, which would shut it.
    #[tokio::test]
    async fn the_trunk_toggle_looks_first_when_the_reading_is_old() {
        for opened_since in [false, true] {
            let home = with_a_car("asleep", |_| {}).await;
            let mut now = answer("vehicle_data");
            now["response"]["vehicle_state"]["rt"] = json!(u8::from(opened_since));
            home.fake.answer(
                &format!("GET /na/api/1/vehicles/{VIN}/vehicle_data"),
                vec![Answer::with(200, now)],
            );
            let live_before = home.fake.sent(&format!("{VIN}/vehicle_data")).len();
            let trunk = format!("{VIN}:trunk");

            home.call(&trunk, Service::CoverOpen).await.expect("opened");
            assert_eq!(
                home.fake.sent(&format!("{VIN}/vehicle_data")).len(),
                live_before + 1,
                "read live once, first"
            );
            let toggles = |home: &Home| {
                the_car(home)
                    .done
                    .iter()
                    .filter(|(domain, bytes)| *domain == 2 && bytes.starts_with(&[0x22]))
                    .count()
            };
            assert_eq!(
                toggles(&home),
                usize::from(!opened_since),
                "the toggle goes only to a shut trunk"
            );
            assert_eq!(
                home.log
                    .lock()
                    .expect("log")
                    .state(&trunk)
                    .expect("a trunk")["state"],
                "open"
            );

            // Now freshly known to be open: a second Open sends nothing and reads nothing.
            home.call(&trunk, Service::CoverOpen)
                .await
                .expect("still open");
            assert_eq!(
                home.fake.sent(&format!("{VIN}/vehicle_data")).len(),
                live_before + 1
            );
            assert_eq!(toggles(&home), usize::from(!opened_since));
        }
    }

    /// The usage page needs the core to route its questions here, which it does only for a
    /// manifest asking for a release that can answer them.
    #[test]
    fn the_manifest_asks_for_a_core_that_routes_its_page() {
        use irori_protocol::Protocol as _;
        let manifest = irori_protocol::parse_manifest(crate::Tesla::MANIFEST).expect("valid");
        assert!(manifest.answers_its_page());
    }

    /// Two presses inside one round trip to Tesla: the second sees the first on its way.
    #[tokio::test]
    async fn two_wake_presses_at_once_pay_for_one_wake() {
        let home = with_a_car("asleep", |_| {}).await;
        home.fake.answer(
            &format!("POST /na/api/1/vehicles/{VIN}/wake_up"),
            vec![Answer::ok(json!({ "vin": VIN, "state": "asleep" }))],
        );
        let wake = format!("{VIN}:wake");
        let (first, second) = tokio::join!(
            home.call(&wake, Service::ButtonPress),
            home.call(&wake, Service::ButtonPress)
        );
        first.expect("woken");
        second.expect("already waking");
        assert_eq!(home.fake.sent("/wake_up").len(), 1);
    }

    /// Opening the trunk of a sleeping car: one wake, then only the cheap "which cars are
    /// awake" check until it's up, then one full read, then the toggle.
    #[tokio::test]
    async fn a_sleeping_car_s_trunk_waits_cheaply_for_the_car_to_wake() {
        use super::reading::car as listed;
        let home = with_a_car("asleep", |_| {}).await;
        let mut shut = answer("vehicle_data");
        shut["response"]["vehicle_state"]["rt"] = json!(0);
        home.fake.answer(
            &format!("GET /na/api/1/vehicles/{VIN}/vehicle_data"),
            vec![
                Answer::with(408, json!({ "error": "vehicle unavailable: asleep" })),
                Answer::with(200, shut),
            ],
        );
        home.fake.answer(
            "GET /na/api/1/vehicles",
            vec![
                Answer::ok(json!([listed(VIN, "Red car", "asleep")])),
                Answer::ok(json!([listed(VIN, "Red car", "asleep")])),
                Answer::ok(json!([listed(VIN, "Red car", "online")])),
            ],
        );
        let reads_before = home.fake.sent(&format!("{VIN}/vehicle_data")).len();
        let lists_before = home.fake.sent("/api/1/vehicles").len();

        home.call(&format!("{VIN}:trunk"), Service::CoverOpen)
            .await
            .expect("opened");
        assert_eq!(home.fake.sent("/wake_up").len(), 1, "woken once");
        assert_eq!(
            home.fake.sent(&format!("{VIN}/vehicle_data")).len(),
            reads_before + 2,
            "asleep once, then one read once it's up"
        );
        assert_eq!(
            home.fake.sent("/api/1/vehicles").len(),
            lists_before + 3,
            "the cheap check until it's up"
        );
        assert!(
            the_car(&home)
                .done
                .iter()
                .any(|(domain, bytes)| *domain == 2 && bytes.starts_with(&[0x22])),
            "then the toggle"
        );
    }

    #[tokio::test]
    async fn unlock_is_signed_lands_and_shows_at_once_credited_to_the_call() {
        let home = with_a_car("online", |_| {}).await;
        let lock = format!("{VIN}:lock");
        assert_eq!(
            home.log.lock().expect("log").state(&lock).expect("a lock")["state"],
            "locked"
        );

        home.call(&lock, Service::LockUnlock(LockCode::default()))
            .await
            .expect("unlocked");
        // A handshake with the car's security computer, then the command, which it checked.
        assert_eq!(the_car(&home).done, vec![(2, vec![0x10, 0x00])]);
        let log = home.log.lock().expect("log").state(&lock).expect("a lock");
        assert_eq!(log["state"], "unlocked");
        assert_eq!(
            home.log.lock().expect("log").caused[&lock].as_deref(),
            Some(CALL)
        );
        let calls = on_the_meter(&home, "signed_command", 2).await;
        assert!(calls.iter().all(|call| call["why"]
            == json!({ "kind": "command", "what": "Unlock", "by": "you" })
            && call["cost"] == 1_000));

        // The climate is the other computer: a session of its own.
        home.call(
            &format!("{VIN}:climate"),
            Service::ClimateSetHvacMode(ClimateHvacMode {
                hvac_mode: HvacMode::HeatCool,
            }),
        )
        .await
        .expect("climate on");
        assert_eq!(
            the_car(&home).done[1],
            (3, vec![0x12, 0x04, 0x52, 0x02, 0x08, 0x01])
        );
        // Locking again uses the session it has: one call, not two.
        home.call(&lock, Service::LockLock(LockCode::default()))
            .await
            .expect("locked");
        on_the_meter(&home, "signed_command", 5).await;
        assert_eq!(the_car(&home).counters[&2], 2);
        assert!(
            home.fake.sent("/wake_up").is_empty(),
            "an awake car isn't woken"
        );
    }

    #[tokio::test]
    async fn a_sleeping_car_is_woken_once_for_two_commands() {
        let home = with_a_car("asleep", |car| car.asleep = true).await;
        let (climate_id, lock_id) = (format!("{VIN}:climate"), format!("{VIN}:lock"));
        let (climate, unlock) = tokio::join!(
            home.call(&climate_id, Service::ClimateTurnOn),
            home.call(&lock_id, Service::LockUnlock(LockCode::default())),
        );
        climate.expect("climate on");
        unlock.expect("unlocked");
        let car = the_car(&home);
        assert_eq!(car.wakes, 1, "one wake for both");
        assert_eq!(car.done.len(), 2);
        let wakes = on_the_meter(&home, "wake_up", 1).await;
        assert_eq!(wakes[0]["why"]["kind"], "wake_for_command");
        assert_eq!(wakes[0]["cost"], 20_000);
        // The tries while it woke were billed, and on the commands that made them.
        let asleep: Vec<Value> = billed(&home, "signed_command")
            .into_iter()
            .filter(|call| call["status"] == 408)
            .collect();
        assert!(!asleep.is_empty());
        assert!(asleep.iter().all(|call| call["why"]["kind"] == "command"));
        home.until("the car shown awake", |log| {
            (log.state(&format!("{VIN}:state"))?["value"] == "online").then_some(())
        })
        .await;
    }

    #[tokio::test]
    async fn commands_home_assistant_sent_with_the_same_key_are_caught_up_with() {
        let home = with_a_car("online", |_| {}).await;
        let lock = format!("{VIN}:lock");
        home.call(&lock, Service::LockUnlock(LockCode::default()))
            .await
            .expect("unlocked");

        // Home Assistant sent nine commands with the same key: the car's counter is ahead.
        home.fake.with(|state| {
            state
                .cars
                .get_mut(VIN)
                .expect("the car")
                .counters
                .insert(2, 10);
        });
        home.call(&lock, Service::LockLock(LockCode::default()))
            .await
            .expect("locked after catching up");
        assert_eq!(the_car(&home).counters[&2], 11);

        // The car restarted: a new session, which it hands over with its refusal.
        home.fake.with(|state| {
            let car = state.cars.get_mut(VIN).expect("the car");
            car.epoch = vec![0x99; 16];
            car.counters.insert(2, 0);
        });
        home.call(&lock, Service::LockUnlock(LockCode::default()))
            .await
            .expect("unlocked in the new session");
        assert_eq!(the_car(&home).done.len(), 3);
        // Handshake and unlock, refused lock and lock, refused unlock and unlock.
        on_the_meter(&home, "signed_command", 6).await;
    }

    #[tokio::test]
    async fn a_car_without_irori_s_key_is_told_how_to_add_it_and_works_once_it_has_it() {
        let home = with_a_car("online", |car| car.paired = false).await;
        let lock = format!("{VIN}:lock");
        let refused = home
            .call(&lock, Service::LockUnlock(LockCode::default()))
            .await
            .expect_err("no key on the car");
        assert!(
            refused.message.contains("tesla.com/_ak/"),
            "{}",
            refused.message
        );
        assert_eq!(
            home.fake.sent("/signed_command").len(),
            1,
            "the handshake only"
        );
        let has_step = |log: &super::protocol::Log| {
            log.waiting
                .iter()
                .any(|step| step.unique_id.as_str() == VIN)
        };
        home.until("the step to add the key", |log| has_step(log).then_some(()))
            .await;

        // The person adds the key: the next command goes through, and the step goes.
        home.fake.with(|state| {
            state.cars.get_mut(VIN).expect("the car").paired = true;
        });
        home.call(&lock, Service::LockUnlock(LockCode::default()))
            .await
            .expect("unlocked");
        assert_eq!(the_car(&home).done.len(), 1);
        home.until("the step gone", |log| (!has_step(log)).then_some(()))
            .await;
    }

    #[tokio::test]
    async fn a_busy_car_gets_a_command_again_but_never_a_honk_and_refusals_are_passed_on() {
        use prost::Message as _;
        let refusal = crate::signed::proto::Response {
            action_status: Some(crate::signed::proto::ActionStatus {
                result: 1,
                result_reason: Some(crate::signed::proto::ResultReason {
                    kind: Some(crate::signed::proto::result_reason::Kind::PlainText(
                        "user_present".into(),
                    )),
                }),
            }),
        }
        .encode_to_vec();
        let home = with_a_car("online", |car| car.answers = vec![refusal]).await;
        let refused = home
            .call(
                &format!("{VIN}:charge_limit"),
                Service::NumberSetValue(NumberSetValue { value: 90.0 }),
            )
            .await
            .expect_err("refused by the car");
        assert!(
            refused.message.contains("user present"),
            "{}",
            refused.message
        );
        let limit = home
            .log
            .lock()
            .expect("log")
            .state(&format!("{VIN}:charge_limit"));
        assert_eq!(limit.expect("a limit")["value"], 80.0, "nothing changed");

        // Busy: the lock goes again and lands; a honk isn't sent twice.
        home.fake
            .with(|state| state.cars.get_mut(VIN).expect("the car").busy = 1);
        home.call(
            &format!("{VIN}:lock"),
            Service::LockLock(LockCode::default()),
        )
        .await
        .expect("locked on the second try");
        // The refused charge limit reached the car too.
        assert_eq!(the_car(&home).done.len(), 2);
        home.fake
            .with(|state| state.cars.get_mut(VIN).expect("the car").busy = 1);
        let busy = home
            .call(&format!("{VIN}:honk"), Service::ButtonPress)
            .await
            .expect_err("not honked twice");
        assert!(busy.message.contains("busy"), "{}", busy.message);
        assert_eq!(the_car(&home).done.len(), 2, "the busy car didn't honk");
        home.call(&format!("{VIN}:honk"), Service::ButtonPress)
            .await
            .expect("honked");
        assert_eq!(the_car(&home).done.len(), 3);
    }

    #[tokio::test]
    async fn a_site_s_settings_change_over_plain_rest() {
        let fake = Fake::start().await;
        let sites = format!("/na/api/1/energy_sites/{SITE}");
        fake.answer(
            &format!("GET {sites}/live_status"),
            vec![Answer::with(200, answer("live_status"))],
        );
        fake.answer(
            &format!("GET {sites}/site_info"),
            vec![Answer::with(200, answer("site_info"))],
        );
        fake.answer(
            &format!("GET {sites}/calendar_history"),
            vec![Answer::with(200, answer("history"))],
        );
        fake.answer(
            &format!("POST {sites}/backup"),
            vec![Answer::ok(json!({ "code": 201, "message": "Updated" }))],
        );
        fake.answer(
            &format!("POST {sites}/operation"),
            vec![Answer::with(
                403,
                json!({ "error": "not allowed for this site" }),
            )],
        );
        let home = signed_in(fake, vec![site()], vec![]).await;
        let reserve = format!("{SITE}:backup_reserve");
        home.until("the reserve", |log| log.state(&reserve)).await;

        home.call(
            &reserve,
            Service::NumberSetValue(NumberSetValue { value: 20.0 }),
        )
        .await
        .expect("set");
        let sent = home.fake.sent("/backup");
        assert_eq!(sent[0].body, json!({ "backup_reserve_percent": 20 }));
        {
            let log = home.log.lock().expect("log");
            assert_eq!(log.state(&reserve).expect("the reserve")["value"], 20.0);
            assert_eq!(log.caused[&reserve].as_deref(), Some(CALL));
        }
        let calls = on_the_meter(&home, "energy_command", 1).await;
        assert_eq!(calls[0]["why"]["what"], "Set the backup reserve to 20%");

        let refused = home
            .call(
                &format!("{SITE}:operation_mode"),
                Service::SelectSelectOption(SelectOption {
                    option: "backup".into(),
                }),
            )
            .await
            .expect_err("refused");
        assert!(
            refused.message.contains("not allowed"),
            "{}",
            refused.message
        );
        on_the_meter(&home, "energy_command", 2).await;
    }
}
