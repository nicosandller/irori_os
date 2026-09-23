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

/// The request to open Z2M's network to new devices for `seconds`. Z2M answers on
/// `<base>/bridge/response/permit_join`, which this crate doesn't need to read — the new
/// devices joining and announcing themselves through ordinary HA Discovery is the real signal
/// pairing worked.
pub fn permit_join(base_topic: &str, seconds: u32) -> Publish {
    Publish {
        topic: format!("{base_topic}/bridge/request/permit_join"),
        payload: serde_json::to_vec(&serde_json::json!({ "value": true, "time": seconds }))
            .expect("a json! body always serializes"),
    }
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
}
