# irori-protocol-airgradient

AirGradient air monitors, read on the local network over the HTTP API each monitor serves itself.
Nothing talks to AirGradient's cloud. **Read-only:** the only thing ever sent to a monitor is the
request for its current readings.

**Not yet tried on a real monitor.** It is written from AirGradient's published description of
the local API (`docs/local-server.md` in its firmware repository, for firmware 3.0.10 onwards)
and tested against a fake monitor in the test code.

## What it does

- **Reads each monitor in `known_hosts`** with `GET /measures/current`, every `poll_seconds`
  (60 by default), and reports what changed.
- **A monitor is one device**, identified by the serial number it reports, so it survives a new
  address. Two known hosts that turn out to be the same monitor are one device.
- **A reading becomes a sensor the first time the monitor gives it.** One the monitor leaves out
  or sends as `null` is absent, never zero: a model without that sensor never shows it, and a
  reading that goes missing later is unknown until it is back.
- **A monitor that misses two readings** is marked unavailable and shown in the extension's
  health. It is asked again, less often, up to every five minutes (or every poll, if that is
  longer), and comes back on its next reading.
- **Everything a monitor sends is bounded:** ten seconds for a whole exchange, 64 KB for an
  answer, 32 known hosts, and 32 serial numbers in one run. An answer that isn't status 200 with
  a JSON object carrying a serial number is not a reading. A redirect is never followed.

## Readings

| Field in the monitor's answer | Sensor | Unit |
|---|---|---|
| `rco2` | Carbon dioxide | ppm |
| `pm01`, `pm02`, `pm10` | PM1, PM2.5, PM10 | µg/m³ |
| `pm02Compensated` | PM2.5, corrected | µg/m³ |
| `pm003Count` | PM0.3 count | particles/dL |
| `atmp`, `atmpCompensated` | Temperature; Temperature, corrected | °C |
| `rhum`, `rhumCompensated` | Humidity; Humidity, corrected | % |
| `tvocIndex`, `noxIndex` | VOC index, NOx index | none: Sensirion's indexes, not concentrations |
| `wifi` | Signal strength (diagnostic) | dBm |

The monitor's own value and its corrected one are both shown, as the monitor gives them; on an
indoor monitor the corrected temperature and humidity repeat the uncorrected ones.

AirGradient's description gives no unit for `rhum` and `wifi`; percent and dBm are taken from
its example values.

## Settings

```toml
# extensions/airgradient.toml
known_hosts = ["10.0.30.21", "airgradient_0a1b2c3d4e5f.local"]
poll_seconds = 60   # 10 to 3600
```

A known host is a private, loopback, or link-local IPv4 address, or a name under `.local`,
`.home.arpa`, `.internal`, `.lan`, or `.home`. A port, an IPv6 address, a public address, or an
internet name is refused, and no refusal repeats the value. A name is looked up again at every
reading, and only an address on the local network is ever asked.

A monitor names itself `airgradient_<serial number>.local`. Whether that name can be used here
depends on this machine resolving `.local` names, which hasn't been tried and which Irori's
static builds may not do; the monitor's address always works.

## Left out

- **Finding monitors by itself.** Monitors announce themselves over mDNS, but the extensions
  share no way to listen for that yet, so every monitor is listed in `known_hosts`.
- **Changing a monitor's settings:** the LED bar, the display, the temperature unit, calibration,
  "post data to AirGradient". `GET /config` isn't read and `PUT /config` is never sent.
- **Fields left unread:** `tvocRaw` and `noxRaw` (no unit given), the `…Standard` particulate
  values and the particle counts other than 0.3 µm, `boot`, `bootCount` and `ledMode`.
- **Older firmware** than 3.0.10, which has no local API.
