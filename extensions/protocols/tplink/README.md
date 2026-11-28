# irori-protocol-tplink

TP-Link Tapo plugs and power strips, and Kasa firmware that speaks the same local API, read and
switched on the local network. Nothing talks to TP-Link's cloud.

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
- **Switches a plug, and each outlet of a strip**, on and off: see below.
- **Ids come from the MAC**, and an outlet's from its position on the strip, so they survive
  renaming in the app, a new address, and re-adding.
- **A device that misses two readings** is marked unavailable, and comes back on its next
  reading. An expired session logs in again without going unavailable.

## Switching

| | Turned on and off from Irori |
|---|---|
| A plug's relay | yes |
| Each outlet of a strip | yes, one at a time, through the strip |
| A strip as a whole | no: it has no relay of its own, so switch its outlets |
| The LED | no: it is shown, and set in the Tapo or Kasa app |
| Anything that isn't a plug or a strip (a bulb, a wall switch) | no: it is read if it answers, and never sent a command |

- **What is shown is what the device says.** A command goes to the device, and the device is read
  again straight away; that reading is what Irori shows, whether or not it matches what was asked
  for. Nothing is shown as switched because it was asked to.
- **A device that refuses** gives the command an error with the device's own error code, and
  stays shown as it last reported.
- **A device that can't be reached, or doesn't answer within 8 seconds,** gives the command an
  error. A command that ran out of time is never sent late or sent twice: the only request tried
  again is one the device turned away because its session had ended, which it didn't act on. A
  command cut off mid-answer may still have reached the device; the next reading shows which.
- **Commands wait their turn** behind the device's readings, one device at a time, so a command
  sent while a device has hung can wait up to 5 seconds before it is answered.

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

A known host has to be a private, loopback, or link-local IPv4 address, or a name under
`.local`, `.home.arpa`, `.internal`, `.lan`, or `.home`. A port, an IPv6 address, a public
address, or an internet name is refused, and no refusal repeats the value. TP-Link devices are
found and read over IPv4 only.

Without an account, each device found is listed as waiting for one. A device that refuses the
account is listed the same way and tried again every ten minutes, so a wrong password doesn't
flood it.

A device with no account set on it (reset, or never added in the app) still answers TP-Link's
setup login or a blank one; those are tried when the account doesn't match, as python-kasa does.

Those logins are published, so anything on the network can answer to them. A device that has
proved it holds the account is therefore never followed to a new address on a setup login: it is
asked to prove the account there first, and until it does it stays where it was, with a line in
the log. A plug that was reset on purpose and now sits at a new address is picked up again when
it is added back to the account, or when the extension restarts.

## Tested on

Tapo metering plugs and power strips on current firmware, read alongside Home Assistant's
TP-Link integration: the same relay states, today's energy to the watt-hour, and power, voltage
and current within a reading's drift.

**Switching has not been tried on a real device.** It is tested against a fake device in this
package's tests, and the request it sends (`set_device_info` with `device_on`, on its own, and
inside `control_child` for a strip's outlet) is the one python-kasa sends.

## Left out

- **Switching the LED**, a strip as a whole, and any device that isn't a plug or a strip.
- **The older Kasa protocol** (TCP 9999, XOR "encryption") used by HS100, HS110, KP303 and
  older Kasa firmware, and Tapo's older AES transport. Such a device is found and logged, not
  read.
- **Bulbs, light strips, hubs, cameras.** Plugs and strips only.
- **Device settings** beyond the LED: auto-off, power protection, child lock.
