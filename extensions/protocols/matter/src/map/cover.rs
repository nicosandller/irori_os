//! A blind, shade, curtain or awning: Window Covering (0x0102). Following Home Assistant's
//! `cover.py`.
//!
//! Matter's positions are in hundredths of a percent *closed*: 0 is fully open, 10000 fully
//! closed. Irori's are percent open. Every position is turned round here, both ways.

use serde_json::json;

use crate::model::{Command, Node, cluster};
use irori_types::{CoverCapabilities, CoverClass, CoverState, OpenState, Service, State};

/// Window Covering's feature bits.
mod feature {
    pub const LIFT: u64 = 1;
    pub const TILT: u64 = 2;
    pub const POSITION_AWARE_LIFT: u64 = 4;
    pub const POSITION_AWARE_TILT: u64 = 16;
}

const TYPE: u32 = 0x0000;
const OPERATIONAL_STATUS: u32 = 0x000A;
const CURRENT_LIFT: u32 = 0x000E;
const CURRENT_TILT: u32 = 0x000F;

pub(super) fn capabilities(node: &Node, endpoint: u16) -> CoverCapabilities {
    let features = node.features(endpoint, cluster::WINDOW_COVERING);
    let has = |bits| features & bits == bits;
    CoverCapabilities {
        device_class: class(node.number(endpoint, cluster::WINDOW_COVERING, TYPE)),
        position: has(feature::LIFT | feature::POSITION_AWARE_LIFT),
        tilt: has(feature::TILT | feature::POSITION_AWARE_TILT),
        stop: true,
    }
}

/// The covering's `Type`, as Home Assistant names it.
fn class(kind: Option<f64>) -> Option<CoverClass> {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "an enum8"
    )]
    Some(match kind? as u8 {
        0..=3 | 9 => CoverClass::Shade,
        4 => CoverClass::Curtain,
        5 => CoverClass::Awning,
        6 => CoverClass::Shutter,
        7 | 8 => CoverClass::Blind,
        _ => return None,
    })
}

/// Percent open, from hundredths of a percent closed.
fn percent_open(hundredths_closed: f64) -> u8 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=100 first"
    )]
    let open = (100.0 - hundredths_closed / 100.0)
        .round()
        .clamp(0.0, 100.0) as u8;
    open
}

/// Hundredths of a percent closed, from percent open.
fn hundredths_closed(percent_open: u8) -> u16 {
    (100 - u16::from(percent_open.min(100))) * 100
}

pub(super) fn state(node: &Node, endpoint: u16, known: &CoverCapabilities) -> Option<State> {
    let read = |attribute| node.number(endpoint, cluster::WINDOW_COVERING, attribute);
    let position = known
        .position
        .then(|| read(CURRENT_LIFT))
        .flatten()
        .map(percent_open);
    let tilt = known
        .tilt
        .then(|| read(CURRENT_TILT))
        .flatten()
        .map(percent_open);
    // The two low bits of OperationalStatus are the whole covering's: 1 opening, 2 closing.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a bitmap8"
    )]
    let moving = read(OPERATIONAL_STATUS).map_or(0, |status| status as u8 & 0b11);
    let state = match (moving, position) {
        (1, _) => OpenState::Opening,
        (2, _) => OpenState::Closing,
        (_, Some(position)) => OpenState::at(position),
        // A covering that can't say where it is: open, unless it said it's closed.
        (_, None) => match read(CURRENT_LIFT) {
            Some(closed) if closed >= 10_000.0 => OpenState::Closed,
            _ => OpenState::Open,
        },
    };
    Some(State::Cover(CoverState {
        state,
        position,
        tilt,
    }))
}

pub(super) fn command(endpoint: u16, service: &Service) -> Result<Vec<Command>, String> {
    let invoke = |name, payload| Command::invoke(endpoint, cluster::WINDOW_COVERING, name, payload);
    Ok(vec![match service {
        Service::CoverOpen => invoke("upOrOpen", json!({})),
        Service::CoverClose => invoke("downOrClose", json!({})),
        Service::CoverStop => invoke("stopMotion", json!({})),
        Service::CoverSetPosition(set) => invoke(
            "goToLiftPercentage",
            json!({"liftPercent100thsValue": hundredths_closed(set.position)}),
        ),
        Service::CoverSetTilt(set) => invoke(
            "goToTiltPercentage",
            json!({"tiltPercent100thsValue": hundredths_closed(set.tilt)}),
        ),
        other => return Err(format!("a cover doesn't take {}", other.name())),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_turn_round_between_matter_and_irori() {
        assert_eq!(percent_open(0.0), 100);
        assert_eq!(percent_open(10_000.0), 0);
        assert_eq!(percent_open(2_500.0), 75);
        assert_eq!(hundredths_closed(75), 2_500);
        assert_eq!(hundredths_closed(100), 0);
        assert_eq!(hundredths_closed(0), 10_000);
    }
}
