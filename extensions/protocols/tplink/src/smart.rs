//! TP-Link's "smart" JSON API, as Tapo devices speak it over KLAP: a method name and its
//! parameters, several batched as `multipleRequest`, and a strip's outlets reached through
//! `control_child`.
//!
//! Only reading methods are built here. The extension is read-only until switching is tested.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use serde_json::{Value, json};

/// The methods a reading uses, and the component that says a device has them.
pub const DEVICE_INFO: &str = "get_device_info";
pub const ENERGY_USAGE: &str = "get_energy_usage";
pub const EMETER_DATA: &str = "get_emeter_data";
pub const LED_INFO: &str = "get_led_info";
pub const CHILD_LIST: &str = "get_child_device_list";
pub const COMPONENTS: &str = "component_nego";
pub const CHILD_COMPONENTS: &str = "get_child_device_component_list";

/// The error code a device gives for a method it doesn't have.
pub const UNKNOWN_METHOD: i64 = -1002;

pub fn request(method: &str, params: Value) -> Value {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    json!({ "method": method, "params": params, "request_time_milis": millis as u64 })
}

pub fn multiple(methods: &[(&str, Value)]) -> Value {
    let requests: Vec<Value> = methods
        .iter()
        .map(|(method, params)| json!({ "method": method, "params": params }))
        .collect();
    request("multipleRequest", json!({ "requests": requests }))
}

pub fn control_child(child: &str, inner: Value) -> Value {
    request(
        "control_child",
        json!({ "device_id": child, "requestData": inner }),
    )
}

/// What one method answered: its result, or the device's error code.
pub type Answer = Result<Value, i64>;

/// The answers of a `multipleRequest`, by method. A whole-request error is the `Err`.
pub fn answers(response: &Value) -> Result<BTreeMap<String, Answer>, i64> {
    let code = response
        .get("error_code")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    if code != 0 {
        return Err(code);
    }
    let mut out = BTreeMap::new();
    let list = response
        .pointer("/result/responses")
        .and_then(Value::as_array)
        .ok_or(-1)?;
    for item in list {
        let Some(method) = item.get("method").and_then(Value::as_str) else {
            continue;
        };
        let code = item.get("error_code").and_then(Value::as_i64).unwrap_or(-1);
        let answer = if code == 0 {
            Ok(item.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(code)
        };
        out.insert(method.to_owned(), answer);
    }
    Ok(out)
}

/// The answers of a `multipleRequest` sent to one outlet through `control_child`.
pub fn child_answers(response: &Value) -> Result<BTreeMap<String, Answer>, i64> {
    let code = response
        .get("error_code")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    if code != 0 {
        return Err(code);
    }
    // Real strips leave `error_code` out of the inner batch; each answer still carries its own.
    let mut inner = response.pointer("/result/responseData").ok_or(-1)?.clone();
    if let Some(batch) = inner.as_object_mut() {
        batch.entry("error_code").or_insert(Value::from(0));
    }
    answers(&inner)
}

/// Component ids from `component_nego` or one child's entry in
/// `get_child_device_component_list`.
pub fn component_ids(list: &Value) -> BTreeSet<String> {
    list.get("component_list")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

/// `get_child_device_component_list`: each outlet's components, by its device id.
/// How many entries a page of a paged list holds, whether or not each one can be read.
pub fn entries(result: &Value, key: &str) -> usize {
    result
        .get(key)
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

pub fn child_component_ids(result: &Value) -> BTreeMap<String, BTreeSet<String>> {
    result
        .get("child_component_list")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|child| {
            let id = child.get("device_id").and_then(Value::as_str)?;
            Some((id.to_owned(), component_ids(child)))
        })
        .collect()
}

/// A MAC address as `aa:bb:cc:dd:ee:ff`, from TP-Link's `AA-BB-CC-DD-EE-FF` or with colons.
pub fn mac(text: &str) -> Option<String> {
    let hex: String = text
        .chars()
        .filter(|c| !matches!(c, '-' | ':'))
        .collect::<String>()
        .to_ascii_lowercase();
    if hex.len() != 12 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let pairs: Vec<&str> = (0..6).map(|i| &hex[i * 2..i * 2 + 2]).collect();
    Some(pairs.join(":"))
}

/// A device's own name. TP-Link keeps nicknames as base64; blank and whitespace-padded ones are
/// common (the app leaves a strip's own name empty).
pub fn nickname(info: &Value) -> Option<String> {
    let encoded = info.get("nickname").and_then(Value::as_str)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let text = String::from_utf8(bytes).ok()?;
    // Cut to a name's 100 characters before trimming, so the cut can't leave a space at the end.
    let text: String = text.chars().filter(|c| !c.is_control()).take(100).collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// What a device says about itself (`get_device_info`), or an outlet in a strip's child list.
#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    pub device_id: String,
    pub name: Option<String>,
    pub model: Option<String>,
    pub fw_ver: Option<String>,
    pub hw_ver: Option<String>,
    pub mac: Option<String>,
    /// The relay, when the device has one (a strip itself doesn't).
    pub on: Option<bool>,
    pub rssi: Option<f64>,
    pub position: Option<u64>,
}

pub fn info(value: &Value) -> Option<Info> {
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    Some(Info {
        device_id: text("device_id")?,
        name: nickname(value),
        model: text("model"),
        fw_ver: text("fw_ver"),
        hw_ver: text("hw_ver"),
        mac: text("mac").as_deref().and_then(mac),
        on: value.get("device_on").and_then(Value::as_bool),
        rssi: value.get("rssi").and_then(Value::as_f64),
        position: value.get("position").and_then(Value::as_u64),
    })
}

/// What a metering device measures, in W, V, A and kWh.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meter {
    pub power: Option<f64>,
    pub voltage: Option<f64>,
    pub current: Option<f64>,
    pub today: Option<f64>,
    pub total: Option<f64>,
}

impl Meter {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A meter from whichever of `get_emeter_data` and `get_energy_usage` answered. The first has
/// voltage, current and the running total; the second has today's energy. Both have power: the
/// first in mW as `power_mw`, the second in mW as `current_power`.
pub fn meter(emeter: Option<&Value>, usage: Option<&Value>) -> Meter {
    let number = |value: Option<&Value>, key: &str, scale: f64| {
        value
            .and_then(|value| value.get(key))
            .and_then(Value::as_f64)
            .filter(|number| number.is_finite())
            .map(|number| round(number / scale))
    };
    Meter {
        power: number(emeter, "power_mw", 1000.0).or(number(usage, "current_power", 1000.0)),
        voltage: number(emeter, "voltage_mv", 1000.0),
        current: number(emeter, "current_ma", 1000.0),
        today: number(usage, "today_energy", 1000.0),
        total: number(emeter, "energy_wh", 1000.0),
    }
}

/// Three decimals: mW, mV and mA, and Wh as kWh, without float noise.
fn round(number: f64) -> f64 {
    (number * 1000.0).round() / 1000.0
}

/// The LED: on unless its rule is `never` (or the device says it's off right now).
pub fn led(value: &Value) -> Option<bool> {
    if let Some(status) = value.get("led_status").and_then(Value::as_bool) {
        return Some(status);
    }
    value
        .get("led_rule")
        .and_then(Value::as_str)
        .map(|rule| rule != "never")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_multiple_request_lists_each_method() {
        let request = multiple(&[(DEVICE_INFO, Value::Null), (LED_INFO, Value::Null)]);
        assert_eq!(request["method"], "multipleRequest");
        assert_eq!(request["params"]["requests"][1]["method"], LED_INFO);
    }

    #[test]
    fn answers_keep_each_methods_error() {
        let response = json!({
            "error_code": 0,
            "result": { "responses": [
                { "method": "get_device_info", "result": { "device_id": "X" }, "error_code": 0 },
                { "method": "get_emeter_data", "error_code": -1 }
            ]}
        });
        let answers = answers(&response).expect("valid");
        assert!(answers["get_device_info"].is_ok());
        assert_eq!(answers["get_emeter_data"], Err(-1));
        assert_eq!(super::answers(&json!({ "error_code": -1010 })), Err(-1010));
    }

    #[test]
    fn an_outlets_answers_need_no_batch_error_code() {
        // As a Tapo strip answers `control_child`.
        let response = json!({
            "error_code": 0,
            "result": { "responseData": { "result": { "responses": [
                { "method": "get_emeter_data", "result": { "power_mw": 118922 }, "error_code": 0 }
            ]}}}
        });
        let answers = child_answers(&response).expect("valid");
        assert!(answers["get_emeter_data"].is_ok());
        let refused = json!({
            "error_code": 0,
            "result": { "responseData": { "error_code": -1001 } }
        });
        assert_eq!(child_answers(&refused), Err(-1001));
    }

    #[test]
    fn macs_and_nicknames_are_cleaned_up() {
        assert_eq!(
            mac("02-00-00-00-00-02").as_deref(),
            Some("02:00:00:00:00:02")
        );
        assert_eq!(mac("nope"), None);
        let encoded = base64::engine::general_purpose::STANDARD.encode("Reading Lamp ");
        assert_eq!(
            nickname(&json!({ "nickname": encoded })).as_deref(),
            Some("Reading Lamp")
        );
        assert_eq!(nickname(&json!({ "nickname": "" })), None);
        // The 100-character cut lands just after a space.
        let long = format!("{} {}", "a".repeat(99), "b".repeat(20));
        let encoded = base64::engine::general_purpose::STANDARD.encode(long);
        assert_eq!(
            nickname(&json!({ "nickname": encoded })),
            Some("a".repeat(99))
        );
    }

    #[test]
    fn a_meter_takes_what_each_method_has() {
        let emeter =
            json!({ "power_mw": 1234, "voltage_mv": 240123, "current_ma": 56, "energy_wh": 98765 });
        let usage = json!({ "today_energy": 1500, "current_power": 999 });
        let meter = meter(Some(&emeter), Some(&usage));
        assert_eq!(meter.power, Some(1.234));
        assert_eq!(meter.voltage, Some(240.123));
        assert_eq!(meter.current, Some(0.056));
        assert_eq!(meter.today, Some(1.5));
        assert_eq!(meter.total, Some(98.765));

        let usage_only = super::meter(None, Some(&usage));
        assert_eq!(usage_only.power, Some(0.999));
        assert_eq!(usage_only.voltage, None);
    }

    #[test]
    fn the_led_follows_its_status_then_its_rule() {
        assert_eq!(led(&json!({ "led_rule": "never" })), Some(false));
        assert_eq!(led(&json!({ "led_rule": "always" })), Some(true));
        assert_eq!(
            led(&json!({ "led_rule": "auto", "led_status": false })),
            Some(false)
        );
        assert_eq!(led(&json!({})), None);
    }
}
