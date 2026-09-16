# irori-int-esphome

Devices running [ESPHome](https://esphome.io) firmware, over ESPHome's **native API** — the
protocol ESPHome speaks to Home Assistant. No MQTT broker, no cloud, nothing to set up: the
devices announce themselves on the local network and Irori connects.

This is the first real protocol in Irori (ROADMAP D26), chosen because ESP32 boards are easy to
flash and it runs alongside an existing Home Assistant setup without touching it.

## What it does

- **Finds devices** by mDNS (`_esphomelib._tcp`) and connects to each one.
- **Learns what they have**: lights, switches, sensors and binary sensors become Irori entities,
  with their units, device classes and, for lights, brightness and colour range.
- **Streams their state** as the device reports it — ESPHome pushes, so there's no polling.
- **Switches them.** A command goes out as a `LightCommandRequest` or `SwitchCommandRequest`;
  the new state comes back as a report, traced to whoever asked for it.
- **Survives devices going away**: entities stay, marked offline with their last value, and the
  integration reconnects (1 s, doubling to a minute).

## What it doesn't do yet

- **Encrypted devices.** ESPHome's API can require a pre-shared key. A key has to be kept
  somewhere, and that's the config dir (M0.7), so devices that want one are named in the log and
  skipped for now. Give a test device a plain `api:` block with no `encryption:` and it's picked
  up on its own.
- **Kinds Irori doesn't model**: fans, covers, climate, text sensors, numbers, selects, buttons.
  They're counted in the log and left out, not mangled into something else.
- **Devices that don't announce themselves.** Everything comes from discovery; naming a device
  by address needs the config dir too.
- **Brightness and colour from the UI.** The protocol side is done — the command carries
  brightness, colour temperature and RGB — but the Devices page only sends on and off.

## Trying it without hardware

ESPHome can compile a config for **your own machine** instead of an ESP32 (its `host` platform),
which is how this integration was developed:

```sh
pip install esphome
esphome compile testnode.yaml   # a config with host:, api:, and a few entities
.esphome/build/<name>/.pioenvs/<name>/program
```

That device doesn't announce itself over mDNS, so Irori won't find it on its own; the tests
connect to one directly instead. `src/fake_device.rs` goes further and speaks the protocol from
Rust — enough of the plaintext framing to answer a handshake, list entities, push state and take
commands — so `cargo test` covers the whole conversation with no ESPHome install at all.

## How it's put together

| | |
|---|---|
| `src/lib.rs` | discovery, and the one loop that talks to the core (its handle can't be shared) |
| `src/node.rs` | one task per device: connect, list, subscribe, command, reconnect |
| `src/map.rs` | ESPHome's model into Irori's: identity, capabilities, units, scales |

Identity is the device's **MAC address**, and an entity is that plus ESPHome's entity **key** (a
hash of its object id, stable across reboots). The protocol's own `unique_id` field was removed
upstream, and object ids now often arrive empty, so this is what's left — and it's what Home
Assistant leans on too. Rename an entity on the device and it arrives as a new one.

The protocol itself comes from the [`esphome-client`](https://crates.io/crates/esphome-client)
crate (client, mDNS, and the Noise transport for when encryption lands). It's pinned to one API
version on purpose — the crate generates types per ESPHome release — so an ESPHome update can't
silently change what Irori compiles against.
