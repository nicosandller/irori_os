# irori-protocol-zigbee

Zigbee devices, through a [Zigbee2MQTT](https://www.zigbee2mqtt.io) instance this extension
installs, configures, and runs for you — no separate Z2M install, no broker of your own, no Z2M
frontend to open. This is D46 in `ROADMAP.md`: the project's first exception to "single static
binary with no bundled runtime," because no Rust Zigbee radio stack exists to build a native
protocol on the way `esphome-client`/`rumqttc` let ESPHome and MQTT work.

**The `mqtt` extension is not a prerequisite, and installing it alongside changes nothing here.**
This one brings its own broker and its own discovery client; the two share parsing code, not a
running process. Install `mqtt` when you have a broker of your own — Tasmota, ESPHome-over-MQTT,
a Zigbee2MQTT you run yourself — and this one when you have a Zigbee dongle and want Irori to
handle the rest.

## What it does

- **Installs Node.js and Zigbee2MQTT** into its own package directory the first time it starts —
  never touching any Node.js already on the machine. Linux and macOS, x86_64 or arm64, with
  glibc; see [`src/provision.rs`](src/provision.rs) for why a musl-only host (Alpine, say) isn't
  supported.
- **Runs its own embedded broker** (in-process, via `rumqttd`, loopback-only) purely so its own
  Zigbee2MQTT has somewhere to publish to. It's never reachable from the network and isn't a
  general-purpose broker — that's the `mqtt` extension, against a real one.
- **Speaks HA MQTT Discovery** against that broker exactly like the `mqtt` extension does against
  an external one — the same `light`/`switch`/`sensor`/`binary_sensor` support, the same
  availability handling, the parsing itself shared via `irori-ha-discovery`.
- **Permit joining**, from Irori's own "+ Add device" flow, never Z2M's frontend: a new device
  goes into pairing mode, you press the button, it shows up.
- **Restarts Zigbee2MQTT** by restarting itself if the Z2M process ever exits — the same
  crash-and-backoff the core already gives every extension (`docs/specs/protocols.md` §3), so
  there's no second retry loop to get wrong inside this one.

## Settings

Behind the gear icon on the Extensions page (auto-generated from
[`config.schema.json`](config.schema.json)):

| Field | Required | Notes |
|---|---|---|
| `serial_port` | yes | The dongle's device path. Irori can't list what's plugged in yet — `ls /dev/tty*` (Linux) or `/dev/cu.*` (macOS) with the dongle in and out is how to find it. |
| `adapter` | no, default `ember` | The dongle's radio chip family: `ember` (most current Silicon Labs–based dongles), `zstack` (Texas Instruments), `deconz` (ConBee/RaspBee), `zboss`. |
| `channel`, `pan_id` | no | Left to Zigbee2MQTT's own defaults if omitted. |
| `network_key` | no | Hex (32 digits) — only needed to join an existing network. Left out, Zigbee2MQTT generates and keeps its own. A secret: never sent back once given. |
| `zigbee2mqtt_version` | no | Pins a version. Left out, the first start installs current and remembers exactly which — updating after that is a deliberate re-install, never silent. |
| `broker_port` | no, default `17883` | The embedded broker's own loopback port. Only change it if something else on the machine already uses the default. |

## Known limits

- **Linux/macOS with glibc only.** No musl, no Windows.
- **No live serial-port picker.** The settings form is a plain text field for `serial_port` —
  same limit ESPHome's own "no key list" note has for a different field.
- **A `SIGKILL`ed parent can orphan the Zigbee2MQTT child.** Graceful stops (the normal path)
  clean it up (`kill_on_drop`); an un-catchable kill of this extension's own process doesn't run
  that cleanup. Solving this fully needs platform-specific process-group code this project's own
  `unsafe_code = "forbid"` rule rules out; a known, accepted trade rather than an oversight.
- **Docker was considered and rejected** in favor of raw Node.js/pnpm, accepting the native-module
  fragility across architectures that Zigbee2MQTT's own maintainers use Docker specifically to
  avoid (`ROADMAP.md` D46). Revisit if this proves unreliable in practice.
