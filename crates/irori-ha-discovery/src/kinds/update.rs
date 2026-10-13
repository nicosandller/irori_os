//! Home Assistant's MQTT update: what version is installed, what the latest is, and one payload
//! that starts an install.
//!
//! The state topic carries either the installed version as text, or a JSON object with Home
//! Assistant's own field names. Zigbee2MQTT builds that object with a template that needs Jinja
//! (`.get()`, `| lower`), which Irori doesn't run; its template is recognised by its shape, and
//! the device's own `update` object is read instead, which says more anyway: whether an update
//! is available is Zigbee2MQTT's word, not a comparison of two version numbers.

use irori_types::{Capabilities, Service, State, UpdateCapabilities, UpdateClass, UpdateState};

use super::setting::{Reading, reading};
use crate::discovery::{EntityTopics, str_field};
use crate::state::{Message, Publish, text_publish};
use crate::template::ValueTemplate;

/// How the state topic's payload is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    /// Home Assistant's way: a version, or an object of its fields, through this template.
    Plainly(ValueTemplate),
    /// Zigbee2MQTT's way: the object at this path in the device's payload, in its own words.
    ByZigbee2Mqtt(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateTopics {
    pub state_topic: Option<String>,
    pub said: Said,
    /// The latest version, when it has a topic of its own.
    pub latest: Option<Reading>,
    /// Where an install is asked for, and with what. `None` when it only reports.
    pub install: Option<(String, String)>,
    /// Where it's asked to look for a newer version now, and with what.
    pub check: Option<(String, String)>,
    /// Where the bridge says an install failed, and the `id` it says it for.
    pub failure: Option<(String, String)>,
    pub title: Option<String>,
    pub release_summary: Option<String>,
    pub release_url: Option<String>,
}

impl UpdateTopics {
    pub(crate) fn listens(&self) -> Vec<&str> {
        self.state_topic
            .as_deref()
            .into_iter()
            .chain(self.latest.as_ref().map(|latest| latest.topic.as_str()))
            .chain(self.failure.as_ref().map(|(topic, _)| topic.as_str()))
            .collect()
    }
}

/// Zigbee2MQTT's request to update one device, which it also takes a check on.
const Z2M_UPDATE: &str = "/bridge/request/device/ota_update/update";

pub(crate) fn parse(root: &serde_json::Value) -> Result<(Capabilities, EntityTopics), String> {
    let template = str_field(root, "value_template");
    let said = match ValueTemplate::parse(template) {
        ValueTemplate::Unsupported(text) => {
            Said::ByZigbee2Mqtt(zigbee2mqtt_object(&text).ok_or_else(|| {
                format!("`value_template` needs more than Irori's templates do: {text}")
            })?)
        }
        simple => Said::Plainly(simple),
    };
    let install = str_field(root, "command_topic")
        .zip(str_field(root, "payload_install"))
        .map(|(topic, payload)| (topic.to_owned(), payload.to_owned()));
    // Zigbee2MQTT takes a check the way it takes an update, one topic over, and answers both.
    let bridge = install.as_ref().and_then(|(topic, payload)| {
        let base = topic.strip_suffix(Z2M_UPDATE)?;
        let id = serde_json::from_str::<serde_json::Value>(payload).ok()?;
        let id = id.get("id")?.as_str()?.to_owned();
        Some((base.to_owned(), payload.clone(), id))
    });
    let topics = UpdateTopics {
        state_topic: str_field(root, "state_topic").map(str::to_owned),
        said,
        latest: reading(root, "latest_version"),
        check: bridge.as_ref().map(|(base, payload, _)| {
            (
                format!("{base}/bridge/request/device/ota_update/check"),
                payload.clone(),
            )
        }),
        failure: bridge.map(|(base, _, id)| {
            (
                format!("{base}/bridge/response/device/ota_update/update"),
                id,
            )
        }),
        install,
        title: str_field(root, "title").map(str::to_owned),
        release_summary: str_field(root, "release_summary").map(str::to_owned),
        release_url: str_field(root, "release_url").map(str::to_owned),
    };
    let capabilities = UpdateCapabilities {
        device_class: str_field(root, "device_class").and_then(UpdateClass::parse),
        install: topics.install.is_some(),
        check: topics.check.is_some(),
    };
    Ok((
        Capabilities::Update(capabilities),
        EntityTopics::Update(Box::new(topics)),
    ))
}

/// Where Zigbee2MQTT's template reads from: it builds Home Assistant's object out of
/// `value_json['update']['latest_version']`, `…['installed_version']` and the rest, so the path
/// they share is the object itself. `None` for any other template.
fn zigbee2mqtt_object(template: &str) -> Option<Vec<String>> {
    let builds_an_object = ["installed_version", "latest_version", "in_progress"]
        .iter()
        .all(|field| template.contains(&format!("\"{field}\":")));
    if !builds_an_object {
        return None;
    }
    match ValueTemplate::first_path(template)? {
        ValueTemplate::JsonPathOrNothing(mut path) | ValueTemplate::JsonPath(mut path) => {
            path.pop()?;
            (!path.is_empty()).then_some(path)
        }
        _ => None,
    }
}

pub(crate) fn decode(
    topics: &UpdateTopics,
    message: Message,
    previous: Option<&State>,
) -> Option<Result<State, String>> {
    let previous = match previous {
        Some(State::Update(update)) => Some(update),
        _ => None,
    };
    if let Some((topic, id)) = &topics.failure
        && message.on(Some(topic))
    {
        return failed(message.payload, id, previous?).map(|state| Ok(State::Update(state)));
    }
    let mut state = previous.cloned().unwrap_or_default();
    // What the core adds comes back from the core; it isn't this entity's to repeat.
    state.skipped_version = None;
    state.last_error = None;
    let mut heard = false;
    if message.on(topics.state_topic.as_deref()) {
        heard = match &topics.said {
            Said::Plainly(template) => match plainly(&mut state, template, message.payload) {
                Ok(heard) => heard,
                Err(why) => return Some(Err(why)),
            },
            Said::ByZigbee2Mqtt(path) => by_zigbee2mqtt(&mut state, path, message.payload),
        };
    }
    if let Some(latest) = message.said(topics.latest.as_ref()) {
        state.latest_version = latest;
        heard = true;
    }
    if !heard {
        return None;
    }
    if matches!(topics.said, Said::Plainly(_)) {
        state.available = UpdateState::differs(
            state.installed_version.as_deref(),
            state.latest_version.as_deref(),
        );
    }
    if !state.in_progress {
        state.progress = None;
    }
    state.title = state.title.or_else(|| topics.title.clone());
    state.release_summary = state
        .release_summary
        .or_else(|| topics.release_summary.clone());
    state.release_url = state.release_url.or_else(|| topics.release_url.clone());
    Some(Ok(State::Update(state)))
}

/// A version as a device writes it: text, or (Zigbee's file versions) a bare number.
fn version(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_owned()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn text(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn percent(value: &serde_json::Value) -> Option<f64> {
    value.as_f64().map(|progress| progress.clamp(0.0, 100.0))
}

/// Home Assistant's state payload: an object of its fields, or the installed version alone.
/// Returns whether it said anything.
fn plainly(
    state: &mut UpdateState,
    template: &ValueTemplate,
    payload: &[u8],
) -> Result<bool, String> {
    let said = match template.extract(payload)? {
        serde_json::Value::String(text) => match serde_json::from_str(&text) {
            Ok(object @ serde_json::Value::Object(_)) => object,
            _ => serde_json::Value::String(text),
        },
        other => other,
    };
    let serde_json::Value::Object(fields) = said else {
        let installed = version(&said);
        let heard = installed.is_some();
        if heard {
            state.installed_version = installed;
        }
        return Ok(heard);
    };
    if let Some(installed) = fields.get("installed_version") {
        state.installed_version = version(installed);
    }
    if let Some(latest) = fields.get("latest_version") {
        state.latest_version = version(latest);
    }
    if let Some(title) = fields.get("title") {
        state.title = text(title);
    }
    if let Some(summary) = fields.get("release_summary") {
        state.release_summary = text(summary);
    }
    if let Some(url) = fields.get("release_url") {
        state.release_url = text(url);
    }
    // A percentage means it's installing, said or not, as Home Assistant reads it.
    let progress = fields.get("update_percentage").and_then(percent);
    if fields.contains_key("in_progress") || fields.contains_key("update_percentage") {
        state.in_progress = progress.is_some()
            || fields
                .get("in_progress")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
        state.progress = progress;
    }
    Ok(true)
}

/// Zigbee2MQTT's `update` object: `state` is `available`, `updating`, `scheduled` or `idle`.
/// Returns whether the payload had one that says anything; a device that hasn't been checked
/// yet has `"state": null`.
fn by_zigbee2mqtt(state: &mut UpdateState, path: &[String], payload: &[u8]) -> bool {
    let Ok(root) = serde_json::from_slice::<serde_json::Value>(payload) else {
        return false;
    };
    let Some(update) = path.iter().try_fold(&root, |at, key| at.get(key)) else {
        return false;
    };
    let Some(said) = update.get("state").and_then(serde_json::Value::as_str) else {
        return false;
    };
    state.installed_version = update.get("installed_version").and_then(version);
    state.latest_version = update.get("latest_version").and_then(version);
    state.in_progress = said == "updating";
    state.progress = update
        .get("progress")
        .and_then(percent)
        .filter(|_| state.in_progress);
    state.available = match said {
        // One it will install the next time the device asks is still one to install.
        "available" | "updating" | "scheduled" => true,
        "idle" => false,
        _ => UpdateState::differs(
            state.installed_version.as_deref(),
            state.latest_version.as_deref(),
        ),
    };
    true
}

/// The bridge's answer to an install: when it's an error, and for this device, the install is
/// over and this is why. Anything else says nothing new.
fn failed(payload: &[u8], id: &str, previous: &UpdateState) -> Option<UpdateState> {
    let answer: serde_json::Value = serde_json::from_slice(payload).ok()?;
    let for_this = answer.get("data")?.get("id")?.as_str()? == id;
    let error = answer.get("error").and_then(text)?;
    for_this.then(|| UpdateState {
        in_progress: false,
        progress: None,
        skipped_version: None,
        last_error: Some(error),
        ..previous.clone()
    })
}

pub(crate) fn encode(topics: &UpdateTopics, service: &Service) -> Result<Vec<Publish>, String> {
    let ask = |what: &Option<(String, String)>| {
        what.as_ref()
            .map(|(topic, payload)| vec![text_publish(topic, payload)])
            .ok_or_else(|| super::no_service("this update", service))
    };
    match service {
        Service::UpdateInstall => ask(&topics.install),
        Service::UpdateCheck => ask(&topics.check),
        service => Err(super::no_service("an update", service)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::parse;
    use crate::state::{decode, encode};
    use crate::topic::Component;

    /// What Zigbee2MQTT 2.x publishes for a device whose definition has OTA
    /// (`lib/extension/homeassistant.ts`).
    const ZIGBEE2MQTT: &str = r#"{
        "name": null,
        "object_id": "hall_plug_update",
        "unique_id": "0x00158d0001234567_update_zigbee2mqtt",
        "device": {"identifiers": ["zigbee2mqtt_0x00158d0001234567"], "name": "Hall plug"},
        "state_topic": "zigbee2mqtt/Hall plug",
        "device_class": "firmware",
        "entity_category": "config",
        "command_topic": "zigbee2mqtt/bridge/request/device/ota_update/update",
        "payload_install": "{\"id\": \"0x00158d0001234567\"}",
        "value_template": "{\"latest_version\":\"{{ value_json['update']['latest_version'] }}\",\"installed_version\":\"{{ value_json['update']['installed_version'] }}\",\"update_percentage\":{{ value_json['update'].get('progress', 'null') }},\"in_progress\":{{ (value_json['update']['state'] == 'updating')|lower }}}"
    }"#;

    fn said(
        topics: &EntityTopics,
        topic: &str,
        payload: &str,
        before: Option<&State>,
    ) -> UpdateState {
        match decode(topics, topic, payload.as_bytes(), before) {
            Some(Ok(State::Update(update))) => update,
            other => panic!("expected an update, got {other:?}"),
        }
    }

    #[test]
    fn zigbee2mqtts_update_is_read_from_the_device_itself() {
        let parsed = parse(Component::Update, ZIGBEE2MQTT.as_bytes()).expect("parses");
        assert_eq!(
            parsed.capabilities,
            Capabilities::Update(UpdateCapabilities {
                device_class: Some(UpdateClass::Firmware),
                install: true,
                check: true,
            })
        );
        let topic = "zigbee2mqtt/Hall plug";
        let there = said(
            &parsed.topics,
            topic,
            r#"{"state":"ON","update":{"installed_version":16777241,"latest_version":16777250,"state":"available"}}"#,
            None,
        );
        assert!(there.available);
        assert_eq!(there.installed_version.as_deref(), Some("16777241"));
        assert_eq!(there.latest_version.as_deref(), Some("16777250"));

        let before = State::Update(there);
        let flashing = said(
            &parsed.topics,
            topic,
            r#"{"update":{"installed_version":16777241,"latest_version":16777250,"state":"updating","progress":12.5,"remaining":900}}"#,
            Some(&before),
        );
        assert_eq!(
            (flashing.in_progress, flashing.progress),
            (true, Some(12.5))
        );

        let done = said(
            &parsed.topics,
            topic,
            r#"{"update":{"installed_version":16777250,"latest_version":16777250,"state":"idle"}}"#,
            Some(&State::Update(flashing)),
        );
        assert!(!done.available && !done.in_progress);
        assert_eq!(done.progress, None);

        // Not checked yet, or a message about something else: nothing to say.
        for nothing in [r#"{"update":{"state":null}}"#, r#"{"state":"OFF"}"#] {
            assert!(decode(&parsed.topics, topic, nothing.as_bytes(), None).is_none());
        }
    }

    #[test]
    fn zigbee2mqtt_is_asked_to_install_and_to_check_and_says_when_it_failed() {
        let parsed = parse(Component::Update, ZIGBEE2MQTT.as_bytes()).expect("parses");
        let sent = |service| {
            let sent = encode(&parsed.topics, &service).expect("it can");
            (
                sent[0].topic.clone(),
                String::from_utf8_lossy(&sent[0].payload).into_owned(),
            )
        };
        let id = r#"{"id": "0x00158d0001234567"}"#.to_owned();
        assert_eq!(
            sent(Service::UpdateInstall),
            (
                "zigbee2mqtt/bridge/request/device/ota_update/update".to_owned(),
                id.clone()
            )
        );
        assert_eq!(
            sent(Service::UpdateCheck),
            (
                "zigbee2mqtt/bridge/request/device/ota_update/check".to_owned(),
                id
            )
        );

        let flashing = State::Update(UpdateState {
            available: true,
            installed_version: Some("1".into()),
            latest_version: Some("2".into()),
            in_progress: true,
            progress: Some(3.0),
            ..UpdateState::default()
        });
        let answers = "zigbee2mqtt/bridge/response/device/ota_update/update";
        let failed = said(
            &parsed.topics,
            answers,
            r#"{"data":{"id":"0x00158d0001234567"},"status":"error","error":"Update of 'Hall plug' failed (Device didn't respond to OTA request)"}"#,
            Some(&flashing),
        );
        assert!(!failed.in_progress);
        assert!(
            failed
                .last_error
                .is_some_and(|why| why.contains("didn't respond"))
        );
        // Another device's failure, and a success, aren't this one's to report.
        for other in [
            r#"{"data":{"id":"0x0000000000000001"},"status":"error","error":"no"}"#,
            r#"{"data":{"id":"0x00158d0001234567","from":{},"to":{}},"status":"ok"}"#,
        ] {
            assert!(decode(&parsed.topics, answers, other.as_bytes(), Some(&flashing)).is_none());
        }
    }

    #[test]
    fn home_assistants_own_update_is_a_version_or_an_object() {
        let parsed = parse(
            Component::Update,
            br#"{
                "name": "Firmware", "unique_id": "amp-fw",
                "state_topic": "amp/firmware",
                "latest_version_topic": "amp/firmware/latest",
                "command_topic": "amp/firmware/install",
                "payload_install": "go",
                "title": "Amplifier firmware",
                "release_url": "https://example.com/amp"
            }"#,
        )
        .expect("parses");
        assert_eq!(
            parsed.capabilities,
            Capabilities::Update(UpdateCapabilities {
                device_class: None,
                install: true,
                check: false,
            })
        );
        let installed = said(&parsed.topics, "amp/firmware", "2.0.1", None);
        assert!(!installed.available, "nothing to compare it with yet");
        assert_eq!(installed.title.as_deref(), Some("Amplifier firmware"));
        let before = State::Update(installed);
        let latest = said(
            &parsed.topics,
            "amp/firmware/latest",
            "2.1.0",
            Some(&before),
        );
        assert!(latest.available);
        assert_eq!(latest.installed_version.as_deref(), Some("2.0.1"));

        let before = State::Update(latest);
        let flashing = said(
            &parsed.topics,
            "amp/firmware",
            r#"{"installed_version":"2.0.1","update_percentage":40,"release_summary":"Quieter fan."}"#,
            Some(&before),
        );
        assert_eq!(
            (flashing.in_progress, flashing.progress),
            (true, Some(40.0))
        );
        assert_eq!(flashing.release_summary.as_deref(), Some("Quieter fan."));
        assert!(flashing.validate().is_ok());
        assert!(encode(&parsed.topics, &Service::UpdateCheck).is_err());
    }

    #[test]
    fn a_template_that_needs_jinja_and_isnt_zigbee2mqtts_is_refused() {
        let refused = parse(
            Component::Update,
            br#"{"name": "Firmware", "unique_id": "x", "state_topic": "t",
                "value_template": "{{ value_json.version | regex_replace('v', '') }}"}"#,
        );
        assert!(refused.is_err_and(|why| why.contains("value_template")));
    }
}
