# irori-protocol-matter

Matter devices on the local network, through the
[Open Home Foundation Matter Server](https://github.com/matter-js/matterjs-server) (the
matter.js server Home Assistant uses). This extension installs it beside a Node.js of its own,
runs it on loopback, and talks to it over its WebSocket API. It's the same trade Zigbee makes
with Zigbee2MQTT: a bundled runtime, in exchange for a controller that already works in a great
many homes. [Why not `rs-matter`](#why-wrap-a-server-instead-of-rs-matter) is below.

## What it does

- **Installs Node.js and the Matter server** into its own package directory the first time it
  starts (`runtime/` and `matter/`), never touching a Node.js already on the machine. About
  340 MB once installed. Linux and macOS, x86_64 or arm64, with glibc, as for Zigbee.
- **Runs the server on loopback only** (`127.0.0.1`, port 15580 by default). Its WebSocket API
  has no authentication, so nothing off this machine may reach it. Port 15580, not the server's
  usual 5580, so it doesn't collide with a Home Assistant Matter server on the same machine.
  Anything running *on* this machine can still reach it: see
  [Who can control your devices](#who-can-control-your-devices).
- **Adds a device from a pairing code**, with the extension's "Add with a pairing code" action:
  over the network, or, with Bluetooth turned on, a new device fresh out of its box (see below).
- **Shows each device and its readings**, kept up to date as the device reports them, and
  **carries out commands** on them:
  - Lights: on and off, brightness, colour temperature and colour, as the light supports.
  - Plugs, in-wall switches and appliances' power: switches.
  - What a light or plug does when the power comes back (on, off, toggle, or as it was): a
    select under the device's settings.
  - Locks: lock, unlock, and unlatch for a lock that can pull its latch. A lock that wants a PIN
    for remote use is sent the code given with the command.
  - Blinds, shades, curtains, awnings and shutters: open, close, stop, and a position and tilt
    for one that knows where it is.
  - Thermostats and radiator valves: off, heat, cool, or heat and cool between two setpoints, as
    the thermostat supports, with its own limits. Turning one on goes back to the mode it was
    last in.
  - Fans and air purifiers' fans: speed, auto and the wind presets, oscillation and direction,
    as the fan supports.
  - Each device's firmware, when it can take new firmware (see [Firmware](#firmware)).
  - Temperature, humidity, illuminance, pressure, flow, battery, power, voltage, current and
    energy: sensors in Irori's units (°C, %, lx, kPa, m³/h, W, V, A, kWh). A plug that measures
    RMS voltage and current, as most do, shows those. Voltage, current and battery are
    diagnostics, as in Home Assistant.
  - Each device's make, model, firmware and hardware versions, and serial number.
  - Occupancy, door contact, water leak, freeze and rain: binary sensors.
  - A bridge's devices (a Hue bridge, an IKEA hub) as devices of their own, reached through the
    bridge.
- **Lists what it doesn't read yet** on the device: buttons, valves and smoke alarms appear
  under "not supported yet" rather than vanishing.
- **Says who asked.** The state a command causes is reported as that command's result, so an
  automation's run and the activity log show what it changed.
- **Marks a device unavailable** when the server can't reach it, or, behind a bridge, when the
  bridge says it can't. The extension's card says how many aren't answering.
- **Removing a device takes Irori off it.** A Matter device can belong to several apps at once
  (its "fabrics"). Removing it from Irori removes only Irori's fabric; Google Home, Apple Home
  or Home Assistant keep it. A device that isn't answering can still be removed anyway: it's
  forgotten here, and keeps Irori in its list of controllers until it's reset.
- **Restarts the server** by restarting itself if the server's process ever exits: the core's
  own crash-and-backoff (`docs/specs/protocols.md` §3), as for Zigbee.

## Adding a device

"Add device" on the extension's card asks which kind of device it is, then for its code. The
code is the 11 digits (dashes and spaces are fine) or the text of a QR code (`MT:…`). A typo is
caught at once: the last digit is a check digit.

- **It's set up in another app** (the common case). Share it from there, which gives a fresh
  code good for about 15 minutes. It stays in that app too:
  - Google Home: the device → Settings → Linked Matter apps & services → Link apps & services.
  - Apple Home: the device → Settings → Turn On Pairing Mode.
  - Home Assistant: the device → Share device.
- **It's new, or reset**: the code printed on it or its box. The form also shows Bluetooth, the
  Wi-Fi network and the Thread network, the same settings as behind the gear icon, so they can be
  set right there. Each is a section that folds away once it's set:
  - A wired device, or one already on the network, needs neither.
  - A device that isn't on a network yet joins over Bluetooth, which is most new Wi-Fi devices:
    turn Bluetooth on and give the Wi-Fi network's name and password (see
    [Bluetooth](#adding-over-bluetooth)). Changing them restarts the Matter server first, which
    takes a few seconds.
  - A new Thread device joins over Bluetooth too, and needs the Thread network's dataset, which
    holds its key. That takes a border router that gives its dataset out, such as Home
    Assistant's. Apple's and Google's don't: set the device up in Apple Home or Google Home
    first, then share it. Irori running a border router of its own is
    [#86](https://github.com/nicosandller/irori_os/issues/86).

Adding takes a few seconds once the device is found. If nothing answers to the code within two
minutes (about five over Bluetooth), the action says so. The device then waits under "+ Add device" on the
Devices page, like anything else an extension finds, until it's added to the home.

## Removing a device

Remove it from the Devices page like any other. A device reached through a bridge isn't removed
on its own: remove it in the bridge's own app, or remove the bridge to take all of its devices
off Irori.

## Settings

Behind the gear icon on the Extensions page, from [`config.schema.json`](config.schema.json).
Everything has a default. Bluetooth, Wi-Fi and Thread are sections there (`x-irori-group`), folded
until opened; each says in a word what it holds.

| Field | Default | Notes |
|---|---|---|
| `matter_server_version` | 1.4.0 | Pins the server's version. Left out, the first start installs the version this extension was tested with and keeps it. A different version reinstalls. |
| `port` | 15580 | The server's loopback port. Change it only if something else here uses it. |
| `primary_interface` | the server picks | The network interface Matter devices are on (`eth0`, `wlan0`), for a machine with more than one. |
| `allow_test_devices` | off | Accept devices made with development certificates: Matter SDK samples, the dev lab's devices, a prototype. A shop-bought device never needs it. |
| `firmware_updates` | on | Offer each device's firmware updates. Off, the server doesn't look for any. |
| `bluetooth` | off | Add new devices over Bluetooth. |
| `bluetooth_adapter` | 0 | Which adapter on Linux: 0 for `hci0`. A Mac ignores it. |
| `wifi_ssid`, `wifi_password` | none | The Wi-Fi network a new Wi-Fi device joins when it's added over Bluetooth. The password is a secret. |
| `thread_dataset` | none | The Thread network a new Thread device joins, as hex. A secret. |

The two secrets are kept in Irori's `secrets.toml`, never shown again once saved, and handed to
the Matter server each time it starts.

## Adding over Bluetooth

A device fresh out of its box isn't on any network yet. Over Bluetooth, the Matter server
proves it's genuine, then gives it the network to join, Wi-Fi or Thread, and finds it there.

- Turn on `bluetooth` and give `wifi_ssid` and `wifi_password` (most Matter devices need 2.4 GHz),
  or `thread_dataset` for Thread devices. The dataset is the border router's "active operational
  dataset" in hex: Home Assistant's Thread settings show it, as does `ot-ctl dataset active -x`.
  Apple's and Google's border routers don't give theirs out; add their Thread devices in their
  own apps first, then share them.
- **Linux:** the Matter server talks to the adapter directly, so its Node.js needs two
  capabilities: `sudo setcap cap_net_raw,cap_net_admin+eip <package directory>/runtime/bin/node`
  (again after each upgrade, which reinstalls Node.js). If the server still can't use the adapter,
  BlueZ may be holding it: stop it (`sudo systemctl stop bluetooth`) or use a second adapter,
  set with `bluetooth_adapter`.
- **macOS:** the app Irori runs in (Terminal, say) needs Bluetooth permission, in System
  Settings → Privacy & Security → Bluetooth.
- Keep the device within a few metres while it's added.

## Firmware

With `firmware_updates` on, the Matter server looks up each device in the DCL, the register
where makers publish their firmware, and Irori asks it what it found a minute after starting and
every six hours after. A device that can take new firmware gets a "Firmware" entity under its
settings, which shows the version it runs and any newer one, with its release notes.

Nothing is installed until someone installs it. Installing has the server download the firmware
and hand it to the device, which says how far it has got; the device restarts to finish, and the
entity then shows the new version. "Check for updates" asks again at once.

## Networking

Matter runs over IPv6 on the local network and finds devices with mDNS.

- Irori's machine needs IPv6 turned on (link-local is enough) and must be on the same network
  segment as the devices. A firewall must let mDNS (UDP 5353) and Matter (UDP 5540) through.
- In Docker, run Irori with host networking: Matter's multicast doesn't cross Docker's bridge.
- With several network interfaces, set `primary_interface` to the one the devices are on.
- **Thread** devices (Eve, Nanoleaf, IKEA's newer ones) are reached through a Thread border
  router already in the home: an Apple TV or HomePod, a Google Nest hub, an IKEA DIRIGERA. Irori
  doesn't run one. On Linux the machine has to accept the routes the border router announces:
  set `net.ipv6.conf.<interface>.accept_ra_rt_info_max_plen = 64` (and `accept_ra = 1`),
  otherwise Thread devices are found but can't be reached.

## On the network map

Devices → Network map draws Matter's devices: Thread ones through the border routers heard over
mDNS (`_meshcop._udp`), with how well each hears its neighbours, Wi-Fi ones on their access
point, with their signal from Wi-Fi diagnostics. A Wi-Fi device without Wi-Fi diagnostics (a Tapo
plug, say) is on the map without a signal, and says it doesn't report one. A border router
carries its mDNS host, so a Nest Hub added through Google Cast is drawn once, as that device.

## Who can control your devices

The Matter server's WebSocket API has no password, and the server offers no way to set one.
Listening on loopback keeps other machines out, but **any program running on this machine can
connect to port 15580 and do anything Irori can**: unlock a lock, open a blind, add a device or
remove one. That includes every other Irori extension, since Irori doesn't yet enforce which
addresses an extension may reach (`docs/specs/extensions.md`, "Enforcement").

Home Assistant's Matter server works the same way. In practice:

- Run Irori on a machine of its own (a Pi, say), not one shared with people or programs you
  don't trust.
- Install only extensions you trust while Matter devices that matter, like locks, are on it.
- Don't forward port 15580 to another machine.

If the extension finds something already listening on its port when it starts, it refuses to
start rather than talk to it: see [Settings](#settings) for `port`.

## Where things live

- `runtime/` and `matter/` in the package directory: Node.js and the server. Safe to lose; the
  next start installs them again.
- `matter-server/` under the extension's data directory (`IRORI_EXTENSION_DATA`): **Irori's
  fabric**, its certificates, and every device added to it. This survives uninstalling and
  upgrading. Losing it strands every device on a fabric nobody holds the keys to; each would have
  to be shared with Irori again, and keeps a dead entry in its list of controllers until reset.

## Ids

A device's id is the fabric's compressed id and the device's node id, in hex
(`8E576D939A1C2B52-0000000000000003`), as Home Assistant builds it. It doesn't change when the
device is renamed or moves to another IP address. A device added again after being removed is a
new node, so a new device here, as it is on the network.

A bridged device adds the bridged device's own UniqueID, which stays the same when the bridge
renumbers its endpoints. A bridge that gives none gets its endpoint number instead (`…-ep42`),
and a device re-added to such a bridge can come back under a new id. An entity adds its endpoint
and what it is: `…-1-light`, `…-1-temperature`.

## What it doesn't do yet

- Buttons (their presses are events), valves and smoke alarms: listed, not read.
- Thermostat presets and fan modes beyond the fan's own, a thermostat's humidity, lock users and
  PIN management, and schedules.
- Running a Thread border router. Thread devices need one already in the home.
- Using a Matter server someone already runs (Home Assistant's, say). Irori would then act on
  that server's fabric: removing a device would remove it from Home Assistant.

## Why wrap a server instead of `rs-matter`

The roadmap first said "via `rs-matter`" (X17). Issue #82 weighed that against wrapping the
server, the way Zigbee wraps Zigbee2MQTT. In October 2026:

| | `rs-matter` | The Matter server (chosen) |
|---|---|---|
| Runtime | one Rust binary | Node.js beside it, as Zigbee already has |
| Install | `lan` only | full access (`host_shell`), as Zigbee |
| Controller work | Irori's to build: fabric and certificate storage, node ids, resubscribing, keeping each node's attributes, attestation | the server's: commissioning, certificates, sessions, subscriptions, attestation |
| Maturity | its commissioner landed in May 2026 and changed incompatibly in October; crates.io 0.4.1; no controller example | the controller Home Assistant ships; Matter 1.6; tagged releases |

The part that turns Matter into Irori's devices (`src/map/`) is written in Matter's own terms,
nodes, endpoints, clusters and attributes, and doesn't know where they came from. `src/client.rs`
is the whole of what the rest asks of a controller: the server's events; commissioning and
removing a node; a cluster command or an attribute write; and firmware to look for and install. A controller on `rs-matter` could take its place later without touching the
mapping, and drop the Node.js.

## Credits

The mapping follows Home Assistant's Matter integration
(`homeassistant/components/matter/`, Apache-2.0): which device types make an On/Off cluster a
light or a switch, each measurement's units, and how locks, covers, thermostats and fans are read
and commanded. Most of the node dumps in `tests/fixtures/` are
Home Assistant's own test fixtures; see [`tests/fixtures/README.md`](tests/fixtures/README.md).
