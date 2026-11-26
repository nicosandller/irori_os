//! A fake Home Assistant for the load gate (`dev/ha-load`): a home of `--devices` devices with
//! `--per-device` entities each, whose sensors change `--rate` times a second in all.
//!
//! ```text
//! cargo run -p irori-protocol-homeassistant --features fake --example fake_ha -- \
//!     --port 18123 --devices 200 --per-device 10 --rate 300 --token load-token
//! ```
//!
//! Prints `listening <url>` once it serves, then once a second how many changes it has sent.
//!
//! `--twin` adds a Zigbee plug in the kitchen (IEEE address `0x00158d0001a2b3c4`), with a switch,
//! a power sensor and a signal sensor: the same plug Irori's own MQTT extension reaches through
//! Zigbee2MQTT, for trying a handover (`docs/specs/config.md` §3.11) by hand.

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
    if std::env::args().any(|a| a == "--twin") {
        twin(&mut world);
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

/// A Zigbee plug in the kitchen, as Home Assistant's ZHA would have it.
fn twin(world: &mut World) {
    world.device(json!({
        "id": "kitchenplug",
        "name": "Kitchen plug",
        "manufacturer": "TuYa",
        "model": "TS011F",
        "area_id": "kitchen",
        "connections": [["zigbee", "00:15:8d:00:01:a2:b3:c4"]],
    }));
    world
        .entity(
            "twin1",
            "switch.kitchen_plug",
            Some("kitchenplug"),
            "on",
            json!({}),
        )
        .entity(
            "twin2",
            "sensor.kitchen_plug_power",
            Some("kitchenplug"),
            "12.5",
            json!({
                "device_class": "power",
                "unit_of_measurement": "W",
                "state_class": "measurement",
            }),
        )
        .named("Power")
        .entity(
            "twin3",
            "sensor.kitchen_plug_rssi",
            Some("kitchenplug"),
            "-61",
            json!({
                "device_class": "signal_strength",
                "unit_of_measurement": "dBm",
            }),
        )
        .named("Signal strength");
}

/// The next number of a xorshift sequence.
fn next(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}
