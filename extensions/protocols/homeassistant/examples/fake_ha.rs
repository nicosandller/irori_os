//! A fake Home Assistant for the load gate (`dev/ha-load`): a home of `--devices` devices with
//! `--per-device` entities each, whose sensors change `--rate` times a second in all.
//!
//! ```text
//! cargo run -p irori-protocol-homeassistant --features fake --example fake_ha -- \
//!     --port 18123 --devices 200 --per-device 10 --rate 300 --token load-token
//! ```
//!
//! `--not-admin` makes the token a non-administrator's, to see what an Irori says about it.
//! `--automations` adds three automations on the first device: one that can be brought over as
//! a flow, one that can't yet (it uses a template), and one written in YAML without an `id`.
//!
//! Prints `listening <url>` once it serves, then once a second how many changes it has sent.

use std::time::Duration;

use irori_protocol_homeassistant::fake::{FakeHa, World};
use serde_json::json;

fn arg(name: &str, default: u64) -> u64 {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn text_arg(name: &str, default: &str) -> String {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| default.to_owned())
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let port = u16::try_from(arg("--port", 0)).unwrap_or(0);
    let devices = usize::try_from(arg("--devices", 200)).unwrap_or(200);
    let per_device = usize::try_from(arg("--per-device", 10)).unwrap_or(10);
    let rate = arg("--rate", 300);
    let token = text_arg("--token", "load-token");
    let mut world = World::home(&token, devices, per_device);
    // `--not-admin`: the token's user can sign in but not read the registries.
    world.admin = !std::env::args().any(|a| a == "--not-admin");
    if std::env::args().any(|a| a == "--automations") && devices > 0 {
        automations(&mut world);
    }
    let sensors: Vec<String> = world
        .states
        .keys()
        .filter(|id| id.starts_with("sensor."))
        .cloned()
        .collect();
    let fake = FakeHa::start_on(world, port).await?;
    println!("listening {}", fake.url);
    // Ten bursts a second, `rate / 10` sensors each, chosen by a fixed sequence.
    let per_tick = usize::try_from(rate / 10).unwrap_or(0);
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut sent: u64 = 0;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut ticks: u64 = 0;
    loop {
        tick.tick().await;
        ticks += 1;
        if !sensors.is_empty() {
            for _ in 0..per_tick {
                // Two draws: one number for both would tie each sensor to one value whenever
                // the count of sensors is a multiple of the values' range.
                let pick = usize::try_from(next(&mut seed) % sensors.len() as u64).unwrap_or(0);
                let value = format!("{:.1}", (next(&mut seed) % 400) as f64 / 10.0);
                fake.set_state(&sensors[pick], &value, json!({}));
                sent += 1;
            }
        }
        if ticks.is_multiple_of(10) {
            println!("sent {sent} connections {}", fake.connections());
        }
    }
}

/// Three automations on the first device's light and motion sensor.
fn automations(world: &mut World) {
    world
        .automation(
            "automation.hall_light_on_motion",
            "Hall light on motion",
            true,
            Some("1700000000001"),
            json!({
                "id": "1700000000001",
                "alias": "Hall light on motion",
                "mode": "restart",
                "triggers": [{ "trigger": "state", "entity_id": "binary_sensor.d0_motion", "to": "on" }],
                "conditions": [{ "condition": "sun", "after": "sunset" }],
                "actions": [
                    { "action": "light.turn_on", "target": { "entity_id": "light.d0" } },
                    { "delay": { "minutes": 5 } },
                    { "action": "light.turn_off", "target": { "entity_id": "light.d0" } }
                ]
            }),
        )
        .automation(
            "automation.warm_evening",
            "Warm evening",
            true,
            Some("1700000000002"),
            json!({
                "id": "1700000000002",
                "alias": "Warm evening",
                "triggers": [{ "trigger": "template", "value_template": "{{ states('sensor.d0_temperature_2') | float > 25 }}" }],
                "actions": [{ "action": "light.turn_on", "target": { "entity_id": "light.d0" }, "data": { "brightness_pct": 40 } }]
            }),
        )
        .automation(
            "automation.from_yaml",
            "Written in YAML",
            false,
            None,
            json!({}),
        );
}

/// The next number of a xorshift sequence.
fn next(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}
