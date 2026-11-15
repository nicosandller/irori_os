//! Signing commands the way a car checks them: Tesla's vehicle command protocol, as
//! teslamotors/vehicle-command (Apache-2.0) lays it out in `protocol.md`.
//!
//! A car has two computers that take commands ([`Domain`]), and Irori keeps a session with each:
//! - It asks for one (a handshake). The car answers with its public key, a session id (the
//!   epoch), the last counter it took from Irori's key, and its clock, tagged with a key only
//!   the car and Irori can work out (from ECDH between their two keys).
//! - Each command carries the next counter and an expiry on the car's clock, and an HMAC over
//!   them, the car's VIN and the command.
//! - Home Assistant signs with the same key, so its commands move the counter too. The car then
//!   turns Irori's next one away with a fresh session info, which is checked and taken, and the
//!   command is signed again ([`Reply::Resync`]).
//!
//! Commands are signed, not encrypted: they pass through Tesla's servers, which read them to
//! check them against what the sign-in allows, as the Fleet API requires.
//!
//! The car's key is trusted the first time it's seen and held to after that, for as long as
//! Irori runs: an answer naming another key is turned away.

pub mod proto;

use hmac::{Hmac, KeyInit, Mac};
use p256::{PublicKey, SecretKey};
use prost::Message;
use sha2::Sha256;
use tokio::time::Instant;

pub use proto::Domain;
use proto::{
    Destination, FromVcsecMessage, HmacPersonalizedSignatureData, KeyIdentity, Response,
    RoutableMessage, SessionInfo, SessionInfoRequest, SignatureData, from_vcsec, key_identity,
    result_reason, routable, signature_data,
};

/// How long a signed command stays good, in seconds on the car's clock.
const LIFETIME: u32 = 10;

/// The tags of the values a signature covers.
mod tag {
    pub const SIGNATURE_TYPE: u8 = 0;
    pub const DOMAIN: u8 = 1;
    pub const PERSONALIZATION: u8 = 2;
    pub const EPOCH: u8 = 3;
    pub const EXPIRES_AT: u8 = 4;
    pub const COUNTER: u8 = 5;
    pub const CHALLENGE: u8 = 6;
    pub const END: u8 = 0xff;
}

/// `SignatureType`: how a tag was made.
mod signature {
    /// The car's tag on a session info.
    pub const HMAC: u8 = 6;
    /// A command's tag.
    pub const HMAC_PERSONALIZED: u8 = 8;
}

/// `Session_Info_Status`: the key isn't on the car.
const KEY_NOT_ON_WHITELIST: i32 = 1;

/// The values a tag covers, laid out as the protocol says: tag, length, value, in rising tag
/// order, then `0xFF`.
#[derive(Default)]
struct Metadata(Vec<u8>);

impl Metadata {
    fn add(mut self, tag: u8, value: &[u8]) -> Self {
        let length = u8::try_from(value.len()).expect("metadata values are short");
        self.0.push(tag);
        self.0.push(length);
        self.0.extend_from_slice(value);
        self
    }

    fn finish(mut self) -> Vec<u8> {
        self.0.push(tag::END);
        self.0
    }
}

fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC takes any key");
    for part in parts {
        mac.update(part);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&mac.finalize().into_bytes());
    out
}

fn tag_matches(key: &[u8], parts: &[&[u8]], tag: &[u8]) -> bool {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC takes any key");
    for part in parts {
        mac.update(part);
    }
    mac.verify_slice(tag).is_ok()
}

/// The secret Irori and the car share: SHA-1 of the ECDH point's x coordinate, cut to 16 bytes.
fn shared(local: &SecretKey, vehicle: &[u8]) -> Option<[u8; 16]> {
    use sha1::Digest as _;
    let public = PublicKey::from_sec1_bytes(vehicle).ok()?;
    let point = p256::ecdh::diffie_hellman(local.to_nonzero_scalar(), public.as_affine());
    let digest = sha1::Sha1::digest(point.raw_secret_bytes());
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    Some(out)
}

#[derive(Clone)]
struct Keys {
    /// Signs commands.
    command: [u8; 32],
    /// Checks the car's session infos.
    session_info: [u8; 32],
}

impl Keys {
    fn agree(local: &SecretKey, vehicle: &[u8]) -> Option<Self> {
        let shared = shared(local, vehicle)?;
        Some(Self {
            command: hmac(&shared, &[b"authenticated command"]),
            session_info: hmac(&shared, &[b"session info"]),
        })
    }
}

/// A key's public half as the protocol sends it: 65 bytes, uncompressed.
pub fn public_bytes(key: &SecretKey) -> Vec<u8> {
    use p256::elliptic_curve::sec1::ToEncodedPoint as _;
    key.public_key().to_encoded_point(false).as_bytes().to_vec()
}

fn random() -> Result<Vec<u8>, String> {
    let mut bytes = vec![0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| format!("no randomness: {error}"))?;
    Ok(bytes)
}

#[derive(Clone)]
struct Session {
    keys: Keys,
    epoch: Vec<u8>,
    /// The last counter used: the next command takes the one after.
    counter: u32,
    /// The car's clock, in seconds, when the session info was taken in.
    clock: u32,
    seen: Instant,
}

/// A message on its way, kept until its answer comes: the answer is checked against its `uuid`.
#[derive(Debug, Clone)]
pub struct Sent {
    pub message: RoutableMessage,
    pub uuid: Vec<u8>,
}

/// What the car said to a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// It took the message: what the domain answered (empty for a handshake).
    Answer(Vec<u8>),
    /// The session was out of step (another app used the key, the car restarted): sign the
    /// command again.
    Resync,
    /// It's busy: send it again shortly.
    Busy,
    /// Irori's key isn't on the car.
    NotPaired,
    /// It refused, in words.
    Refused(String),
}

/// One computer of one car, as Irori talks to it.
#[derive(Default)]
pub struct Channel {
    /// The car's public key, as first seen.
    pinned: Option<Vec<u8>>,
    session: Option<Session>,
}

impl Channel {
    pub fn ready(&self) -> bool {
        self.session.is_some()
    }

    /// Asks the car for a session.
    pub fn hello(domain: Domain, from: &[u8], local: &SecretKey) -> Result<Sent, String> {
        let uuid = random()?;
        let message = RoutableMessage {
            to_destination: Some(Destination::domain(domain)),
            from_destination: Some(Destination::address(from.to_vec())),
            payload: Some(routable::Payload::SessionInfoRequest(SessionInfoRequest {
                public_key: public_bytes(local),
                challenge: Vec::new(),
            })),
            uuid: uuid.clone(),
            ..RoutableMessage::default()
        };
        Ok(Sent { message, uuid })
    }

    /// Signs a command with the session's next counter. `None` without a session.
    pub fn sign(
        &mut self,
        domain: Domain,
        vin: &str,
        from: &[u8],
        local: &SecretKey,
        command: Vec<u8>,
        now: Instant,
    ) -> Result<Option<Sent>, String> {
        let Some(session) = self.session.as_mut() else {
            return Ok(None);
        };
        let Some(counter) = session.counter.checked_add(1) else {
            // Four billion commands in one session: start a new one.
            self.session = None;
            return Ok(None);
        };
        session.counter = counter;
        let elapsed = u32::try_from(now.saturating_duration_since(session.seen).as_secs())
            .unwrap_or(u32::MAX);
        let expires_at = session
            .clock
            .saturating_add(elapsed)
            .saturating_add(LIFETIME);
        let tag = command_tag(
            &session.keys.command,
            domain,
            vin,
            &session.epoch,
            expires_at,
            counter,
            &command,
        );
        let uuid = random()?;
        let message = RoutableMessage {
            to_destination: Some(Destination::domain(domain)),
            from_destination: Some(Destination::address(from.to_vec())),
            payload: Some(routable::Payload::Command(command)),
            signature: Some(routable::Signature::Data(SignatureData {
                signer_identity: Some(KeyIdentity {
                    kind: Some(key_identity::Kind::PublicKey(public_bytes(local))),
                }),
                kind: Some(signature_data::Kind::HmacPersonalized(
                    HmacPersonalizedSignatureData {
                        epoch: session.epoch.clone(),
                        counter,
                        expires_at,
                        tag: tag.to_vec(),
                    },
                )),
            })),
            uuid: uuid.clone(),
            ..RoutableMessage::default()
        };
        Ok(Some(Sent { message, uuid }))
    }

    /// Makes sense of the car's answer to `sent`, taking in any session info it carries.
    pub fn read(
        &mut self,
        vin: &str,
        sent: &Sent,
        reply: &RoutableMessage,
        local: &SecretKey,
        now: Instant,
    ) -> Reply {
        // The car may leave the echo out (VCSEC often does), but never names another message.
        if !reply.request_uuid.is_empty() && reply.request_uuid != sent.uuid {
            return Reply::Refused("the car answered another message".into());
        }
        let mut took_info = false;
        if let Some(routable::Payload::SessionInfo(info)) = &reply.payload {
            let tag = match &reply.signature {
                Some(routable::Signature::Data(SignatureData {
                    kind: Some(signature_data::Kind::SessionInfoTag(tag)),
                    ..
                })) => Some(tag.tag.as_slice()),
                _ => None,
            };
            match self.take(vin, &sent.uuid, info, tag, local, now) {
                Ok(()) => took_info = true,
                Err(reply) => return reply,
            }
        }
        let status = reply.signed_message_status.clone().unwrap_or_default();
        match status.signed_message_fault {
            0 => {}
            // A bad signature, a used counter, another session, an expired command: the session
            // is out of step. The car sends its session info along; without it, start a new one.
            5 | 6 | 15 | 17 => {
                if !took_info {
                    self.session = None;
                }
                return Reply::Resync;
            }
            // Unknown or disabled key.
            3 | 4 => return Reply::NotPaired,
            // Busy, timed out, still starting up.
            1 | 2 | 11 => return Reply::Busy,
            fault => return Reply::Refused(fault_words(fault)),
        }
        if status.operation_status == 1 {
            return Reply::Busy;
        }
        match &reply.payload {
            Some(routable::Payload::Command(answer)) => Reply::Answer(answer.clone()),
            _ => Reply::Answer(Vec::new()),
        }
    }

    /// Checks a session info's tag and takes it in.
    fn take(
        &mut self,
        vin: &str,
        challenge: &[u8],
        bytes: &[u8],
        tag: Option<&[u8]>,
        local: &SecretKey,
        now: Instant,
    ) -> Result<(), Reply> {
        let refused = |why: &str| Reply::Refused(why.to_owned());
        let info =
            SessionInfo::decode(bytes).map_err(|_| refused("the car's answer is garbled"))?;
        if info.public_key.is_empty() {
            // The car has no session for a key it doesn't know, so nothing to tag it with.
            return Err(if info.status == KEY_NOT_ON_WHITELIST {
                Reply::NotPaired
            } else {
                refused("the car's answer has no key in it")
            });
        }
        if self
            .pinned
            .as_ref()
            .is_some_and(|pinned| *pinned != info.public_key)
        {
            return Err(refused(
                "the car answered with a different key from before, so Irori won't trust it until \
                 it restarts",
            ));
        }
        let keys = Keys::agree(local, &info.public_key)
            .ok_or_else(|| refused("the car's key isn't a P-256 key"))?;
        let metadata = session_info_metadata(vin, challenge);
        let tag = tag.ok_or_else(|| refused("the car's answer isn't tagged"))?;
        if !tag_matches(&keys.session_info, &[&metadata, bytes], tag) {
            return Err(refused("the car's answer didn't check out"));
        }
        if info.status == KEY_NOT_ON_WHITELIST {
            return Err(Reply::NotPaired);
        }
        let counter = match &self.session {
            Some(session) if session.epoch == info.epoch => {
                // An older answer, replayed: its clock went backwards.
                if info.clock_time < session.clock {
                    return Err(refused("the car's answer is older than the last one"));
                }
                // The counter never goes back within a session.
                session.counter.max(info.counter)
            }
            _ => info.counter,
        };
        self.pinned = Some(info.public_key);
        self.session = Some(Session {
            keys,
            epoch: info.epoch,
            counter,
            clock: info.clock_time,
            seen: now,
        });
        Ok(())
    }
}

fn session_info_metadata(vin: &str, challenge: &[u8]) -> Vec<u8> {
    Metadata::default()
        .add(tag::SIGNATURE_TYPE, &[signature::HMAC])
        .add(tag::PERSONALIZATION, vin.as_bytes())
        .add(tag::CHALLENGE, challenge)
        .finish()
}

fn command_tag(
    key: &[u8],
    domain: Domain,
    vin: &str,
    epoch: &[u8],
    expires_at: u32,
    counter: u32,
    command: &[u8],
) -> [u8; 32] {
    let metadata = Metadata::default()
        .add(tag::SIGNATURE_TYPE, &[signature::HMAC_PERSONALIZED])
        .add(tag::DOMAIN, &[domain as u8])
        .add(tag::PERSONALIZATION, vin.as_bytes())
        .add(tag::EPOCH, epoch)
        .add(tag::EXPIRES_AT, &expires_at.to_be_bytes())
        .add(tag::COUNTER, &counter.to_be_bytes())
        .finish();
    hmac(key, &[&metadata, command])
}

/// `MessageFault_E`, for a person.
fn fault_words(fault: i32) -> String {
    match fault {
        7 => "the key on the car isn't allowed to do this".into(),
        8 | 9 | 10 | 13 => "the car doesn't know this command (its software may be too old)".into(),
        12 => "the command was for another car".into(),
        21 => "Mobile Access is turned off in the car (Controls → Safety)".into(),
        fault => format!("the car refused it (fault {fault})"),
    }
}

/// What a domain's answer means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Busy,
    NotPaired,
    Refused(String),
}

/// Reads a domain's answer to a command. `done_anyway` are the car's reasons that mean nothing
/// had to happen ("already_set"; "is_charging" for start charging).
pub fn outcome(domain: Domain, answer: &[u8], done_anyway: &[&str]) -> Outcome {
    if answer.is_empty() {
        return Outcome::Done;
    }
    match domain {
        Domain::VehicleSecurity => {
            let Ok(message) = FromVcsecMessage::decode(answer) else {
                return Outcome::Refused("the car's answer is garbled".into());
            };
            match message.sub {
                None => Outcome::Done,
                Some(from_vcsec::Sub::CommandStatus(status)) => match status.operation_status {
                    0 => Outcome::Done,
                    1 => Outcome::Busy,
                    _ => match status.signed_message_status.map(|s| s.information) {
                        // FAULT_NOT_ON_WHITELIST
                        Some(2) => Outcome::NotPaired,
                        _ => Outcome::Refused("the car refused it".into()),
                    },
                },
                Some(from_vcsec::Sub::NominalError(error)) => match error.generic_error {
                    0 | 3 => Outcome::Done,
                    2 => Outcome::Refused("a door, the trunk or the frunk is open".into()),
                    5 => Outcome::Refused("the car isn't in park".into()),
                    4 | 6 => Outcome::Refused("the car doesn't allow this from an app".into()),
                    7 => Outcome::Refused("the car only allows this from a phone key".into()),
                    _ => Outcome::Refused("the car refused it".into()),
                },
            }
        }
        _ => {
            let Ok(response) = Response::decode(answer) else {
                return Outcome::Refused("the car's answer is garbled".into());
            };
            let Some(status) = response.action_status else {
                return Outcome::Done;
            };
            if status.result == 0 {
                return Outcome::Done;
            }
            let reason = match status.result_reason.and_then(|reason| reason.kind) {
                Some(result_reason::Kind::PlainText(text)) => text,
                None => String::new(),
            };
            if reason == "already_set" || done_anyway.contains(&reason.as_str()) {
                Outcome::Done
            } else if reason.is_empty() {
                Outcome::Refused("the car refused it".into())
            } else {
                Outcome::Refused(format!("the car said “{}”", reason.replace('_', " ")))
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! The golden values are `protocol.md`'s worked example, from teslamotors/vehicle-command.
    //! The command tag, which `protocol.md` doesn't work out, was computed apart from this code
    //! with Python's `hmac` from the same keys.

    use super::*;
    use crate::keys::Key;

    pub const VIN: &str = "5YJ30123456789ABC";

    pub const VEHICLE: &str = "-----BEGIN EC PRIVATE KEY-----
MHcCAQEEIDRO5bRmp88e6xK29QMx2y5exYNO9fS+/P2MvlXCUo1woAoGCCqGSM49
AwEHoUQDQgAEx6H0cThIaqRymXFJSHjTOxok45Vx90im4WxZVbPYd9OmqqDpVRZk
dK9dMsQQ9DmiI0E3rRuwhf1OiBPJWPEdlw==
-----END EC PRIVATE KEY-----";

    pub const CLIENT: &str = "-----BEGIN EC PRIVATE KEY-----
MHcCAQEEICU4zcKal8GcHpmmN9bPT4yXDBGLVu3h5jI+bRYsSzDboAoGCCqGSM49
AwEHoUQDQgAEsra8aMLaBmXOZWgVWUmWxiOU7di+qQX+eBp1T+aoRacUMwkC8iXp
Jp1GbgWzSZgf2p2FzCPG+0RKpztikQXcbg==
-----END EC PRIVATE KEY-----";

    const SESSION_INFO: &str = "0806124104c7a1f47138486aa4729971494878d33b1a24e39571f748a6e16c5955b3d877d3a6aaa0e955166474af5d32c410f439a2234137ad1bb085fd4e8813c958f11d971a104c463f9cc0d3d26906e982ed224adde6255a0a0000";
    const UUID: &str = "1588d5a30eabc6f8fc9a951b11f6fd11";
    const TAG: &str = "996c1fe38331be138f8039c194b14db2198846ed7d8251e6749284d7b32ea002";

    pub fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex"))
            .collect()
    }

    pub fn to_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn secret(pem: &str) -> SecretKey {
        Key::from_pasted(pem).expect("a key").secret().clone()
    }

    fn tagged(info: Vec<u8>, tag: Vec<u8>, request: &[u8]) -> RoutableMessage {
        RoutableMessage {
            payload: Some(routable::Payload::SessionInfo(info)),
            signature: Some(routable::Signature::Data(SignatureData {
                signer_identity: None,
                kind: Some(signature_data::Kind::SessionInfoTag(
                    proto::HmacSignatureData { tag },
                )),
            })),
            request_uuid: request.to_vec(),
            ..RoutableMessage::default()
        }
    }

    #[test]
    fn keys_and_tags_match_tesla_s_worked_example() {
        let client = secret(CLIENT);
        let vehicle = secret(VEHICLE);
        assert_eq!(
            to_hex(&public_bytes(&vehicle)),
            "04c7a1f47138486aa4729971494878d33b1a24e39571f748a6e16c5955b3d877d3a6aaa0e955166474af5d32c410f439a2234137ad1bb085fd4e8813c958f11d97"
        );
        let shared = shared(&client, &public_bytes(&vehicle)).expect("agreed");
        assert_eq!(to_hex(&shared), "1b2fce19967b79db696f909cff89ea9a");
        // Both ends work out the same secret.
        assert_eq!(
            Some(shared),
            super::shared(&vehicle, &public_bytes(&client))
        );
        let keys = Keys::agree(&client, &public_bytes(&vehicle)).expect("keys");
        assert_eq!(
            to_hex(&keys.session_info),
            "fceb679ee7bca756fcd441bf238bf2f338629b41d9eb9c67be1b32c9672ce300"
        );
        let metadata = session_info_metadata(VIN, &hex(UUID));
        assert_eq!(
            to_hex(&metadata),
            "000106021135594a333031323334353637383941424306101588d5a30eabc6f8fc9a951b11f6fd11ff"
        );
        assert_eq!(
            to_hex(&hmac(&keys.session_info, &[&metadata, &hex(SESSION_INFO)])),
            TAG
        );
    }

    #[test]
    fn a_session_info_is_checked_then_taken_and_commands_are_signed_with_it() {
        let client = secret(CLIENT);
        let now = Instant::now();
        let sent = Sent {
            message: RoutableMessage::default(),
            uuid: hex(UUID),
        };
        let reply = tagged(hex(SESSION_INFO), hex(TAG), &[]);

        // A tag that doesn't match is turned away, and nothing is taken.
        let mut channel = Channel::default();
        let mut wrong = hex(TAG);
        wrong[0] ^= 1;
        assert!(matches!(
            channel.read(
                VIN,
                &sent,
                &tagged(hex(SESSION_INFO), wrong, &[]),
                &client,
                now
            ),
            Reply::Refused(_)
        ));
        assert!(!channel.ready());
        // So is a right tag for another challenge.
        let other = Sent {
            message: RoutableMessage::default(),
            uuid: vec![0; 16],
        };
        assert!(matches!(
            channel.read(VIN, &other, &reply, &client, now),
            Reply::Refused(_)
        ));

        assert_eq!(
            channel.read(VIN, &sent, &reply, &client, now),
            Reply::Answer(Vec::new())
        );
        assert!(channel.ready());
        let session = channel.session.clone().expect("a session");
        assert_eq!(session.counter, 6);
        assert_eq!(to_hex(&session.epoch), "4c463f9cc0d3d26906e982ed224adde6");
        assert_eq!(session.clock, 2650);

        // "Turn on the climate", signed with the next counter, good for ten seconds.
        let command = hex("120452020801");
        let from = vec![0xab; 16];
        let signed = channel
            .sign(
                Domain::Infotainment,
                VIN,
                &from,
                &client,
                command.clone(),
                now,
            )
            .expect("signed")
            .expect("a session");
        let Some(routable::Signature::Data(data)) = &signed.message.signature else {
            panic!("no signature");
        };
        let Some(signature_data::Kind::HmacPersonalized(hmac_data)) = &data.kind else {
            panic!("not an HMAC signature");
        };
        assert_eq!(hmac_data.counter, 7);
        assert_eq!(hmac_data.expires_at, 2660);
        assert_eq!(
            to_hex(&hmac_data.tag),
            "948ecc36a77deb932bd184c663bafe076a6c13e205e4fd8a16f0118496071eaf"
        );
        assert_eq!(
            data.signer_identity,
            Some(KeyIdentity {
                kind: Some(key_identity::Kind::PublicKey(public_bytes(&client)))
            })
        );
        assert_eq!(
            signed.message.payload,
            Some(routable::Payload::Command(command))
        );
        assert_eq!(signed.message.uuid, signed.uuid);
    }

    #[test]
    fn the_counter_only_goes_forward_and_a_replayed_answer_is_refused() {
        let client = secret(CLIENT);
        let vehicle = secret(VEHICLE);
        let keys = Keys::agree(&vehicle, &public_bytes(&client)).expect("keys");
        let now = Instant::now();
        let epoch = vec![7u8; 16];
        let answer = |counter: u32, clock: u32, epoch: &[u8], uuid: &[u8]| {
            let info = SessionInfo {
                counter,
                public_key: public_bytes(&vehicle),
                epoch: epoch.to_vec(),
                clock_time: clock,
                ..SessionInfo::default()
            }
            .encode_to_vec();
            let tag = hmac(
                &keys.session_info,
                &[&session_info_metadata(VIN, uuid), &info],
            );
            tagged(info, tag.to_vec(), uuid)
        };
        let sent = |uuid: u8| Sent {
            message: RoutableMessage::default(),
            uuid: vec![uuid; 16],
        };
        let counter = |channel: &Channel| channel.session.as_ref().map(|s| s.counter);

        let mut channel = Channel::default();
        channel.read(
            VIN,
            &sent(1),
            &answer(40, 100, &epoch, &[1; 16]),
            &client,
            now,
        );
        assert_eq!(counter(&channel), Some(40));
        // Home Assistant sent ten commands with the same key: the car says 50 and Irori follows.
        channel.read(
            VIN,
            &sent(2),
            &answer(50, 110, &epoch, &[2; 16]),
            &client,
            now,
        );
        assert_eq!(counter(&channel), Some(50));
        // A lower counter in the same session never takes Irori back.
        channel.read(
            VIN,
            &sent(3),
            &answer(45, 120, &epoch, &[3; 16]),
            &client,
            now,
        );
        assert_eq!(counter(&channel), Some(50));
        // An answer from before (its clock went back) is turned away.
        assert!(matches!(
            channel.read(
                VIN,
                &sent(4),
                &answer(60, 90, &epoch, &[4; 16]),
                &client,
                now
            ),
            Reply::Refused(_)
        ));
        assert_eq!(counter(&channel), Some(50));
        // A new session (the car restarted) starts where the car says.
        channel.read(
            VIN,
            &sent(5),
            &answer(3, 5, &[8; 16], &[5; 16]),
            &client,
            now,
        );
        assert_eq!(counter(&channel), Some(3));
        // An answer echoing another message is turned away.
        assert!(matches!(
            channel.read(
                VIN,
                &sent(6),
                &answer(9, 9, &[8; 16], &[7; 16]),
                &client,
                now
            ),
            Reply::Refused(_)
        ));
    }

    #[test]
    fn a_car_without_irori_s_key_says_so_and_another_key_is_never_trusted() {
        let client = secret(CLIENT);
        let now = Instant::now();
        let sent = Sent {
            message: RoutableMessage::default(),
            uuid: vec![1; 16],
        };
        let unpaired = SessionInfo {
            status: KEY_NOT_ON_WHITELIST,
            ..SessionInfo::default()
        };
        let mut channel = Channel::default();
        assert_eq!(
            channel.read(
                VIN,
                &sent,
                &tagged(unpaired.encode_to_vec(), Vec::new(), &[]),
                &client,
                now
            ),
            Reply::NotPaired
        );

        // Trusted on first sight, then held to.
        let reply = tagged(hex(SESSION_INFO), hex(TAG), &[]);
        let first = Sent {
            message: RoutableMessage::default(),
            uuid: hex(UUID),
        };
        channel.read(VIN, &first, &reply, &client, now);
        assert!(channel.ready());
        let impostor = Key::generate().expect("a key").secret().clone();
        let keys = Keys::agree(&impostor, &public_bytes(&client)).expect("keys");
        let info = SessionInfo {
            counter: 1,
            public_key: public_bytes(&impostor),
            epoch: vec![9; 16],
            clock_time: 1,
            ..SessionInfo::default()
        }
        .encode_to_vec();
        let tag = hmac(
            &keys.session_info,
            &[&session_info_metadata(VIN, &[1; 16]), &info],
        );
        assert!(matches!(
            channel.read(VIN, &sent, &tagged(info, tag.to_vec(), &[]), &client, now),
            Reply::Refused(_)
        ));
    }

    #[test]
    fn faults_and_answers_read_as_what_to_do_next() {
        let client = secret(CLIENT);
        let now = Instant::now();
        let sent = Sent {
            message: RoutableMessage::default(),
            uuid: vec![1; 16],
        };
        let fault = |fault: i32, operation: i32| RoutableMessage {
            signed_message_status: Some(proto::MessageStatus {
                operation_status: operation,
                signed_message_fault: fault,
            }),
            ..RoutableMessage::default()
        };
        let mut channel = Channel::default();
        assert_eq!(
            channel.read(VIN, &sent, &fault(6, 2), &client, now),
            Reply::Resync
        );
        assert_eq!(
            channel.read(VIN, &sent, &fault(15, 2), &client, now),
            Reply::Resync
        );
        assert_eq!(
            channel.read(VIN, &sent, &fault(3, 2), &client, now),
            Reply::NotPaired
        );
        assert_eq!(
            channel.read(VIN, &sent, &fault(0, 1), &client, now),
            Reply::Busy
        );
        assert!(matches!(
            channel.read(VIN, &sent, &fault(21, 2), &client, now),
            Reply::Refused(why) if why.contains("Mobile Access")
        ));

        let vcsec = |sub| FromVcsecMessage { sub: Some(sub) }.encode_to_vec();
        let ok = vcsec(from_vcsec::Sub::CommandStatus(proto::CommandStatus {
            operation_status: 0,
            signed_message_status: None,
        }));
        assert_eq!(outcome(Domain::VehicleSecurity, &ok, &[]), Outcome::Done);
        let in_drive = vcsec(from_vcsec::Sub::NominalError(proto::NominalError {
            generic_error: 5,
        }));
        assert!(matches!(
            outcome(Domain::VehicleSecurity, &in_drive, &[]),
            Outcome::Refused(why) if why.contains("park")
        ));

        let said = |reason: &str| {
            Response {
                action_status: Some(proto::ActionStatus {
                    result: 1,
                    result_reason: Some(proto::ResultReason {
                        kind: Some(result_reason::Kind::PlainText(reason.into())),
                    }),
                }),
            }
            .encode_to_vec()
        };
        assert_eq!(
            outcome(Domain::Infotainment, &said("already_set"), &[]),
            Outcome::Done
        );
        assert_eq!(
            outcome(Domain::Infotainment, &said("is_charging"), &["is_charging"]),
            Outcome::Done
        );
        assert_eq!(
            outcome(
                Domain::Infotainment,
                &said("cabin_comfort_remote_settings_not_enabled"),
                &[]
            ),
            Outcome::Refused("the car said “cabin comfort remote settings not enabled”".into())
        );
    }
}
