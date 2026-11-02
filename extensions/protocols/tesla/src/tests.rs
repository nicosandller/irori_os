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
    }

    pub struct Home {
        pub fake: Fake,
        pub log: Arc<Mutex<Log>>,
        actions: mpsc::Sender<irori_protocol::IncomingAction>,
        /// Kept open: the protocol stops when the core's end of its calls closes.
        _calls: mpsc::Sender<irori_protocol::IncomingCall>,
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
                    host::Op::DescribeDevice(_, reply)
                    | host::Op::DescribeEntity(_, reply)
                    | host::Op::RemoveDevice(_, reply)
                    | host::Op::RemoveEntity(_, reply)
                    | host::Op::SetAvailability(_, _, reply) => {
                        let _ = reply.send(Ok(()));
                    }
                    host::Op::SetHealth(health) => log.health.push(health),
                    host::Op::SetWaiting(waiting) => log.waiting = waiting,
                    host::Op::SetAvailableActions(actions) => log.actions = actions,
                    host::Op::SetUnmodeled(_)
                    | host::Op::SetNetworks(_)
                    | host::Op::SetActionOpen(_, _) => {}
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
        let timing = Timing {
            hosts: fake.hosts(),
            now: nov_10,
            relist: Duration::from_secs(3600),
            save: Duration::from_millis(50),
        };
        let drive = tokio::spawn(drive(settings, ctx, timing));
        Home {
            fake,
            log,
            actions: host.actions,
            _calls: host.calls,
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
        assert!(home.step("Tesla has a different").is_none());
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
                    .find(|step| step.reason.starts_with("Tesla has a different"))
                    .cloned()
            })
            .await;
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
                .any(|step| step.reason.starts_with("Tesla has a different")))
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
