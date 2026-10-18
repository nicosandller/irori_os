//! The live socket and the socket an extension dials in on (`docs/specs/api.md` §4, §6).
//!
//! `/api/ws` only pushes. The client sends nothing; commands stay on HTTP. `/api/extension`
//! carries the same messages a child process would, one JSON object per frame.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use axum::Extension;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use irori_core::Event;
use irori_protocol::{FromExt, ToExt};
use irori_types::{ExtensionId, LiveMessage, TokenId, UserId};
use tokio::sync::{mpsc, watch};

use super::auth::Actor;
use super::{AppState, refused};

/// How often an extension's connection is asked if it is still there.
const PING_EVERY: Duration = Duration::from_secs(30);
/// How long an extension may say nothing, pongs included, before the link is given up.
const QUIET_FOR: Duration = Duration::from_secs(40);

/// Open sockets, so ending a sign-in or a token can close the ones it opened.
#[derive(Debug, Default)]
pub(super) struct Sockets {
    seats: Mutex<Vec<Seat>>,
}

#[derive(Debug)]
struct Seat {
    kind: SeatKind,
    close: watch::Sender<bool>,
}

/// What opened a socket. An open home's socket is [`SeatKind::Open`] even when a cookie
/// came along: until the home asks who is there, that cookie isn't what let it in.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SeatKind {
    Open,
    Session { hash: String, user: UserId },
    Token { id: TokenId, user: UserId },
}

impl Sockets {
    fn open(&self, kind: SeatKind) -> watch::Receiver<bool> {
        let (close, closed) = watch::channel(false);
        let mut seats = self.seats.lock().unwrap_or_else(PoisonError::into_inner);
        seats.retain(|seat| !seat.close.is_closed());
        seats.push(Seat { kind, close });
        closed
    }

    pub(super) fn close_token(&self, id: &TokenId) {
        self.close(|kind| matches!(kind, SeatKind::Token { id: token, .. } if token == id));
    }

    pub(super) fn close_user_tokens(&self, user: &UserId) {
        self.close(|kind| matches!(kind, SeatKind::Token { user: owner, .. } if owner == user));
    }

    pub(super) fn close_all_tokens(&self) {
        self.close(|kind| matches!(kind, SeatKind::Token { .. }));
    }

    pub(super) fn close_session(&self, hash: &str) {
        self.close(|kind| {
            matches!(
                kind,
                SeatKind::Session { hash: session, .. } if session.as_str() == hash
            )
        });
    }

    /// Ends `user`'s sign-ins, apart from `except` (the screen that changed the password).
    pub(super) fn close_user_sessions(&self, user: &UserId, except: Option<&str>) {
        self.close(|kind| {
            matches!(
                kind,
                SeatKind::Session { hash, user: owner }
                    if owner == user && except != Some(hash.as_str())
            )
        });
    }

    /// The home has started asking who is there. Open sockets, and every sign-in, close.
    pub(super) fn close_open_and_sessions(&self) {
        self.close(|kind| matches!(kind, SeatKind::Open | SeatKind::Session { .. }));
    }

    fn close(&self, mut matches_kind: impl FnMut(&SeatKind) -> bool) {
        let mut seats = self.seats.lock().unwrap_or_else(PoisonError::into_inner);
        seats.retain(|seat| !seat.close.is_closed());
        for seat in seats.iter().filter(|seat| matches_kind(&seat.kind)) {
            let _ = seat.close.send(true);
        }
    }
}

/// Pushes the home, then each change, for as long as the socket stays open.
pub async fn feed(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
    Extension(who): Extension<Actor>,
) -> Response {
    if let Err(response) = same_server(&headers) {
        return *response;
    }
    let seat = seat_kind(&state, &who, &headers).await;
    // A sign-in is only as good as the person it was for: whoever they are can be given other
    // rooms, or stepped down, while the page is open.
    let signed_in = matches!(seat, SeatKind::Session { .. });
    let closed = state.0.sockets.open(seat);
    ws.on_upgrade(move |socket| push(socket, state, closed, who, signed_in))
}

async fn seat_kind(state: &AppState, who: &Actor, headers: &HeaderMap) -> SeatKind {
    if let Some(token) = &who.token {
        return SeatKind::Token {
            id: token.id.clone(),
            user: who.user_id(),
        };
    }
    // Locked, and the cookie is still a sign-in. An open home is `Open` on purpose.
    if state.0.config.people().await.locked()
        && let Some(hash) = state.0.auth.live_session(headers)
    {
        return SeatKind::Session {
            hash,
            user: who.user_id(),
        };
    }
    SeatKind::Open
}

/// How often a signed-in person's socket looks at whether they are still who it was opened
/// for. Rooms and roles can be changed by editing `users.toml` as well as from the page, and a
/// file has no event.
const PEOPLE_EVERY: Duration = Duration::from_secs(3);

async fn push(
    mut socket: WebSocket,
    state: AppState,
    mut closed: watch::Receiver<bool>,
    who: Actor,
    signed_in: bool,
) {
    // Before the snapshot. A change while that frame is on its way is then in this
    // queue. One that is in both is applied twice; the page keeps the newer one.
    let mut events = state.0.core.subscribe();
    // What this person has of the home. Asked again whenever the home changes shape, so a
    // device moved into their room is theirs as soon as the page is told, and one moved out
    // is not sent another word.
    let mut reach = who.reach(&state.0.core);
    if send_snapshot(&mut socket, &state, &reach).await.is_err() {
        return;
    }
    let mut people = tokio::time::interval(PEOPLE_EVERY);
    people.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            changed = closed.changed() => {
                if changed.is_err() || *closed.borrow() {
                    break;
                }
            }
            _ = people.tick(), if signed_in => {
                if !still_the_same_person(&state, &who).await {
                    break;
                }
            }
            incoming = events.recv() => {
                let event = match incoming {
                    Ok(event) => event,
                    // This client fell behind. A fresh picture, then a subscription that
                    // starts after it, so the gap is the snapshot rather than a skipped event.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        events = events.resubscribe();
                        reach = who.reach(&state.0.core);
                        if send_snapshot(&mut socket, &state, &reach).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                if changes_the_home(&event) && !reach.is_whole() {
                    reach = who.reach(&state.0.core);
                }
                let Some(message) = frame(event, &reach) else {
                    continue;
                };
                if send(&mut socket, &message).await.is_err() {
                    break;
                }
            }
        }
    }
}

/// Whether the person this socket was opened for still has the same role and the same rooms.
/// A person who has gone, or changed, is not who the socket was shown the home for.
async fn still_the_same_person(state: &AppState, who: &Actor) -> bool {
    let Some(me) = &who.user else {
        return true;
    };
    let people = state.0.config.people().await;
    people
        .user(&me.id)
        .is_some_and(|now| now.role == me.role && now.areas == me.areas)
}

/// Whether what a person has of the home could be different after this.
fn changes_the_home(event: &Event) -> bool {
    matches!(
        event,
        Event::DeviceAdded { .. }
            | Event::DeviceUpdated { .. }
            | Event::DeviceRemoved { .. }
            | Event::EntityAdded { .. }
            | Event::EntityUpdated { .. }
            | Event::EntityRemoved { .. }
            | Event::SettingsChanged
    )
}

/// What a socket is told of an event, if anything. Written with no arm for "the rest", so an
/// event added to the core will not compile until somebody has said who is told of it.
///
/// A person with only some rooms is told when a thing in them changes and that the home has
/// changed shape (the page reads it again, and reads only what is theirs). They are not told
/// what the extensions are doing, or what they have found, or what is waiting to install.
fn frame(event: Event, reach: &irori_core::Reach) -> Option<LiveMessage> {
    match event {
        Event::StateChanged {
            entity_id,
            new_state,
            ..
        } => reach.sees_entity(&entity_id).then_some(LiveMessage::State {
            entity_id,
            state: new_state,
        }),
        Event::ServiceCalled { entity_id, .. } => reach
            .sees_entity(&entity_id)
            .then_some(LiveMessage::Changed),
        Event::DeviceAdded { .. }
        | Event::DeviceUpdated { .. }
        | Event::DeviceRemoved { .. }
        | Event::EntityAdded { .. }
        | Event::EntityUpdated { .. }
        | Event::EntityRemoved { .. }
        | Event::SettingsChanged => Some(LiveMessage::Changed),
        Event::ExtensionStatusChanged { .. }
        | Event::PlaceChanged { .. }
        | Event::FoundChanged
        | Event::InstallsChanged => reach.is_whole().then_some(LiveMessage::Changed),
    }
}

async fn send_snapshot(
    socket: &mut WebSocket,
    state: &AppState,
    reach: &irori_core::Reach,
) -> Result<(), ()> {
    let home = serde_json::to_value(super::snapshot(state, reach)).map_err(|error| {
        tracing::error!(%error, "couldn't write the home as JSON");
    })?;
    send(socket, &LiveMessage::Snapshot { home }).await
}

async fn send(socket: &mut WebSocket, message: &LiveMessage) -> Result<(), ()> {
    let text = serde_json::to_string(message).map_err(|error| {
        tracing::error!(%error, "couldn't write a live message");
    })?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|_| ())
}

/// A browser sends `Origin`. Its host has to be this server's, so another website cannot
/// open the socket. A program sends no `Origin`, and that is still welcome. `null` is a
/// browser hiding its page, which is not this one.
fn same_server(headers: &HeaderMap) -> Result<(), Box<Response>> {
    let Some(value) = headers.get(header::ORIGIN) else {
        return Ok(());
    };
    let Ok(origin) = value.to_str() else {
        return Err(page_only());
    };
    match (origin_host(origin), request_host(headers)) {
        (Some(origin), Some(host)) if origin.eq_ignore_ascii_case(host) => Ok(()),
        _ => Err(page_only()),
    }
}

fn page_only() -> Box<Response> {
    Box::new(refused(
        StatusCode::FORBIDDEN,
        "this socket is only for Irori's own page".to_owned(),
    ))
}

/// The host of an `Origin` value: what follows `://`, up to a path. `null` is none.
fn origin_host(origin: &str) -> Option<&str> {
    if !origin.is_ascii() || origin.eq_ignore_ascii_case("null") {
        return None;
    }
    let host = origin.split_once("://")?.1;
    let host = host.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() { None } else { Some(host) }
}

fn request_host(headers: &HeaderMap) -> Option<&str> {
    let host = headers.get(header::HOST)?.to_str().ok()?;
    if host.is_empty() { None } else { Some(host) }
}

/// One extension's connection. The guard has already required its token.
pub async fn extension(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
    Extension(who): Extension<Actor>,
) -> Response {
    if let Err(response) = same_server(&headers) {
        return *response;
    }
    let Some(id) = who.token.as_ref().and_then(|token| token.extension.clone()) else {
        return refused(
            StatusCode::UNAUTHORIZED,
            "an extension connects with the token made for it".to_owned(),
        );
    };
    let closed = state
        .0
        .sockets
        .open(seat_kind(&state, &who, &headers).await);
    ws.on_upgrade(move |socket| connect(socket, state, id, closed))
}

async fn connect(
    mut socket: WebSocket,
    state: AppState,
    id: ExtensionId,
    closed: watch::Receiver<bool>,
) {
    // 64, the same bound the host uses for a child process's calls.
    let (outgoing, to_socket) = mpsc::channel::<ToExt>(64);
    let (from_socket, incoming) = mpsc::channel::<FromExt>(64);
    if let Err(why) = state
        .0
        .host
        .offer_inbound(&id, irori_core::InboundLink { incoming, outgoing })
    {
        let _ = socket
            .send(Message::Close(Some(CloseFrame {
                code: 1008,
                reason: why.into(),
            })))
            .await;
        return;
    }
    // Reading and writing are separate tasks. One task that awaits a send while it is
    // also the only thing draining the other channel deadlocks once both buffers fill.
    let (write, read) = socket.split();
    let (halt_tx, halt_rx) = mpsc::channel(1);
    tokio::join!(
        read_extension(read, from_socket, halt_tx, closed.clone(), id.clone()),
        write_extension(write, to_socket, halt_rx, closed, id),
    );
}

async fn read_extension(
    mut read: SplitStream<WebSocket>,
    from_socket: mpsc::Sender<FromExt>,
    halt: mpsc::Sender<Option<CloseFrame>>,
    mut closed: watch::Receiver<bool>,
    id: ExtensionId,
) {
    let quiet = tokio::time::sleep(QUIET_FOR);
    tokio::pin!(quiet);
    loop {
        tokio::select! {
            biased;
            changed = closed.changed() => {
                if changed.is_err() || *closed.borrow() {
                    break;
                }
            }
            incoming = read.next() => {
                quiet.as_mut().reset(tokio::time::Instant::now() + QUIET_FOR);
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<FromExt>(&text) {
                            Ok(message) => {
                                if from_socket.send(message).await.is_err() {
                                    break;
                                }
                            }
                            Err(error) => {
                                tracing::warn!(
                                    extension = %id,
                                    %error,
                                    "an extension sent a frame that isn't one of its messages"
                                );
                                let _ = halt.try_send(Some(CloseFrame {
                                    code: 1008,
                                    reason: "that frame isn't one of an extension's messages"
                                        .to_owned()
                                        .into(),
                                }));
                                break;
                            }
                        }
                    }
                    // Protocol traffic. A pong is what resets the quiet timer, above.
                    Some(Ok(Message::Close(_) | Message::Ping(_) | Message::Pong(_))) => {}
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        tracing::debug!(extension = %id, %error, "the extension's connection failed");
                        break;
                    }
                    None => break,
                }
            }
            () = &mut quiet => {
                tracing::info!(extension = %id, "the extension stopped answering");
                break;
            }
        }
    }
    let _ = halt.try_send(None);
}

enum Outgoing {
    Message(Option<ToExt>),
    Ping,
}

async fn write_extension(
    mut write: SplitSink<WebSocket, Message>,
    mut to_socket: mpsc::Receiver<ToExt>,
    mut halt: mpsc::Receiver<Option<CloseFrame>>,
    mut closed: watch::Receiver<bool>,
    id: ExtensionId,
) {
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick is ready immediately. The link just opened; ask again later.
    ping.tick().await;
    loop {
        tokio::select! {
            biased;
            halt = halt.recv() => {
                if let Some(Some(frame)) = halt {
                    let _ = write.send(Message::Close(Some(frame))).await;
                }
                break;
            }
            changed = closed.changed() => {
                if changed.is_err() || *closed.borrow() {
                    break;
                }
            }
            step = next_outgoing(&mut to_socket, &mut ping) => {
                match step {
                    Outgoing::Message(Some(message)) => {
                        let Ok(text) = serde_json::to_string(&message) else {
                            tracing::warn!(extension = %id, "couldn't write a message to the extension");
                            break;
                        };
                        if write.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    Outgoing::Message(None) => break,
                    Outgoing::Ping => {
                        if write.send(Message::Ping(Default::default())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
    }
}

/// The next message for the extension, or a ping. Neither waits behind the other: a
/// program that only listens still has to be asked if it is there.
async fn next_outgoing(
    to_socket: &mut mpsc::Receiver<ToExt>,
    ping: &mut tokio::time::Interval,
) -> Outgoing {
    tokio::select! {
        message = to_socket.recv() => Outgoing::Message(message),
        _ = ping.tick() => Outgoing::Ping,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use irori_types::{Availability, EntityId, EntityState};

    fn state() -> EntityState {
        serde_json::from_value(serde_json::json!({
            "entity_id": "light.hall",
            "availability": "available",
            "state": { "kind": "light", "on": true },
            "last_changed": "2026-01-01T00:00:00Z",
            "last_updated": "2026-01-01T00:00:00Z",
            "last_reported": "2026-01-01T00:00:00Z",
            "context": {
                "id": "01K5B2Q9A1B2C3D4E5F6G7H8J9",
                "origin": { "type": "system" }
            }
        }))
        .expect("a valid state")
    }

    #[test]
    fn a_state_change_names_the_entity_and_anything_else_is_a_nudge() {
        let whole = irori_core::Reach::Whole;
        let entity_id = EntityId::try_from("light.hall").expect("valid");
        let state_frame = frame(
            Event::StateChanged {
                entity_id: entity_id.clone(),
                old_state: None,
                new_state: Box::new(state()),
            },
            &whole,
        );
        assert!(matches!(
            state_frame,
            Some(LiveMessage::State { entity_id: id, state })
                if id == entity_id && state.availability == Availability::Available
        ));
        assert_eq!(
            frame(Event::SettingsChanged, &whole),
            Some(LiveMessage::Changed)
        );
        assert_eq!(
            frame(Event::FoundChanged, &whole),
            Some(LiveMessage::Changed)
        );
    }

    /// A person with only some rooms is told of what is in them and that the home changed
    /// shape, and of nothing the owner's extensions are doing.
    #[test]
    fn a_person_with_rooms_is_told_only_of_theirs() {
        let nobody = irori_core::Reach::nothing();
        let entity_id = EntityId::try_from("light.hall").expect("valid");
        assert_eq!(
            frame(
                Event::StateChanged {
                    entity_id: entity_id.clone(),
                    old_state: None,
                    new_state: Box::new(state()),
                },
                &nobody,
            ),
            None,
            "a change to a thing that isn't theirs is not sent"
        );
        assert_eq!(
            frame(
                Event::ServiceCalled {
                    entity_id,
                    service: irori_types::ServiceName::LightTurnOn,
                    context: state().context,
                },
                &nobody,
            ),
            None
        );
        assert_eq!(
            frame(Event::SettingsChanged, &nobody),
            Some(LiveMessage::Changed),
            "the page reads the home again, and reads only what is theirs"
        );
        assert_eq!(frame(Event::FoundChanged, &nobody), None);
        assert_eq!(frame(Event::InstallsChanged, &nobody), None);
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).expect("a header name"),
                HeaderValue::from_str(value).expect("a header"),
            );
        }
        headers
    }

    #[test]
    fn an_origin_has_to_be_this_server() {
        let here = headers(&[
            ("host", "127.0.0.1:8480"),
            ("origin", "http://127.0.0.1:8480"),
        ]);
        assert!(same_server(&here).is_ok());
        let path = headers(&[
            ("host", "home.local"),
            ("origin", "https://home.local/devices"),
        ]);
        assert!(same_server(&path).is_ok());
        let away = headers(&[
            ("host", "127.0.0.1:8480"),
            ("origin", "http://evil.example"),
        ]);
        assert_eq!(
            same_server(&away).expect_err("another site").status(),
            StatusCode::FORBIDDEN
        );
        let hidden = headers(&[("host", "127.0.0.1:8480"), ("origin", "null")]);
        assert!(same_server(&hidden).is_err());
        assert!(same_server(&headers(&[("host", "127.0.0.1:8480")])).is_ok());
        let mut opaque = headers(&[("host", "127.0.0.1:8480")]);
        opaque.insert(
            header::ORIGIN,
            HeaderValue::from_bytes(&[0xff]).expect("bytes"),
        );
        assert!(same_server(&opaque).is_err());
    }

    #[test]
    fn closing_one_token_leaves_the_other_socket() {
        let sockets = Sockets::default();
        let user = UserId::try_from("nico").expect("valid");
        let tablet = TokenId::try_from("tablet").expect("valid");
        let phone = TokenId::try_from("phone").expect("valid");
        let tablet_socket = sockets.open(SeatKind::Token {
            id: tablet.clone(),
            user: user.clone(),
        });
        let phone_socket = sockets.open(SeatKind::Token {
            id: phone,
            user: user.clone(),
        });
        let browser = sockets.open(SeatKind::Session {
            hash: "keep".to_owned(),
            user: user.clone(),
        });
        sockets.close_token(&tablet);
        assert!(tablet_socket.has_changed().expect("open"));
        assert!(*tablet_socket.borrow());
        assert!(!phone_socket.has_changed().expect("open"));
        assert!(!browser.has_changed().expect("open"));

        let other = sockets.open(SeatKind::Session {
            hash: "other".to_owned(),
            user: user.clone(),
        });
        sockets.close_user_sessions(&user, Some("keep"));
        assert!(!browser.has_changed().expect("open"));
        assert!(other.has_changed().expect("open"));
        assert!(*other.borrow());
    }
}
