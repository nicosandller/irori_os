# dev/ — a Raspberry Pi in a container

Irori targets the Raspberry Pi. This directory lets you run and test it on a Mac (or any
Docker host) in an environment that behaves like one, so you can try a pull request before
approving it.

| | Raspberry Pi 4/5 | `dev/pi` container |
|---|---|---|
| CPU architecture | arm64 (aarch64) | `linux/arm64` (native on Apple Silicon) |
| OS | Raspberry Pi OS (Debian trixie) | `debian:trixie-slim` |
| Binary | static musl `irori` | the same kind of static musl `irori`, built in the container |
| Runs as | unprivileged `irori` user | unprivileged `irori` user |
| Data | `/var/lib/irori` | `/var/lib/irori` (Docker volume, survives restarts) |
| Resources | 4 cores, 1–8 GB | 1 CPU, 1 GB (configurable, see below) |

## Prerequisites

- Docker with **Compose** and **buildx**. Docker Desktop and OrbStack include both.
- With Homebrew and Colima:

  ```sh
  brew install colima docker docker-compose docker-buildx
  colima start
  ```

  Make sure `~/.docker/config.json` has
  `"cliPluginsExtraDirs": ["/opt/homebrew/lib/docker/cli-plugins"]`
  so `docker compose` and `docker buildx` are found.
- `gh` (GitHub CLI), only for `dev/pi review`.

Everything runs through one script from the repository root: `dev/pi`.

## Run Irori like a Pi

```sh
dev/pi up              # build the image and start irori → http://127.0.0.1:8480
dev/pi status          # container state and /api/health
dev/pi smoke           # automated smoke test against the running server
dev/pi logs            # follow logs
dev/pi shell           # shell inside the container (try: irori version)
dev/pi restart         # rebuild and restart. A lab container stays a lab container.
dev/pi down            # stop (data is kept)
dev/pi down --wipe     # stop and delete the data volume, like a fresh SD card
```

The barebones build (no extensions, no UI) must always work too:

```sh
dev/pi up --barebones
```

The first build compiles the dependencies and takes a while. Later builds reuse the cargo
cache. A code change relinks the binary without fat LTO, and a change that does not touch
the pages does not rebuild them. `--barebones` builds only `irori`, with no UI and no
extension binaries. The wasm pages in this image are dev builds. CI still ships the
release pages and the fat-LTO binaries; this container is not that artifact, and not a
benchmark (see below).

## Run the checks on Linux arm64

`dev/pi check` runs the same checks as CI's `check` job (`rustfmt`, `clippy`, tests for the
default and barebones builds, `cargo xtask check-deps`, and `cargo xtask schemas --check`),
but inside the arm64 Linux toolchain container against your working tree. This catches problems that don't show up on
macOS, such as Linux-only code paths, `cfg(unix)` differences, and file system behavior.

```sh
dev/pi check
dev/pi cargo test -p irori                 # or any other cargo command
dev/pi cargo run -- version
```

Build output goes to a Docker volume, not your `target/`, so the macOS and Linux builds
don't overwrite each other.

## Review a pull request

```sh
dev/pi review 12
```

That checks out PR #12 with `gh`, runs `dev/pi check`, starts the Pi container from the PR's
code, and runs the smoke test. If it all passes, open http://127.0.0.1:8480 and try the
change by hand. When you're done, **switch back first, then stop it**:

```sh
git switch main && dev/pi down
```

The order matters. While your checkout is on the PR, `dev/pi` *is the PR's version* of the
script, so running any `dev/pi` command there runs the PR's code on your Mac. `review` prints
the exact command to return to the branch you started from.

`review` refuses to run with uncommitted changes, so it never clobbers your work.

### How `review` keeps PR code off your Mac

A pull request can change any file, including the tools in `dev/`. So `review`:

- **Uses the `dev/` tooling from the commit you started on, not the PR's.** Before checking
  out the PR, it copies `pi`, `compose.yaml`, `Dockerfile`, and `smoke-test.sh` to a temp
  folder and runs from there. If a PR changes those files, review the diff first, then try
  them with plain `dev/pi up` / `dev/pi check`.
- **Runs the PR's code only inside containers.** That includes building it, its build scripts,
  and its tests.
- **Mounts your checkout read-only** in the container, so PR code can't plant files, such as
  git hooks, that would later run on your Mac.

It is not a full sandbox: the containers have network access, and they share the cargo
caches with your normal `dev/pi` runs. Use it for PRs you'd reasonably run, like your own,
Claude's, and collaborators'. Don't use it for code from strangers.

## Disk space

Builds fill Docker's disk over time: every PR you check adds to the cargo build cache, and a
full workspace build with its tests runs to several GB. So `up`, `restart`, `check`, and
`review` tidy up first. They drop leftover images and build cache older than a week, and they
leave the cargo cache in place. That cache is what makes the next build incremental. Wiping it
whenever free space dropped under 8 GB made every build a cold one on a 20 GB disk, which
cannot hold the cache and 8 GB free at the same time.

BuildKit still deletes cache mounts that have not been used for 48 hours, once that
part of the build cache is past its cap (512MB in the default policy). Two days
without a build can make the next one compile from scratch. That is BuildKit's own
collection, not `dev/pi clean`.

If free space is under 2 GB (`IRORI_DEV_MIN_FREE_GB`), the build stops instead of deleting the
cache. irori's data volume is never touched.

To clear the caches by hand:

```sh
dev/pi clean
```

GitHub's dev-container job keeps those same cargo directories between runs. Only a
push to main writes the cache. A pull request reads it and does not write one, so
what a pull request compiles cannot become the next build's starting point. The
run that fills the cache is still a full build. GitHub drops the cache if the
repository's caches together pass 10 GB, and the job still counts as passed when
that save does not fit.

With Colima, the default 20 GB disk and 2 CPUs are tight for this repository. A disk can grow
but not shrink:

```sh
colima stop && colima start --disk 40 --cpu 4 --memory 8
```

## Tuning the "Pi"

Set these environment variables when running `dev/pi up`:

| Variable | Default | Meaning |
|---|---|---|
| `IRORI_PORT` | `8480` | Host port (always bound to `127.0.0.1`) |
| `IRORI_PI_CPUS` | `1` | CPU limit. One Apple Silicon core is roughly a whole Pi 4's CPU throughput. |
| `IRORI_PI_MEMORY` | `1g` | Memory limit. Use `512m` to stress-test, `4g` for a bigger Pi. |

```sh
IRORI_PI_MEMORY=512m IRORI_PORT=9000 dev/pi up
```

Colima's default VM has 2 CPUs and 2–4 GB. Limits above that fail to start; raise them with
`colima start --cpu 4 --memory 8`.

## Lab devices

`dev/pi up --lab` is the same Pi, plus stand-ins for the hardware the container cannot see.
The emulator binary is built only into that image. It raises the memory limit to 2 GB
(Zigbee2MQTT needs it) unless `IRORI_PI_MEMORY` is already set.
`dev/pi restart` with no flags keeps that mode. `dev/pi up` without `--lab` leaves it.

The first time the Zigbee extension starts, it downloads Node and installs Zigbee2MQTT into
its package directory. A later start of any mode keeps `runtime/` and `z2m/` when it refreshes
the binary from the image, so that install does not run again. The network key lives in
`extension-data/` either way. Changing `zigbee2mqtt_version` still reinstalls.

| | Where it shows up |
|---|---|
| Zigbee dongle | `/dev/zigbee0`, an Ember coordinator. In the Zigbee extension's settings set the serial port to that path and `zigbee2mqtt_version` to `2.14.1`. |
| Zigbee devices | Named and placed from `dev/lab/home/devices.toml`. Permit joining announces the catalog devices; Zigbee2MQTT interviews them and they show up in the add-device flow. |
| ESPHome | Four boards announce `_esphomelib._tcp` inside the container. One extra encrypted board prints its key in `dev/pi logs`. |
| Matter | Three nodes, once their binaries are pinned in `dev/lab/matter/`. Until then the log names each one's discriminator and passcode. |

`areas.toml` and `devices.toml` from `dev/lab/home/` are each copied only when that file is missing. A file already in the volume is left alone, so lab mode does not replace device names that were saved without a rooms file. `dev/pi down --wipe` starts the house over.

The emulators live under `dev/lab/` and are not linked into `irori` or any extension. The coordinator speaks the ASH framing Zigbee2MQTT 2.14.1's ember driver uses. It answers the startup sequence that driver sends and the interview of the catalog devices. An EZSP command that is not implemented is logged as `not handled yet` and answered with not-supported, rather than reported as success.

## What this does *not* emulate

The container is a close functional stand-in, not a benchmark rig. Before trusting a
performance number (ROADMAP §4.3), measure it on real hardware:

- **Per-core speed and latency.** Apple Silicon cores are several times faster than a Pi's
  Cortex-A72/A76. The CPU limit throttles total throughput, not single-core latency.
- **Storage.** SD card write speed and wear aren't simulated; the Docker volume is fast.
- **GPIO and Bluetooth.** Nothing bridges these into the container.
- **USB, directly.** Docker Desktop has no host to pass a USB device through *from* — it's a VM,
  not this Mac. A real Zigbee dongle still reaches the container over TCP: see "Reach a Zigbee
  dongle" below. `dev/pi up --lab` instead presents `/dev/zigbee0` inside the container.
- **mDNS from this Mac.** Announcements on the desk don't cross into the container. `dev/pi up
  --lab` publishes ESPHome boards on the container's own network, which is the one the extension
  listens on. A device already known by IP is unaffected: ordinary outbound TCP to your LAN works.
- **armv7 / 32-bit Pi OS.** Only 64-bit is covered.
- **Intel Macs.** `linux/arm64` still works, but through QEMU emulation, so builds are slow.

## Reach a Zigbee dongle

The container can't see a USB device plugged into this Mac directly (see above). `dev/pi
usb-bridge` bridges it over TCP instead, using `socat` on this Mac (`brew install socat`) and
`host.docker.internal` — Docker Desktop's own DNS name for the host — to reach it from inside
the container:

```sh
ls /dev/cu.*                        # find the dongle: plug it in and out, see what appears
dev/pi usb-bridge /dev/cu.usbserial-1420   # leave this running
```

Then, in the Zigbee extension's settings (the gear icon on its Extensions card), set the serial
port to `tcp://host.docker.internal:6638` (or whatever port `usb-bridge` printed) rather than a
`/dev/...` path — Zigbee2MQTT's own adapter drivers already accept a `tcp://host:port` in place
of a device path, for exactly this: a network-attached coordinator. `dev/pi usb-bridge <device>
[port] [baud]` takes the port and baud rate as optional arguments, if the defaults (`6638`,
`115200`) aren't right for your dongle.

## Files

| File | Purpose |
|---|---|
| `pi` | The helper script described above |
| `Dockerfile` | `toolchain` (Rust on Alpine/musl), `ui` (wasm pages), `build` (static binary), `pi` (Debian slim runtime) |
| `compose.yaml` | The `pi` service and the on-demand `toolchain` service |
| `smoke-test.sh` | Smoke test shared with CI: health, WAL mode, UI present or absent per build, and the demo extension's devices when it's compiled in |
| `lab/` | Opt-in emulators for `dev/pi up --lab`: Zigbee dongle, ESPHome boards, Matter nodes, and the house seed |
