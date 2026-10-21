# irori-protocol-matter

Matter devices on the local network, through the
[Open Home Foundation Matter Server](https://github.com/matter-js/matterjs-server) (the
matter.js server Home Assistant uses). This extension installs it beside a Node.js of its own,
runs it on loopback, and talks to it over its WebSocket API. It's the same trade Zigbee makes
with Zigbee2MQTT: a bundled runtime, in exchange for a controller that already works in a great
many homes. [Why not `rs-matter`](#why-wrap-a-server-instead-of-rs-matter) is below.

**Read-only for now.** Devices show up with their live state, but switching them is refused
until it has been tried on real devices.

## What it does

- **Installs Node.js and the Matter server** into its own package directory the first time it
  starts (`runtime/` and `matter/`), never touching a Node.js already on the machine. About
  340 MB once installed. Linux and macOS, x86_64 or arm64, with glibc, as for Zigbee.
- **Runs the server on loopback only** (`127.0.0.1`, port 15580 by default). Its WebSocket API
  has no authentication, so nothing off this machine may reach it. Port 15580, not the server's
  usual 5580, so it doesn't collide with a Home Assistant Matter server on the same machine.
- **Adds a device from a pairing code**, with the extension's "Add with a pairing code" action.
  Over the network only (see below).
- **Shows each device and its readings**, kept up to date as the device reports them:
  - Lights: on and off, brightness, colour temperature and colour, as the light supports.
  - Plugs, in-wall switches and appliances' power: switches.
  - Temperature, humidity, illuminance, pressure, flow, battery, power, voltage, current and
    energy: sensors in Irori's units (°C, %, lx, kPa, m³/h, W, V, A, kWh).
  - Occupancy, door contact, water leak, freeze and rain: binary sensors.
  - A bridge's devices (a Hue bridge, an IKEA hub) as devices of their own, reached through the
    bridge.
- **Lists what it doesn't read yet** on the device: locks, covers, thermostats, fans, buttons,
  valves and smoke alarms appear under "not supported yet" rather than vanishing.
- **Marks a device unavailable** when the server can't reach it, and says how many aren't
  answering on the extension's card.
- **Removing a device takes Irori off it.** A Matter device can belong to several apps at once
  (its "fabrics"). Removing it from Irori removes only Irori's fabric; Google Home, Apple Home
  or Home Assistant keep it. A device that isn't answering can still be removed anyway: it's
  forgotten here, and keeps Irori in its list of controllers until it's reset.
- **Restarts the server** by restarting itself if the server's process ever exits: the core's
  own crash-and-backoff (`docs/specs/protocols.md` §3), as for Zigbee.

## Adding a device

The code goes into "Add with a pairing code" on the extension's card. It takes the 11 digits
(dashes and spaces are fine) or the text of a QR code (`MT:…`). A typo is caught at once: the
last digit is a check digit.

- **A device already set up in another app** (the common case). Share it from there, which
  gives a fresh code good for about 15 minutes:
  - Google Home: the device → Settings → Linked Matter apps & services → Link apps & services.
  - Apple Home: the device → Settings → Turn On Pairing Mode.
  - Home Assistant: the device → Share device.
- **A new device that's already on the network** (Wi-Fi or Ethernet): the 11 digits printed
  under its QR code.
- **A new device that joins over Bluetooth**, which is most new Wi-Fi and Thread devices, can't
  be added directly yet. Set it up in another app first, then share it.

Adding takes a few seconds once the device is found. If nothing on the network answers to the
code within two minutes, the action says so.

## Removing a device

Remove it from the Devices page like any other. A device reached through a bridge isn't removed
on its own: remove it in the bridge's own app, or remove the bridge to take all of its devices
off Irori.

## Settings

Behind the gear icon on the Extensions page, from [`config.schema.json`](config.schema.json).
Everything has a default.

| Field | Default | Notes |
|---|---|---|
| `matter_server_version` | 1.4.0 | Pins the server's version. Left out, the first start installs the version this extension was tested with and keeps it. A different version reinstalls. |
| `port` | 15580 | The server's loopback port. Change it only if something else here uses it. |
| `primary_interface` | the server picks | The network interface Matter devices are on (`eth0`, `wlan0`), for a machine with more than one. |
| `allow_test_devices` | off | Accept devices made with development certificates: Matter SDK samples, the dev lab's devices, a prototype. A shop-bought device never needs it. |

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

- Switching, dimming and every other command. Next, once tried on real devices.
- Commissioning over Bluetooth, and giving a device Wi-Fi or Thread credentials.
- Firmware updates (the server's OTA is turned off until it can be offered as the `update` kind).
- Locks, covers, thermostats, fans, buttons and valves: listed, not read.
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

The part that turns Matter into Irori's devices (`src/map.rs`) is written in Matter's own terms,
nodes, endpoints, clusters and attributes, and doesn't know where they came from. `src/client.rs`
is the whole of what the rest asks of a controller: the server's events, and commissioning and
removing a node. A controller on `rs-matter` could take its place later without touching the
mapping, and drop the Node.js.

## Credits

The mapping follows Home Assistant's Matter integration
(`homeassistant/components/matter/`, Apache-2.0): which device types make an On/Off cluster a
light or a switch, and each measurement's units. Most of the node dumps in `tests/fixtures/` are
Home Assistant's own test fixtures; see [`tests/fixtures/README.md`](tests/fixtures/README.md).
