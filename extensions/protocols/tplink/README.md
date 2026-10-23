# irori-protocol-tplink

TP-Link Tapo plugs and power strips, and Kasa firmware that speaks the same local API, read on
the local network. Nothing talks to TP-Link's cloud. **Read-only for now:** switches show what the
device says, and turning one on or off is refused with a message until switching has been tested
on real devices.

## What it does

- **Finds devices** with TP-Link's discovery probe on UDP 20002, broadcast on this network and
  sent to each address in `known_hosts`. A broadcast doesn't cross into another VLAN, so plugs on
  an IoT network go in `known_hosts`.
- **Logs in over KLAP**, the local transport Tapo and recent Kasa firmware use: two handshakes
  prove both ends hold the same hash of the TP-Link account, then each request is AES-128-CBC
  encrypted and signed. The account and its hash never cross the network.
- **Reads each device every `poll_seconds`** (15 by default) and reports what changed.
- **A plug** is one device: its relay (named after the device), power, voltage, current, today's
  energy and total energy where it meters, its LED, and its Wi-Fi signal.
- **A strip** is a device with its LED and signal, and each outlet is a device reached through it,
  with its own switch and meter. A strip's own reading is left out: on Tapo strips it repeats
  its first outlet's.
- **Ids come from the MAC**, and an outlet's from its position on the strip, so they survive
  renaming in the app, a new address, and re-adding.
- **A device that misses two readings** is marked unavailable, and comes back on its next
  reading. An expired session logs in again without going unavailable.

## Settings

The account is secret, so it goes in `secrets.toml`; the rest can go in `extensions/tplink.toml`.

```toml
# secrets.toml
[tplink]
username = "you@example.com"   # the account the devices were set up with
password = "…"
```

```toml
# extensions/tplink.toml
known_hosts = ["192.168.30.21", "plug.home.arpa"]
poll_seconds = 15   # 5 to 300
```

A known host has to be a private, loopback, or link-local address, or a name under `.local`,
`.home.arpa`, `.internal`, `.lan`, or `.home`. A port, a public address, or an internet name is
refused, and no refusal repeats the value.

Without an account, each device found is listed as waiting for one. A device that refuses the
account is listed the same way and tried again every ten minutes, so a wrong password doesn't
flood it.

A device with no account set on it (reset, or never added in the app) still answers TP-Link's
setup login or a blank one; those are tried when the account doesn't match, as python-kasa does.

## Tested on

Tapo metering plugs and power strips on current firmware, read alongside Home Assistant's
TP-Link integration: the same relay states, today's energy to the watt-hour, and power, voltage
and current within a reading's drift.

## Left out

- **Switching**, until it has been tested.
- **The older Kasa protocol** (TCP 9999, XOR "encryption") used by HS100, HS110, KP303 and
  older Kasa firmware, and Tapo's older AES transport. Such a device is found and logged, not
  read.
- **Bulbs, light strips, hubs, cameras.** Plugs and strips only.
- **Device settings** beyond the LED: auto-off, power protection, child lock.
