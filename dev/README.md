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
dev/pi restart         # rebuild from your current checkout and restart
dev/pi down            # stop (data is kept)
dev/pi down --wipe     # stop and delete the data volume, like a fresh SD card
```

The barebones build (no integrations, no UI) must always work too:

```sh
dev/pi up --barebones
```

The first build takes a couple of minutes. After that, dependencies and build artifacts
are cached, and a rebuild after a code change takes about half a minute (most of that is
link-time optimization, which the release build uses on purpose).

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

## What this does *not* emulate

The container is a close functional stand-in, not a benchmark rig. Before trusting a
performance number (ROADMAP §4.3), measure it on real hardware:

- **Per-core speed and latency.** Apple Silicon cores are several times faster than a Pi's
  Cortex-A72/A76. The CPU limit throttles total throughput, not single-core latency.
- **Storage.** SD card write speed and wear aren't simulated; the Docker volume is fast.
- **Hardware.** No GPIO, serial ports, USB Zigbee sticks, or Bluetooth.
- **armv7 / 32-bit Pi OS.** Only 64-bit is covered.
- **Intel Macs.** `linux/arm64` still works, but through QEMU emulation, so builds are slow.

## Files

| File | Purpose |
|---|---|
| `pi` | The helper script described above |
| `Dockerfile` | `toolchain` (Rust on Alpine/musl), `build` (static binary), `pi` (Debian slim runtime) |
| `compose.yaml` | The `pi` service and the on-demand `toolchain` service |
| `smoke-test.sh` | Smoke test shared with CI: health, WAL mode, and UI present or absent per build |
