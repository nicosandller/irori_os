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
- **Talks to encrypted devices** once it has their key (below).

## A note on trust

Plain ESPHome has **no authentication of its own**. A device doesn't prove who it is, and Irori
connects to whatever announces itself as one. On a home network that's the same trust Home
Assistant extends, and it's why this is usable with no setup at all — but it is a real limit,
not a detail:

- Anything on the network can advertise `_esphomelib._tcp` and have its entities adopted.
- A hostile host could claim a device id that belongs to a real device.

The fix is encryption, which does authenticate: give a device an `api: encryption: key:` in its
YAML and Irori only believes it if it holds that key. For devices left on plaintext, Irori won't
listen beyond this machine without `--allow-unauthenticated-lan`, every plaintext connection says
so in the log, and the limit is recorded as decision D29 rather than left to be discovered.

## Encrypted devices

A device that announces `api_encryption` is **not** connected to until Irori has its key. It shows
up under **Devices → Add device → Found, and waiting for you**, by the name it announced; paste the
`key:` from its YAML and Irori restarts the ESPHome integration with it (a few seconds, during
which every ESPHome device reconnects).

The key is written to `secrets.toml` in the config directory, keyed by the device's MAC address:

```toml
[esphome.keys]
"00:11:22:33:44:55" = "QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkI="
```

You can write that by hand instead — the MAC in any common form (`00:11:22:33:44:55`,
`001122334455`) — and it's picked up within a couple of seconds.

- **A wrong key** isn't retried: the device goes back to waiting with "the encryption key doesn't
  match", ready for another one.
- **A key that isn't a key** (not base64, not 32 bytes) keeps only its own device waiting, with the
  reason. The other devices carry on.
- **Keys are never shown back, never logged**, and `secrets.toml` is readable only by Irori's user.

One gap, from the client library: firmware old enough not to announce `api_encryption` over mDNS
looks *unreachable* rather than *waiting*, because `esphome-client` 0.2.1 swallows the error that
would say otherwise. Current firmware announces it.

## What it doesn't do yet

- **Kinds Irori doesn't model**: fans, covers, climate, text sensors, numbers, selects, buttons.
  They're counted in the log and left out, not mangled into something else.
- **Devices that don't announce themselves.** Everything comes from discovery; giving a device's
  address by hand is still to come.
- **Choosing which devices to adopt.** Every plaintext device found is connected to.
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

For the encrypted flow there's a quicker way, with no ESPHome install: an encrypted stand-in
device, announced over mDNS the way firmware does (macOS's `dns-sd`; `avahi-publish` on Linux):

```sh
cargo test -p irori-int-esphome -- --ignored --nocapture an_encrypted_device_to_try
# prints its port and key; then, in another terminal:
dns-sd -P "Test lock" _esphomelib._tcp local <port> testlock.local 127.0.0.1 \
    mac=aabbccddeeff "friendly_name=Test lock" api_encryption=Noise_NNpsk0_25519_ChaChaPoly_SHA256
```

A compiled `host` device doesn't announce itself over mDNS, so Irori won't find it on its own; the tests
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
