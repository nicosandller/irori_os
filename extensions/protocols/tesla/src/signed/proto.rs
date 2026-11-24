//! The few messages of Tesla's vehicle command protocol Irori sends and reads, written out from
//! teslamotors/vehicle-command's `.proto` files (Apache-2.0) rather than generated: field numbers
//! and types as there, only the fields Irori uses, names in Rust's case.
//!
//! Every `oneof` is a [`prost::Oneof`], never a plain field: a oneof member holding its type's
//! zero (unlock is `RKEAction = 0`) is still written, where a plain proto3 field would be left
//! out and the car would get an empty message.

#![allow(clippy::enum_variant_names)]

/// Which of the car's computers a message is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum Domain {
    Broadcast = 0,
    /// VCSEC: locks, closures, keys. Answers while the rest of the car sleeps.
    VehicleSecurity = 2,
    /// The car's main computer: climate, charging, everything else.
    Infotainment = 3,
}

// ---- universal_message.proto ----

#[derive(Clone, PartialEq, prost::Message)]
pub struct Destination {
    #[prost(oneof = "destination::Sub", tags = "1, 2")]
    pub sub: Option<destination::Sub>,
}

pub mod destination {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Sub {
        #[prost(enumeration = "super::Domain", tag = "1")]
        Domain(i32),
        #[prost(bytes, tag = "2")]
        RoutingAddress(Vec<u8>),
    }
}

impl Destination {
    pub fn domain(domain: Domain) -> Self {
        Self {
            sub: Some(destination::Sub::Domain(domain as i32)),
        }
    }

    pub fn address(address: Vec<u8>) -> Self {
        Self {
            sub: Some(destination::Sub::RoutingAddress(address)),
        }
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct MessageStatus {
    /// 0 OK, 1 WAIT, 2 ERROR.
    #[prost(int32, tag = "1")]
    pub operation_status: i32,
    /// `MessageFault_E`; 0 is none.
    #[prost(int32, tag = "2")]
    pub signed_message_fault: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SessionInfoRequest {
    #[prost(bytes, tag = "1")]
    pub public_key: Vec<u8>,
    #[prost(bytes, tag = "2")]
    pub challenge: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RoutableMessage {
    #[prost(message, optional, tag = "6")]
    pub to_destination: Option<Destination>,
    #[prost(message, optional, tag = "7")]
    pub from_destination: Option<Destination>,
    #[prost(oneof = "routable::Payload", tags = "10, 14, 15")]
    pub payload: Option<routable::Payload>,
    #[prost(message, optional, tag = "12")]
    pub signed_message_status: Option<MessageStatus>,
    #[prost(oneof = "routable::Signature", tags = "13")]
    pub signature: Option<routable::Signature>,
    #[prost(bytes, tag = "50")]
    pub request_uuid: Vec<u8>,
    #[prost(bytes, tag = "51")]
    pub uuid: Vec<u8>,
    #[prost(uint32, tag = "52")]
    pub flags: u32,
}

pub mod routable {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Payload {
        /// A command, as the domain's own message, encoded.
        #[prost(bytes, tag = "10")]
        Command(Vec<u8>),
        #[prost(message, tag = "14")]
        SessionInfoRequest(super::SessionInfoRequest),
        /// A [`super::SessionInfo`], encoded: its tag covers these exact bytes.
        #[prost(bytes, tag = "15")]
        SessionInfo(Vec<u8>),
    }

    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Signature {
        #[prost(message, tag = "13")]
        Data(super::SignatureData),
    }
}

// ---- signatures.proto ----

#[derive(Clone, PartialEq, prost::Message)]
pub struct KeyIdentity {
    #[prost(oneof = "key_identity::Kind", tags = "1")]
    pub kind: Option<key_identity::Kind>,
}

pub mod key_identity {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Kind {
        #[prost(bytes, tag = "1")]
        PublicKey(Vec<u8>),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct HmacSignatureData {
    #[prost(bytes, tag = "1")]
    pub tag: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct HmacPersonalizedSignatureData {
    #[prost(bytes, tag = "1")]
    pub epoch: Vec<u8>,
    #[prost(uint32, tag = "2")]
    pub counter: u32,
    #[prost(fixed32, tag = "3")]
    pub expires_at: u32,
    #[prost(bytes, tag = "4")]
    pub tag: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SignatureData {
    #[prost(message, optional, tag = "1")]
    pub signer_identity: Option<KeyIdentity>,
    #[prost(oneof = "signature_data::Kind", tags = "6, 8")]
    pub kind: Option<signature_data::Kind>,
}

pub mod signature_data {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Kind {
        /// The car's tag on a session info it sends.
        #[prost(message, tag = "6")]
        SessionInfoTag(super::HmacSignatureData),
        /// A command signed with the session's key.
        #[prost(message, tag = "8")]
        HmacPersonalized(super::HmacPersonalizedSignatureData),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SessionInfo {
    #[prost(uint32, tag = "1")]
    pub counter: u32,
    #[prost(bytes, tag = "2")]
    pub public_key: Vec<u8>,
    #[prost(bytes, tag = "3")]
    pub epoch: Vec<u8>,
    #[prost(fixed32, tag = "4")]
    pub clock_time: u32,
    /// 0 OK, 1 KEY_NOT_ON_WHITELIST.
    #[prost(int32, tag = "5")]
    pub status: i32,
    #[prost(uint32, tag = "6")]
    pub handle: u32,
    #[prost(uint32, tag = "7")]
    pub flags: u32,
}

// ---- vcsec.proto ----

/// `ClosureMoveType_E`.
pub mod closure {
    pub const MOVE: i32 = 1;
    pub const OPEN: i32 = 3;
    pub const CLOSE: i32 = 4;
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ClosureMoveRequest {
    #[prost(int32, tag = "5")]
    pub rear_trunk: i32,
    #[prost(int32, tag = "6")]
    pub front_trunk: i32,
    #[prost(int32, tag = "7")]
    pub charge_port: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct UnsignedMessage {
    #[prost(oneof = "unsigned_message::Sub", tags = "2, 4")]
    pub sub: Option<unsigned_message::Sub>,
}

pub mod unsigned_message {
    /// `RKEAction_E`.
    pub const UNLOCK: i32 = 0;
    pub const LOCK: i32 = 1;

    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Sub {
        #[prost(int32, tag = "2")]
        RkeAction(i32),
        #[prost(message, tag = "4")]
        ClosureMoveRequest(super::ClosureMoveRequest),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SignedMessageStatus {
    #[prost(uint32, tag = "1")]
    pub counter: u32,
    #[prost(int32, tag = "2")]
    pub information: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct CommandStatus {
    /// 0 OK, 1 WAIT, 2 ERROR.
    #[prost(int32, tag = "1")]
    pub operation_status: i32,
    #[prost(message, optional, tag = "2")]
    pub signed_message_status: Option<SignedMessageStatus>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct NominalError {
    /// `GenericError_E`.
    #[prost(int32, tag = "1")]
    pub generic_error: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct FromVcsecMessage {
    #[prost(oneof = "from_vcsec::Sub", tags = "4, 46")]
    pub sub: Option<from_vcsec::Sub>,
}

pub mod from_vcsec {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Sub {
        #[prost(message, tag = "4")]
        CommandStatus(super::CommandStatus),
        #[prost(message, tag = "46")]
        NominalError(super::NominalError),
    }
}

// ---- car_server.proto ----

#[derive(Clone, Copy, PartialEq, Eq, prost::Message)]
pub struct Void {}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Action {
    #[prost(oneof = "action::Msg", tags = "2")]
    pub msg: Option<action::Msg>,
}

pub mod action {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Msg {
        #[prost(message, tag = "2")]
        VehicleAction(super::VehicleAction),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VehicleAction {
    #[prost(
        oneof = "vehicle_action::Msg",
        tags = "5, 6, 10, 12, 13, 14, 26, 27, 30, 34, 36, 43, 44"
    )]
    pub msg: Option<vehicle_action::Msg>,
}

pub mod vehicle_action {
    use super::*;

    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Msg {
        #[prost(message, tag = "5")]
        ChargingSetLimit(ChargingSetLimitAction),
        #[prost(message, tag = "6")]
        ChargingStartStop(ChargingStartStopAction),
        #[prost(message, tag = "10")]
        HvacAuto(HvacAutoAction),
        #[prost(message, tag = "12")]
        HvacSetPreconditioningMax(HvacSetPreconditioningMaxAction),
        #[prost(message, tag = "13")]
        HvacSteeringWheelHeater(HvacSteeringWheelHeaterAction),
        #[prost(message, tag = "14")]
        HvacTemperatureAdjustment(HvacTemperatureAdjustmentAction),
        #[prost(message, tag = "26")]
        FlashLights(Void),
        #[prost(message, tag = "27")]
        HonkHorn(Void),
        #[prost(message, tag = "30")]
        SetSentryMode(VehicleControlSetSentryModeAction),
        #[prost(message, tag = "34")]
        Window(VehicleControlWindowAction),
        #[prost(message, tag = "36")]
        HvacSeatHeater(HvacSeatHeaterActions),
        #[prost(message, tag = "43")]
        SetChargingAmps(SetChargingAmpsAction),
        #[prost(message, tag = "44")]
        HvacClimateKeeper(HvacClimateKeeperAction),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ChargingSetLimitAction {
    #[prost(int32, tag = "1")]
    pub percent: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ChargingStartStopAction {
    #[prost(oneof = "charging_start_stop::Kind", tags = "2, 5")]
    pub kind: Option<charging_start_stop::Kind>,
}

pub mod charging_start_stop {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Kind {
        #[prost(message, tag = "2")]
        Start(super::Void),
        #[prost(message, tag = "5")]
        Stop(super::Void),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct HvacAutoAction {
    #[prost(bool, tag = "1")]
    pub power_on: bool,
    #[prost(bool, tag = "2")]
    pub manual_override: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct HvacSetPreconditioningMaxAction {
    #[prost(bool, tag = "1")]
    pub on: bool,
    #[prost(bool, tag = "2")]
    pub manual_override: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct HvacSteeringWheelHeaterAction {
    #[prost(bool, tag = "1")]
    pub power_on: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct HvacTemperatureAdjustmentAction {
    #[prost(float, tag = "6")]
    pub driver_temp_celsius: f32,
    #[prost(float, tag = "7")]
    pub passenger_temp_celsius: f32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VehicleControlSetSentryModeAction {
    #[prost(bool, tag = "1")]
    pub on: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VehicleControlWindowAction {
    #[prost(oneof = "window::Kind", tags = "3, 4")]
    pub kind: Option<window::Kind>,
}

pub mod window {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Kind {
        #[prost(message, tag = "3")]
        Vent(super::Void),
        #[prost(message, tag = "4")]
        Close(super::Void),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct HvacSeatHeaterActions {
    #[prost(message, repeated, tag = "1")]
    pub actions: Vec<HvacSeatHeaterAction>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct HvacSeatHeaterAction {
    #[prost(oneof = "seat_heater::Level", tags = "2, 3, 4, 5")]
    pub level: Option<seat_heater::Level>,
    #[prost(oneof = "seat_heater::Seat", tags = "7, 8, 9, 11, 12")]
    pub seat: Option<seat_heater::Seat>,
}

pub mod seat_heater {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Level {
        #[prost(message, tag = "2")]
        Off(super::Void),
        #[prost(message, tag = "3")]
        Low(super::Void),
        #[prost(message, tag = "4")]
        Medium(super::Void),
        #[prost(message, tag = "5")]
        High(super::Void),
    }

    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Seat {
        #[prost(message, tag = "7")]
        FrontLeft(super::Void),
        #[prost(message, tag = "8")]
        FrontRight(super::Void),
        #[prost(message, tag = "9")]
        RearLeft(super::Void),
        #[prost(message, tag = "11")]
        RearCenter(super::Void),
        #[prost(message, tag = "12")]
        RearRight(super::Void),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SetChargingAmpsAction {
    #[prost(int32, tag = "1")]
    pub charging_amps: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct HvacClimateKeeperAction {
    /// 0 off, 1 on (keep), 2 dog, 3 camp.
    #[prost(int32, tag = "1")]
    pub climate_keeper_action: i32,
    #[prost(bool, tag = "2")]
    pub manual_override: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ResultReason {
    #[prost(oneof = "result_reason::Kind", tags = "1")]
    pub kind: Option<result_reason::Kind>,
}

pub mod result_reason {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Kind {
        #[prost(string, tag = "1")]
        PlainText(String),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ActionStatus {
    /// 0 OK, 1 ERROR.
    #[prost(int32, tag = "1")]
    pub result: i32,
    #[prost(message, optional, tag = "2")]
    pub result_reason: Option<ResultReason>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Response {
    #[prost(message, optional, tag = "1")]
    pub action_status: Option<ActionStatus>,
}
