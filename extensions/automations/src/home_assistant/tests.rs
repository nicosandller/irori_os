use super::*;

use irori_flow_types::{Node, Trigger};
use irori_types::EntityId;

/// A home Home Assistant's devices came into: a hall light, a motion sensor, a lux sensor and a
/// media player, by their registry ids; and a kitchen light Home Assistant has that isn't added.
fn registry() -> MapRegistry {
    let entity = |json: Value| -> (EntityId, Entity) {
        let entity: Entity = serde_json::from_value(json).expect("an entity");
        (entity.id.clone(), entity)
    };
    MapRegistry {
        entities: [
            entity(json!({
                "id": "light.hall", "protocol": "homeassistant", "unique_id": "r-hall-light",
                "name": "Hall light",
                "capabilities": { "kind": "light", "brightness": true, "rgb": false },
            })),
            entity(json!({
                "id": "binary_sensor.hall_motion", "protocol": "homeassistant",
                "unique_id": "r-hall-motion", "name": "Hall motion",
                "capabilities": { "kind": "binary_sensor", "device_class": "motion" },
            })),
            entity(json!({
                "id": "sensor.hall_lux", "protocol": "homeassistant", "unique_id": "r-hall-lux",
                "name": "Hall lux",
                "capabilities": { "kind": "sensor", "value_type": "number", "device_class": "illuminance", "unit": "lx" },
            })),
            // The same kind of thing from elsewhere: never taken for Home Assistant's.
            entity(json!({
                "id": "light.hall_native", "protocol": "esphome", "unique_id": "r-kitchen",
                "name": "Native light",
                "capabilities": { "kind": "light", "brightness": true, "rgb": false },
            })),
        ]
        .into_iter()
        .collect(),
        timezone: true,
        location: true,
    }
}

fn brought() -> BTreeMap<String, String> {
    [
        ("light.hall_ceiling", "r-hall-light"),
        ("binary_sensor.hall_motion", "r-hall-motion"),
        ("sensor.hall_lux", "r-hall-lux"),
        ("light.kitchen", "r-kitchen"),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_owned(), b.to_owned()))
    .collect()
}

fn automation() -> Listed {
    Listed {
        entity_id: "automation.hall_light_on_motion".into(),
        name: "Hall light on motion".into(),
        on: true,
        id: Some("1700000000001".into()),
    }
}

fn bring(setup: Value) -> Result<Flow, Vec<String>> {
    let registry = registry();
    let home = Home::new(&registry, brought());
    translate(
        &setup,
        &automation(),
        RuleId::try_from("hall_light_on_motion").expect("an id"),
        &home,
    )
}

#[test]
fn a_motion_light_comes_over_switched_off_and_remembering_its_twin() {
    let flow = bring(json!({
        "id": "1700000000001",
        "alias": "Hall light on motion",
        "description": "Light the hall when someone walks in after dark.",
        "mode": "restart",
        "triggers": [{ "trigger": "state", "entity_id": "binary_sensor.hall_motion", "to": "on" }],
        "conditions": [
            { "condition": "numeric_state", "entity_id": "sensor.hall_lux", "below": 30 },
            { "condition": "sun", "after": "sunset", "after_offset": "-00:30:00" },
        ],
        "actions": [
            { "action": "light.turn_on", "target": { "entity_id": "light.hall_ceiling" }, "data": { "brightness_pct": 60 } },
            { "delay": { "minutes": 2 } },
            { "action": "light.turn_off", "target": { "entity_id": "light.hall_ceiling" } },
        ],
    }))
    .expect("a flow");
    assert!(!flow.enabled, "it arrives switched off");
    assert_eq!(flow.name.as_str(), "Hall light on motion");
    assert_eq!(
        flow.home_assistant,
        Some(irori_flow_types::HomeAssistantTwin {
            entity_id: "automation.hall_light_on_motion".into(),
            id: "1700000000001".into(),
        })
    );
    let value = serde_json::to_value(&flow).expect("json");
    assert_eq!(value["mode"], "restart");
    assert_eq!(
        value["nodes"],
        json!({
            "when_1": { "type": "trigger", "trigger": { "type": "state", "entity": "binary_sensor.hall_motion", "to": true } },
            "check_1": { "type": "gate", "condition": { "type": "expr", "expr": "num('sensor.hall_lux') < 30" } },
            "check_2": { "type": "gate", "condition": { "type": "sun", "after": "sunset", "after_offset": "-30m" } },
            "do_1": { "type": "call", "service": "light.turn_on", "entity": "light.hall", "data": { "brightness_pct": 60 } },
            "wait_1": { "type": "delay", "for": "2m" },
            "do_2": { "type": "call", "service": "light.turn_off", "entity": "light.hall" },
        })
    );
    assert_eq!(
        value["wires"],
        json!([
            ["when_1", "check_1"],
            ["check_1:yes", "check_2"],
            ["check_2:yes", "do_1"],
            ["do_1", "wait_1"],
            ["wait_1", "do_2"],
        ])
    );
    // The twin isn't part of what the flow does.
    let mut without = flow.clone();
    without.home_assistant = None;
    assert_eq!(flow.version(), without.version());
}

#[test]
fn the_older_way_of_writing_an_automation_reads_the_same() {
    let flow = bring(json!({
        "alias": "Hall light on motion",
        "trigger": { "platform": "state", "entity_id": ["binary_sensor.hall_motion"], "to": "on", "for": "00:00:05" },
        "condition": [],
        "action": [{ "service": "light.turn_on", "entity_id": "light.hall_ceiling" }],
    }))
    .expect("a flow");
    let value = serde_json::to_value(&flow).expect("json");
    assert_eq!(
        value["nodes"]["when_1"]["trigger"],
        json!({ "type": "state", "entity": "binary_sensor.hall_motion", "to": true, "for": "5s" })
    );
    assert_eq!(value["nodes"]["do_1"]["service"], "light.turn_on");
}

#[test]
fn device_triggers_and_actions_come_over_by_registry_id() {
    let flow = bring(json!({
        "triggers": [
            { "trigger": "device", "device_id": "d1", "domain": "binary_sensor", "entity_id": "r-hall-motion", "type": "motion" },
            { "trigger": "device", "device_id": "d1", "domain": "binary_sensor", "entity_id": "r-hall-motion", "type": "no_motion", "for": { "minutes": 5 } },
        ],
        "actions": [
            { "device_id": "d2", "domain": "light", "entity_id": "r-hall-light", "type": "toggle" },
        ],
    }))
    .expect("a flow");
    let value = serde_json::to_value(&flow).expect("json");
    assert_eq!(value["nodes"]["when_1"]["trigger"]["to"], true);
    assert_eq!(value["nodes"]["when_2"]["trigger"]["to"], false);
    assert_eq!(value["nodes"]["when_2"]["trigger"]["for"], "5m");
    assert_eq!(value["nodes"]["do_1"]["service"], "light.toggle");
    assert_eq!(
        value["wires"],
        json!([["when_1", "do_1"], ["when_2", "do_1"]])
    );
}

#[test]
fn times_sun_and_a_time_pattern_come_over() {
    let flow = bring(json!({
        "triggers": [
            { "trigger": "time", "at": "07:30:00", "weekday": ["mon", "fri"] },
            { "trigger": "sun", "event": "sunset", "offset": "-00:15:00" },
            { "trigger": "time_pattern", "minutes": "/10" },
            { "trigger": "homeassistant", "event": "start" },
        ],
        "conditions": [{ "condition": "time", "after": "06:00", "before": "23:00:00" }],
        "actions": [{ "action": "homeassistant.turn_on", "target": { "entity_id": ["light.hall_ceiling"] } }],
    }))
    .expect("a flow");
    let value = serde_json::to_value(&flow).expect("json");
    assert_eq!(
        value["nodes"]["when_1"]["trigger"],
        json!({ "type": "time", "at": "07:30", "weekday": ["mon", "fri"] })
    );
    assert_eq!(
        value["nodes"]["when_2"]["trigger"],
        json!({ "type": "sun", "event": "sunset", "offset": "-15m" })
    );
    assert_eq!(
        value["nodes"]["when_3"]["trigger"],
        json!({ "type": "time", "cron": "*/10 * * * *" })
    );
    assert!(matches!(
        &flow.nodes[&irori_types::ObjectId::try_from("when_4").expect("id")],
        Node::Trigger { trigger: Trigger::Startup {} }
    ));
    assert_eq!(
        value["nodes"]["check_1"]["condition"],
        json!({ "type": "time", "after": "06:00", "before": "23:00" })
    );
    assert_eq!(value["nodes"]["do_1"]["service"], "light.turn_on");
}

/// Issue #198: when anything can't be said as a flow, nothing comes over, and every such part
/// is listed.
#[test]
fn what_cant_come_over_is_listed_and_nothing_is_made() {
    let missing = bring(json!({
        "triggers": [
            { "trigger": "state", "entity_id": "binary_sensor.hall_motion", "to": "on" },
            { "trigger": "mqtt", "topic": "hall/button" },
            { "trigger": "template", "value_template": "{{ is_state('sun.sun', 'below_horizon') }}" },
        ],
        "conditions": [{ "condition": "zone", "entity_id": "person.ana", "zone": "zone.home" }],
        "actions": [
            { "choose": [] },
            { "action": "light.turn_on", "target": { "entity_id": "light.kitchen" } },
            { "action": "light.turn_on", "target": { "area_id": "hall" } },
            { "action": "notify.mobile_app", "data": { "message": "hi" } },
        ],
    }))
    .expect_err("nothing comes over");
    let said = missing.join("\n");
    for part in [
        "`mqtt` trigger",
        "template",
        "`zone` condition",
        "choice between actions",
        "`light.kitchen` isn't in this home yet",
        "on a whole area",
        "`notify.mobile_app` on no entity",
    ] {
        assert!(said.contains(part), "{part} isn't said in:\n{said}");
    }
}

#[test]
fn an_entity_home_assistant_didnt_bring_over_is_named() {
    let missing = bring(json!({
        "triggers": [{ "trigger": "state", "entity_id": "person.ana", "to": "home" }],
        "actions": [{ "action": "light.turn_on", "target": { "entity_id": "light.hall_ceiling" } }],
    }))
    .expect_err("not brought over");
    assert_eq!(missing, ["`person.ana` isn't brought over from Home Assistant"]);
}

#[test]
fn a_state_irori_cant_watch_for_is_said() {
    let missing = bring(json!({
        "triggers": [{ "trigger": "state", "entity_id": "binary_sensor.hall_motion", "to": "unavailable" }],
        "actions": [{ "action": "light.turn_on", "target": { "entity_id": "light.hall_ceiling" } }],
    }))
    .expect_err("unavailable isn't a value");
    assert!(missing[0].contains("`unavailable`"), "{missing:?}");
}

#[test]
fn settings_a_light_doesnt_take_are_said() {
    let missing = bring(json!({
        "triggers": [{ "trigger": "state", "entity_id": "binary_sensor.hall_motion", "to": "on" }],
        "actions": [{ "action": "light.turn_on", "target": { "entity_id": "light.hall_ceiling" }, "data": { "flash": "long" } }],
    }))
    .expect_err("flash");
    assert!(missing[0].contains("settings"), "{missing:?}");
}

#[test]
fn one_written_in_yaml_without_an_id_cant_come_over() {
    let registry = registry();
    let home = Home::new(&registry, brought());
    let mut yaml = automation();
    yaml.id = None;
    let missing = translate(
        &json!({}),
        &yaml,
        RuleId::try_from("x").expect("an id"),
        &home,
    )
    .expect_err("no id");
    assert!(missing[0].contains("without an `id`"), "{missing:?}");
}

#[test]
fn durations_read_as_irori_writes_them() {
    for (ha, irori) in [
        (json!("00:05:00"), Some("5m")),
        (json!("01:30"), Some("1h30m")),
        (json!("-00:30:00"), Some("-30m")),
        (json!(90), Some("1m30s")),
        (json!({ "hours": 1, "seconds": 5 }), Some("1h5s")),
        (json!({ "milliseconds": 500 }), Some("500ms")),
        (json!("00:00:00"), None),
        (Value::Null, None),
    ] {
        assert_eq!(duration(&ha).expect("a duration").as_deref(), irori, "{ha}");
    }
    assert!(duration(&json!("{{ states('input_number.x') }}")).is_err());
    assert!(duration(&json!({ "weeks": 1 })).is_err());
}

#[test]
fn a_time_pattern_on_seconds_cant_come_over() {
    assert_eq!(cron(&json!({ "hours": "/2" })).as_deref(), Ok("0 */2 * * *"));
    assert_eq!(cron(&json!({ "minutes": 15 })).as_deref(), Ok("15 * * * *"));
    assert!(cron(&json!({ "seconds": "/30" })).is_err());
}

#[test]
fn a_flow_id_is_free_and_a_slug() {
    let taken = |id: &RuleId| id.as_str() == "hall_light";
    assert_eq!(
        free_id("automation.hall_light", taken).expect("free").as_str(),
        "hall_light_2"
    );
    assert_eq!(
        free_id("automation.Hall--Light (new)", |_| false)
            .expect("free")
            .as_str(),
        "hall_light_new"
    );
    assert_eq!(
        free_id("automation.__", |_| false).expect("free").as_str(),
        "home_assistant"
    );
}
