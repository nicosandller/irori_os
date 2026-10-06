//! Zigbee2MQTT's own bridge topics — not part of HA Discovery, but published on the same broker
//! (`<base topic>/bridge/...`) and how a permit-join action is offered and triggered
//! (`docs/specs/protocols.md`'s new `ActionCall`/`next_action()`, the `mqtt`/`zigbee` protocols'
//! `permit_join` action).

use crate::state::Publish;

/// The topic a Z2M bridge publishes its own status to, retained. Seeing anything on it at all is
/// how a protocol tells "a Z2M bridge is actually here" from "this is just a plain broker" —
/// the `permit_join` action is only declared available once this has actually been seen.
pub fn info_topic(base_topic: &str) -> String {
    format!("{base_topic}/bridge/info")
}

/// Whether a payload on [`info_topic`] looks like a real Z2M bridge (rather than, say, some
/// unrelated retained message a coincidentally-matching topic happens to carry). Z2M always
/// includes its own `version` in this payload; that's enough to tell.
pub fn is_bridge_info(payload: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| v.get("version").and_then(|v| v.as_str()).map(str::to_owned))
        .is_some()
}

/// The request to open Z2M's network to new devices for `seconds`, or with `0` to close it
/// now. Z2M answers on `<base>/bridge/response/permit_join`, which this crate doesn't need to
/// read: [`join_window`] reads the outcome off `bridge/info`, which Z2M publishes again
/// whenever the network opens or closes.
pub fn permit_join(base_topic: &str, seconds: u32) -> Publish {
    Publish {
        topic: format!("{base_topic}/bridge/request/permit_join"),
        payload: serde_json::to_vec(&serde_json::json!({ "value": seconds > 0, "time": seconds }))
            .expect("a json! body always serializes"),
    }
}

/// Whether Z2M's network is taking new devices, as a `bridge/info` payload says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinWindow {
    Closed,
    /// Open for this much longer. `None` when Z2M says it's open but not until when.
    Open(Option<std::time::Duration>),
}

/// Reads `permit_join` and `permit_join_end` (milliseconds since 1970) off a [`info_topic`]
/// payload. `now_ms` is this machine's clock, which is also Z2M's: the two run side by side.
/// `None` for a payload that isn't bridge info at all.
pub fn join_window(payload: &[u8], now_ms: u64) -> Option<JoinWindow> {
    let info = serde_json::from_slice::<serde_json::Value>(payload).ok()?;
    info.get("version")?;
    if info.get("permit_join").and_then(serde_json::Value::as_bool) != Some(true) {
        return Some(JoinWindow::Closed);
    }
    let end = info
        .get("permit_join_end")
        .and_then(serde_json::Value::as_f64);
    Some(match end {
        // Already past: Z2M hasn't said so yet, but there's nothing left to count.
        Some(end) if end <= now_ms as f64 => JoinWindow::Closed,
        Some(end) => JoinWindow::Open(Some(std::time::Duration::from_millis(
            (end - now_ms as f64) as u64,
        ))),
        None => JoinWindow::Open(None),
    })
}

/// The prefix Z2M gives every device identifier in its discovery configs.
const DEVICE_PREFIX: &str = "zigbee2mqtt_";

/// The IEEE address behind a device's discovery identifier (`zigbee2mqtt_0x00158d0001a2b3c4`),
/// which is what Z2M's own requests name a device by. The bridge and groups are published as
/// devices too, and neither is something that pairs: for those, why not.
pub fn ieee_address(device_identifier: &str) -> Result<&str, String> {
    let rest = device_identifier
        .strip_prefix(DEVICE_PREFIX)
        .ok_or("it isn't a device Zigbee2MQTT paired")?;
    if rest.starts_with("bridge_") {
        return Err(
            "it's the Zigbee controller itself, which is the network rather than a device on it"
                .into(),
        );
    }
    if !rest.starts_with("0x") {
        return Err("it's a group, not a device that pairs".into());
    }
    Ok(rest)
}

/// The request to take a device off Z2M's network. Plain, Z2M asks the device to leave and
/// waits for it to agree; with `force` it only strikes the device from its own records, for
/// one that doesn't answer. `transaction` comes back in the response, to tell answers apart.
pub fn remove_device(base_topic: &str, ieee: &str, force: bool, transaction: &str) -> Publish {
    Publish {
        topic: format!("{base_topic}/bridge/request/device/remove"),
        payload: serde_json::to_vec(&serde_json::json!({
            "id": ieee,
            "force": force,
            "transaction": transaction,
        }))
        .expect("a json! body always serializes"),
    }
}

/// Where Z2M answers a [`remove_device`] request.
pub fn remove_response_topic(base_topic: &str) -> String {
    format!("{base_topic}/bridge/response/device/remove")
}

/// Z2M's answer to a request: which one, and how it went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub transaction: Option<String>,
    /// `Err` carries Z2M's own words for what went wrong.
    pub outcome: Result<(), String>,
}

/// Reads a `bridge/response/…` payload. `None` if it isn't one.
pub fn response(payload: &[u8]) -> Option<Response> {
    let body = serde_json::from_slice::<serde_json::Value>(payload).ok()?;
    let status = body.get("status")?.as_str()?;
    let outcome = if status == "ok" {
        Ok(())
    } else {
        Err(body
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Zigbee2MQTT refused without saying why")
            .to_owned())
    };
    Some(Response {
        transaction: body
            .get("transaction")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        outcome,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_a_real_bridge_info_payload_and_rejects_unrelated_json() {
        assert!(is_bridge_info(br#"{"version": "2.1.0", "commit": "abc"}"#));
        assert!(!is_bridge_info(br#"{"not_a_bridge": true}"#));
        assert!(!is_bridge_info(b"not json at all"));
    }

    #[test]
    fn builds_the_permit_join_request() {
        let publish = permit_join("zigbee2mqtt", 60);
        assert_eq!(publish.topic, "zigbee2mqtt/bridge/request/permit_join");
        assert_eq!(publish.payload, br#"{"time":60,"value":true}"#);
    }

    #[test]
    fn closing_the_network_asks_for_no_time_at_all() {
        let publish = permit_join("zigbee2mqtt", 0);
        assert_eq!(publish.payload, br#"{"time":0,"value":false}"#);
    }

    #[test]
    fn bridge_info_says_how_long_the_network_stays_open() {
        let open = br#"{"version": "2.14.2", "permit_join": true, "permit_join_end": 61000}"#;
        assert_eq!(
            join_window(open, 1000),
            Some(JoinWindow::Open(Some(std::time::Duration::from_secs(60))))
        );
        assert_eq!(
            join_window(open, 61_000),
            Some(JoinWindow::Closed),
            "an end already reached is closed, whatever the flag still says"
        );
        assert_eq!(
            join_window(br#"{"version": "2.14.2", "permit_join": true}"#, 1000),
            Some(JoinWindow::Open(None))
        );
        assert_eq!(
            join_window(br#"{"version": "2.14.2", "permit_join": false}"#, 1000),
            Some(JoinWindow::Closed)
        );
        assert_eq!(join_window(br#"{"permit_join": true}"#, 1000), None);
    }

    #[test]
    fn only_a_paired_device_has_an_address_to_unpair() {
        assert_eq!(
            ieee_address("zigbee2mqtt_0x00158d0001a2b3c4"),
            Ok("0x00158d0001a2b3c4")
        );
        assert!(ieee_address("zigbee2mqtt_bridge_0x00124b0001020304").is_err());
        assert!(ieee_address("zigbee2mqtt_zigbee2mqtt_3").is_err());
        assert!(ieee_address("tasmota_ABC123").is_err());
    }

    #[test]
    fn builds_the_remove_request_and_reads_its_answer() {
        let publish = remove_device("zigbee2mqtt", "0x00158d0001a2b3c4", true, "irori-7");
        assert_eq!(publish.topic, "zigbee2mqtt/bridge/request/device/remove");
        assert_eq!(
            publish.payload,
            br#"{"force":true,"id":"0x00158d0001a2b3c4","transaction":"irori-7"}"#
        );

        assert_eq!(
            response(br#"{"data":{"id":"0x00158d0001a2b3c4"},"status":"ok","transaction":"irori-7"}"#),
            Some(Response {
                transaction: Some("irori-7".into()),
                outcome: Ok(()),
            })
        );
        let failed = response(
            br#"{"data":{},"status":"error","error":"Failed to remove device (no response)","transaction":"irori-8"}"#,
        )
        .expect("a response");
        assert_eq!(
            failed.outcome,
            Err("Failed to remove device (no response)".into())
        );
        assert_eq!(response(b"not json"), None);
    }
}
