# Irori beside Home Assistant, on one machine

You can try Irori without a spare machine. Install it on the Raspberry Pi, mini PC or Linux box
that already runs Home Assistant, and point it at that Home Assistant with the bridge
(`extensions/protocols/homeassistant`). Home Assistant keeps running your home. Irori reads it,
and can pass on the commands you give in Irori.

This page is for **Home Assistant Container** (Home Assistant in Docker) and **Home Assistant
Core** (Home Assistant in a Python environment). Both run on an ordinary Linux system, so Irori
installs beside them the same way: as its own program, with its own system service.

It needs Irori 1.7.0 or later: the first release with the Home Assistant extension. Leaving the
address empty needs that extension's version 0.5.0 or later.

**Home Assistant OS** (a Green, a Yellow, or Home Assistant's own image on a Pi or a VM) runs only
what its app store installs, so Irori can't be installed beside it this way. Install the Irori
app instead (`install/home-assistant/irori`, and its `DOCS.md`).

Rather run Irori in Docker too? See [In Docker](#in-docker) below.

## What stays apart

| | Home Assistant | Irori |
|---|---|---|
| Web page | port 8123 | port 8480, on `127.0.0.1` only unless you open it up (below). If 8480 is taken, Irori steps up to the next free port up to 8489 and logs which one it took. |
| Data | its config folder (`/config`, or `~/.homeassistant`) | `/var/lib/irori`. Irori never reads or writes Home Assistant's folder. |
| Program | its container or Python environment | one file, `/usr/local/bin/irori`. Nothing to install from your distribution. |
| User | its own | its own `irori` system user |
| USB radios (Zigbee, Z-Wave, Thread dongles) | uses them | never opens them while you only use the bridge (below) |
| Bluetooth | uses it | never uses it while you only use the bridge (below) |
| mDNS (`.local` names) | answers and browses | the core doesn't advertise anything |

Some extensions you might add later run helper programs. Their ports are picked so they don't
meet Home Assistant's add-ons and companions:

- **Matter**: its Matter server listens on `127.0.0.1:15580`, not 5580, which a Home Assistant
  Matter server uses.
- **Zigbee**: its own MQTT broker is on port 17883, not 1883 (Mosquitto's), and the Zigbee2MQTT it
  runs has its web page turned off, so port 8080 stays free.

## Install

These steps are the same next to Home Assistant Container and Home Assistant Core. You need a
shell on the machine (`ssh`) and `sudo`.

1. Install the program system-wide, into `/usr/local/bin`:

   ```sh
   curl -fsSL https://github.com/nicosandller/irori_os/releases/latest/download/install.sh \
     | sudo bash -s -- --system
   ```

2. Give Irori a user of its own and start it as a service. The unit file runs Irori as that user,
   with its data in `/var/lib/irori`, and starts it on every boot:

   ```sh
   sudo useradd --system --home-dir /var/lib/irori --create-home --shell /usr/sbin/nologin irori
   curl -fsSL https://raw.githubusercontent.com/nicosandller/irori_os/main/install/irori.service \
     | sudo tee /etc/systemd/system/irori.service >/dev/null
   sudo systemctl enable --now irori
   ```

3. Open Irori's page. It listens on the machine itself only, so from your computer, open a tunnel
   and browse to <http://127.0.0.1:8480>:

   ```sh
   ssh -L 8480:127.0.0.1:8480 you@your-home-assistant-machine
   ```

   The welcome asks who you are, with a password. From then on Irori asks who is there before it
   shows anything.

4. Connect Home Assistant. Install the **Home Assistant** extension from the Extensions page, and in
   its settings:

   - **Leave "Home Assistant address" empty.** Irori then uses the Home Assistant on this machine,
     at `http://127.0.0.1:8123`. The token never leaves the machine, so plain `http://` is fine
     here.
   - **Access token**: in Home Assistant, as an administrator, open your profile → Security →
     Long-lived access tokens → Create token, and paste it.
   - Turn off **Let Irori control devices** if you only want to look.

   Then bring over Home Assistant's floors and rooms, and add its devices, from the extension's
   page.

### Reaching Irori without a tunnel

To open Irori from your phone, let it listen on the network once the welcome has set a password,
and serve it over https so that password travels encrypted:

```sh
sudo systemctl edit irori
```

```ini
[Service]
Environment=IRORI_BIND=0.0.0.0:8480
Environment=IRORI_ALLOW_UNAUTHENTICATED_LAN=true
Environment=IRORI_TLS=true
```

```sh
sudo systemctl restart irori
```

`IRORI_ALLOW_UNAUTHENTICATED_LAN` is the switch Irori asks for before it listens beyond the
machine itself. Then open `https://<machine>:8480`. Your browser asks once about the certificate
Irori made for itself.

### When Home Assistant isn't at `127.0.0.1:8123`

With the address left empty, the bridge connects to `127.0.0.1:8123` and nowhere else. If nothing
answers there, the extension's page says "no Home Assistant answered on this machine" and asks
for the address. That happens when:

- **Home Assistant serves https** (`ssl_certificate` in its `http:` settings). Enter
  `https://127.0.0.1:8123`, and under "Trust this certificate" its SHA-256 fingerprint or the
  certificate itself.
- **Home Assistant uses another port** (`server_port`). Enter `http://127.0.0.1:<port>`.
- **Home Assistant Core listens on one address only** (`server_host` in its `http:` settings).
  Enter that address, for example `http://192.168.1.20:8123`.
- **Home Assistant Container doesn't publish 8123 on the machine.** Home Assistant's own
  instructions run it with `--network=host` (or `network_mode: host` in compose), which does.
  With `-p 8123:8123` it's reachable too.

## In Docker

Irori's image, `ghcr.io/nicosandller/irori` (published from 1.7.0 on), holds the same program and the official extensions,
for Intel/AMD and ARM machines. [`install/docker/compose.yaml`](../install/docker/compose.yaml)
runs it with host networking, the way Home Assistant Container itself runs, and keeps the home in
a Docker volume:

```sh
curl -fsSLO https://raw.githubusercontent.com/nicosandller/irori_os/main/install/docker/compose.yaml
docker compose up -d
```

It listens on `127.0.0.1:8480` like the service above, so steps 3 and 4 are the same: a tunnel the
first time, then the Home Assistant extension with the address left empty. To reach it without a
tunnel, add the same three variables under `environment:` in the compose file.

## USB radios and Bluetooth stay Home Assistant's

A Zigbee dongle or a Bluetooth adapter can only be used by one program at a time. While you only
use the bridge, Irori never opens either:

- **The core** has no Bluetooth or serial code. It only lists the names under `/dev`, to suggest
  one in a settings field.
- **The bridge** talks to Home Assistant over the network and nothing else. Its manifest asks for
  the local network only, and a test fails if that ever changes.
- **Every other extension** runs only once you install it. Nothing is installed on its own.
- **The `irori` user** from step 2 isn't in the `dialout` group, which USB serial dongles belong
  to on Debian and Raspberry Pi OS, so it can't open one even by mistake. It isn't in the
  `bluetooth` group either. Irori installed for your own user (without `--system`) runs as you and
  doesn't have this protection.

Installing one of these brings the clash back:

- **Zigbee** opens the dongle you choose in its settings, and Home Assistant's ZHA or
  Zigbee2MQTT then loses it. Give each its own dongle. Handing devices over from Home Assistant
  to Irori is #168.
- **SwitchBot** reads nearby devices over Bluetooth by default. Next to Home Assistant's
  Bluetooth, turn off "Read nearby devices over Bluetooth" in its settings.

## Memory and CPU

Measured with `dev/ha-load --release`, a release build run natively on an Apple Silicon Mac, not
yet on a small machine. A fake Home Assistant stood in for the real one, with its sensors changing
300 times a second in all. The figures are Irori's core (the `irori` process) over a 60-second
window:

| | CPU | Memory |
|---|---|---|
| Irori with the Demo and Automations extensions, no Home Assistant | 0.4% of a core | 27 MB |
| The same, bridging a Home Assistant of 2,000 entities | 4.6% of a core | 54 MB |

The bridge's own process stayed under 1% of a core.

Not measured yet: Irori on a real small machine such as a Raspberry Pi, Home Assistant and Irori
there together, Home Assistant's own share (it depends on its integrations), and the bridge's
memory. A Pi's core is slower than a Mac's, so expect higher CPU figures there.
