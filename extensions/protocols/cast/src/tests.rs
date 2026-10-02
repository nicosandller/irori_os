//! The session against a local TLS receiver. Nothing here browses the network.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use irori_protocol::host::{self, Reports};
use irori_protocol::{IncomingCall, ProtocolError, ServiceError};
use irori_types::{
    Availability, Context, ContextId, MediaPlayerState, Origin, PlayMedia, Playback, Service,
    ServiceCall, State, StateReport, UniqueId, VolumeSet,
};
use tokio::sync::{mpsc, watch};

use crate::discover::{Discovery, Found};
use crate::drive;
use crate::fake::{Fake, Script};
use crate::settings::Settings;

const UUID: &str = "00112233445566778899aabbccddeeff";

#[derive(Debug, Default)]
struct Log {
    health: Vec<irori_protocol::Health>,
    availability: Vec<Availability>,
    devices: u32,
    entities: u32,
}

struct World {
    discovery: mpsc::Sender<Discovery>,
    calls: mpsc::Sender<IncomingCall>,
    reports: Reports,
    log: Arc<Mutex<Log>>,
    /// Kept so `next_call` doesn't see the stop channel close and end the run.
    _stop: watch::Sender<bool>,
    drive: tokio::task::JoinHandle<Result<(), ProtocolError>>,
}

async fn world(settings: Settings) -> World {
    let (ctx, host) = irori_protocol::host::connect();
    let log = Arc::new(Mutex::new(Log::default()));
    let recorded = Arc::clone(&log);
    let mut ops = host.ops;
    tokio::spawn(async move {
        while let Some(op) = ops.recv().await {
            match op {
                host::Op::DescribeDevice(_, reply) => {
                    recorded.lock().expect("log").devices += 1;
                    let _ = reply.send(Ok(()));
                }
                host::Op::DescribeEntity(_, reply) => {
                    recorded.lock().expect("log").entities += 1;
                    let _ = reply.send(Ok(()));
                }
                host::Op::SetAvailability(_, availability, reply) => {
                    recorded
                        .lock()
                        .expect("log")
                        .availability
                        .push(availability);
                    let _ = reply.send(Ok(()));
                }
                host::Op::SetHealth(health) => {
                    recorded.lock().expect("log").health.push(health);
                }
                host::Op::RemoveDevice(_, reply) | host::Op::RemoveEntity(_, reply) => {
                    let _ = reply.send(Ok(()));
                }
                host::Op::SetWaiting(_)
                | host::Op::SetUnmodeled(_)
                | host::Op::SetAvailableActions(_) => {}
                host::Op::Load(_, reply) => {
                    let _ = reply.send(Ok(None));
                }
                host::Op::Store(_, _, reply) => {
                    let _ = reply.send(Ok(()));
                }
            }
        }
    });
    let (discovery, incoming) = mpsc::channel(8);
    let drive = tokio::spawn(drive(settings, ctx, incoming));
    World {
        discovery,
        calls: host.calls,
        reports: host.reports,
        log,
        _stop: host.stop,
        drive,
    }
}

impl World {
    async fn shutdown(self) {
        drop(self.discovery);
        let _ = tokio::time::timeout(Duration::from_secs(2), self.drive).await;
    }

    fn log(&self) -> Log {
        let log = self.log.lock().expect("log");
        Log {
            health: log.health.clone(),
            availability: log.availability.clone(),
            devices: log.devices,
            entities: log.entities,
        }
    }
}

fn receiver(addr: std::net::SocketAddr) -> Found {
    Found {
        uuid: UUID.to_owned(),
        name: "Living room".to_owned(),
        model: "Chromecast".to_owned(),
        version: "1".to_owned(),
        address: addr,
        known_host: None,
    }
}

fn entity() -> UniqueId {
    UniqueId::try_from(format!("media:{UUID}")).expect("id")
}

fn call(
    service: Service,
) -> (
    IncomingCall,
    tokio::sync::oneshot::Receiver<Result<(), ServiceError>>,
) {
    irori_protocol::host::incoming_call(ServiceCall {
        unique_id: entity(),
        service,
        context: Context {
            id: ContextId::try_from("01K5B2Q9A1B2C3D4E5F6G7H8J9").expect("context"),
            parent_id: None,
            origin: Origin::System,
        },
    })
}

async fn see(world: &World, fake: &Fake) {
    world
        .discovery
        .send(Discovery::Seen(receiver(fake.addr)))
        .await
        .expect("discovery is open");
}

async fn ask(world: &World, service: Service) -> Result<(), ServiceError> {
    let (incoming, answer) = call(service);
    world
        .calls
        .send(incoming)
        .await
        .expect("the protocol is running");
    tokio::time::timeout(Duration::from_secs(5), answer)
        .await
        .expect("a reply in time")
        .expect("the reply was sent")
}

async fn wait_player(reports: &Reports, want: Playback) -> MediaPlayerState {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        for report in reports.drain() {
            if let Some(player) = player(&report)
                && player.state == want
            {
                return player;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("no {want:?} report");
        }
        tokio::select! {
            _ = reports.ready() => {}
            _ = tokio::time::sleep_until(deadline) => panic!("no {want:?} report"),
        }
    }
}

fn player(report: &StateReport) -> Option<MediaPlayerState> {
    match &report.state {
        Some(State::MediaPlayer(player)) => Some(player.clone()),
        _ => None,
    }
}

async fn wait_kind(fake: &Fake, kind: &str) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let kinds = types_of(&fake.payloads());
        if kinds.iter().any(|item| item == kind) {
            return kinds;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("no {kind} message in {kinds:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn types_of(payloads: &[String]) -> Vec<String> {
    payloads
        .iter()
        .filter_map(|payload| {
            let value: serde_json::Value = serde_json::from_str(payload).ok()?;
            value.get("type")?.as_str().map(str::to_owned)
        })
        .collect()
}

#[tokio::test]
async fn a_receiver_at_rest_is_an_idle_player() {
    let fake = Fake::start(Script::default()).await;
    let world = world(Settings::default()).await;
    see(&world, &fake).await;
    let player = wait_player(&world.reports, Playback::Idle).await;
    assert_eq!(player.volume, Some(20));
    let log = world.log();
    assert!(log.devices >= 1, "the device is described");
    assert!(log.entities >= 1, "the player is described");
    assert!(log.availability.contains(&Availability::Unavailable));
    assert!(log.availability.contains(&Availability::Available));
    world.shutdown().await;
}

#[tokio::test]
async fn playing_a_url_loads_it_on_the_default_receiver() {
    let fake = Fake::start(Script::default()).await;
    let world = world(Settings::default()).await;
    see(&world, &fake).await;
    wait_player(&world.reports, Playback::Idle).await;
    let answered = ask(
        &world,
        Service::MediaPlayerPlayMedia(Box::new(PlayMedia {
            content_type: "video".to_owned(),
            content_id: "https://example.com/clip.mp4".to_owned(),
            title: Some("The evening news".to_owned()),
            artist: None,
            album: None,
            image_url: None,
        })),
    )
    .await;
    assert!(answered.is_ok(), "{answered:?}");
    let player = wait_player(&world.reports, Playback::Playing).await;
    assert_eq!(player.title.as_deref(), Some("The evening news"));
    let kinds = types_of(&fake.payloads());
    assert!(kinds.contains(&"LAUNCH".to_owned()), "{kinds:?}");
    assert!(kinds.contains(&"LOAD".to_owned()), "{kinds:?}");
    world.shutdown().await;
}

#[tokio::test]
async fn volume_is_sent_as_a_fraction() {
    let fake = Fake::start(Script::default()).await;
    let world = world(Settings::default()).await;
    see(&world, &fake).await;
    wait_player(&world.reports, Playback::Idle).await;
    let answered = ask(
        &world,
        Service::MediaPlayerVolumeSet(VolumeSet { volume: 40 }),
    )
    .await;
    assert!(answered.is_ok(), "{answered:?}");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let level = loop {
        let found = fake.payloads().into_iter().find_map(|payload| {
            let value: serde_json::Value = serde_json::from_str(&payload).ok()?;
            if value.get("type")?.as_str() == Some("SET_VOLUME") {
                value.pointer("/volume/level")?.as_f64()
            } else {
                None
            }
        });
        if let Some(level) = found {
            break level;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("no volume message");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(level, 0.4);
    world.shutdown().await;
}

#[tokio::test]
async fn turning_off_quits_the_app() {
    let fake = Fake::start(Script {
        app_id: Some("YouTube".to_owned()),
        display_name: Some("YouTube".to_owned()),
        ..Script::default()
    })
    .await;
    let world = world(Settings::default()).await;
    see(&world, &fake).await;
    wait_player(&world.reports, Playback::Idle).await;
    let answered = ask(&world, Service::MediaPlayerTurnOff).await;
    assert!(answered.is_ok(), "{answered:?}");
    let kinds = wait_kind(&fake, "STOP").await;
    assert!(kinds.contains(&"STOP".to_owned()), "{kinds:?}");
    world.shutdown().await;
}

#[tokio::test]
async fn a_dropped_socket_marks_the_player_unavailable() {
    let fake = Fake::start(Script::default()).await;
    let world = world(Settings::default()).await;
    see(&world, &fake).await;
    wait_player(&world.reports, Playback::Idle).await;
    fake.close();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let log = world.log();
        let up = log
            .availability
            .iter()
            .position(|item| *item == Availability::Available);
        let down = log
            .availability
            .iter()
            .rposition(|item| *item == Availability::Unavailable);
        if up.is_some_and(|up| down.is_some_and(|down| down > up)) {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("stayed available: {:?}", log.availability);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    world.shutdown().await;
}

#[tokio::test]
async fn an_allowlist_skips_other_receivers() {
    let settings = serde_json::from_value(serde_json::json!({
        "uuids": ["ffffffffffffffffffffffffffffffff"]
    }))
    .expect("settings");
    let fake = Fake::start(Script::default()).await;
    let world = world(settings).await;
    see(&world, &fake).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let log = world.log();
    assert_eq!(log.devices, 0);
    assert_eq!(log.entities, 0);
    assert!(fake.payloads().is_empty(), "it was not connected");
    world.shutdown().await;
}

#[tokio::test]
async fn a_standby_tv_reports_standby_and_turn_on_launches_the_receiver() {
    let fake = Fake::start(Script {
        stand_by: Some(true),
        active_input: Some(false),
        app_id: None,
        level: None,
        muted: None,
        ..Script::default()
    })
    .await;
    let world = world(Settings::default()).await;
    see(&world, &fake).await;
    wait_player(&world.reports, Playback::Standby).await;
    let answered = ask(&world, Service::MediaPlayerTurnOn).await;
    assert!(answered.is_ok(), "{answered:?}");
    let kinds = wait_kind(&fake, "LAUNCH").await;
    assert!(!kinds.iter().any(|kind| kind == "LOAD"), "{kinds:?}");
    wait_player(&world.reports, Playback::Idle).await;
    world.shutdown().await;
}

#[tokio::test]
async fn a_file_address_is_refused() {
    let fake = Fake::start(Script::default()).await;
    let world = world(Settings::default()).await;
    see(&world, &fake).await;
    wait_player(&world.reports, Playback::Idle).await;
    let answered = ask(
        &world,
        Service::MediaPlayerPlayMedia(Box::new(PlayMedia {
            content_type: "video".to_owned(),
            content_id: "file:///tmp/clip.mp4".to_owned(),
            title: None,
            artist: None,
            album: None,
            image_url: None,
        })),
    )
    .await;
    let message = match answered {
        Err(error) => error.message,
        Ok(()) => panic!("file should be refused"),
    };
    assert_eq!(message, "only an http or https address can be played");
    assert!(!message.contains("file://"));
    let kinds = types_of(&fake.payloads());
    assert!(!kinds.iter().any(|kind| kind == "LOAD"), "{kinds:?}");
    world.shutdown().await;
}

#[tokio::test]
async fn a_receiver_that_moves_ignores_the_old_socket() {
    let first = Fake::start(Script::default()).await;
    let second = Fake::start(Script::default()).await;
    let world = world(Settings::default()).await;
    see(&world, &first).await;
    wait_player(&world.reports, Playback::Idle).await;
    world
        .discovery
        .send(Discovery::Seen(receiver(second.addr)))
        .await
        .expect("discovery is open");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let available = world
            .log()
            .availability
            .iter()
            .filter(|item| **item == Availability::Available)
            .count();
        if available >= 2 {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "the new address never came up: {:?}",
                world.log().availability
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    first.close();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let availability = world.log().availability;
    assert_eq!(
        availability.last().copied(),
        Some(Availability::Available),
        "{availability:?}"
    );
    world.shutdown().await;
}

#[tokio::test]
async fn an_unreachable_known_host_is_counted_without_naming_it() {
    let world = world(Settings::default()).await;
    world
        .discovery
        .send(Discovery::Unreachable {
            host: "10.0.0.9".to_owned(),
        })
        .await
        .expect("discovery is open");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let message = loop {
        let message = world
            .log()
            .health
            .into_iter()
            .find_map(|health| match health {
                irori_protocol::Health::Degraded(message) => Some(message),
                irori_protocol::Health::Running => None,
            });
        if let Some(message) = message {
            break message;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("health stayed clear");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(message, "1 Cast device can't be reached");
    assert!(!message.contains("10.0.0.9"));
    world.shutdown().await;
}
