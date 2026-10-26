//! A node's firmware: an `update` entity for a node with an OTA Software Update Requestor
//! (0x002A), the cluster that takes new firmware. What's newer comes from the Matter server
//! (`client::SoftwareUpdate`); what's installed, and how far an install has got, from the node.

use crate::client::SoftwareUpdate;
use crate::model::{Node, cluster};
use irori_types::{State, UpdateCapabilities, UpdateClass, UpdateState};

/// Basic Information's `SoftwareVersion` and `SoftwareVersionString`.
const SOFTWARE_VERSION: u32 = 9;
const SOFTWARE_VERSION_STRING: u32 = 10;
/// The requestor's `UpdateState` and `UpdateStateProgress`.
const UPDATE_STATE: u32 = 2;
const UPDATE_PROGRESS: u32 = 3;

/// Whether the node can take new firmware.
pub(super) fn has_requestor(node: &Node) -> bool {
    node.has_cluster(0, cluster::OTA_SOFTWARE_UPDATE_REQUESTOR)
}

pub(super) fn capabilities() -> UpdateCapabilities {
    UpdateCapabilities {
        device_class: Some(UpdateClass::Firmware),
        install: true,
        check: true,
    }
}

/// Whether the node is taking new firmware now: downloading (4), applying (5) or waiting to
/// apply (6).
pub fn installing(node: &Node) -> bool {
    matches!(
        node.value(0, cluster::OTA_SOFTWARE_UPDATE_REQUESTOR, UPDATE_STATE)
            .and_then(serde_json::Value::as_u64),
        Some(4..=6)
    )
}

pub(super) fn state(node: &Node, newer: Option<&SoftwareUpdate>) -> Option<State> {
    let installed_number = node
        .value(0, cluster::BASIC_INFORMATION, SOFTWARE_VERSION)
        .and_then(serde_json::Value::as_u64);
    let installed = node
        .text(0, cluster::BASIC_INFORMATION, SOFTWARE_VERSION_STRING)
        .map(str::to_owned)
        .or_else(|| installed_number.map(|number| number.to_string()));
    // The server keeps an update it found until the node has it; once the node says it runs
    // that version or a later one, it isn't newer any more.
    let newer = newer.filter(|update| installed_number.is_none_or(|now| update.version > now));
    let in_progress = installing(node);
    let progress = in_progress
        .then(|| node.number(0, cluster::OTA_SOFTWARE_UPDATE_REQUESTOR, UPDATE_PROGRESS))
        .flatten()
        .filter(|progress| (0.0..=100.0).contains(progress));
    Some(State::Update(UpdateState {
        available: newer.is_some(),
        latest_version: newer
            .map(|update| update.version_string.clone())
            .or_else(|| installed.clone()),
        installed_version: installed,
        in_progress,
        progress,
        title: None,
        release_summary: None,
        release_url: newer.and_then(|update| update.release_url.clone()),
        skipped_version: None,
        last_error: None,
    }))
}
