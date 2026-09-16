<h1 align="center">
  <img src="assets/irori-banner-a.svg" alt="IroriOS: a fast, modular, single-binary smart home core written in Rust" width="640">
</h1>

<p align="center">
  <a href="INSPIRATION.md"><b>Why and what</b></a> ·
  <a href="ROADMAP.md"><b>How and when</b></a> ·
  <a href="docs/specs/entities.md"><b>Entity model</b></a> ·
  <a href="docs/specs/extensions.md"><b>Extensions</b></a> ·
  <a href="dev/README.md"><b>Try it on a Mac</b></a>
</p>

> Status: the core's registry, live state, and extension host run, with virtual demo devices (M1.1, trimmed), and a Devices page that shows them and switches them (M0.8, M1.6 first slice). Next: ESPHome devices (ROADMAP D26). Nothing here controls a real home yet.

## Build and run

Requires stable Rust (pinned via `rust-toolchain.toml`).

```sh
cargo run -- serve                    # http://127.0.0.1:8480
cargo run -- serve --log-level debug  # also log every device and state change
cargo run -- serve --data /var/lib/irori
curl -s http://127.0.0.1:8480/api/dev/home     # temporary API: the whole home in one response
                                               # (also /api/dev/{devices,entities,states,extensions})
cargo run -- version --json
```

The **web UI** is a separate wasm crate, so `cargo build` alone doesn't need a wasm toolchain and
serves a placeholder page at `/`. Build it once to get the Devices page:

```sh
cargo install trunk --locked          # once
cargo xtask ui                        # build the UI into the folder the binary embeds
cargo run -- serve                    # http://127.0.0.1:8480 now shows your devices
```

See [crates/irori-ui/README.md](crates/irori-ui/README.md) for working on the UI itself (live
reload, no binary rebuild). CI builds it, so downloaded release binaries always have it.

Barebones build, no integrations and no UI (must always build and run):

```sh
cargo run --no-default-features -- serve
```

## Run it like a Raspberry Pi (Docker)

`dev/pi` runs Irori in a Pi-like `linux/arm64` Debian container and runs the CI checks on
Linux arm64. It's the way to try a pull request on a Mac before approving it:

```sh
dev/pi up                             # http://127.0.0.1:8480
dev/pi review <pr-number>             # checkout, check, run, smoke test
```

See [dev/README.md](dev/README.md).

## Checks (same as CI)

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo clippy --locked -p irori --no-default-features --all-targets -- -D warnings
cargo test --locked --workspace --all-features
cargo test --locked -p irori --no-default-features
cargo xtask check-deps                # crate dependency rules, ROADMAP §2.1
cargo xtask schemas --check           # schemas/ matches irori-types (run without --check to update)
```

`dev/pi check` runs exactly this list on Linux arm64 in Docker.

CI also runs `cargo xtask ui` in a job of its own and checks the UI's download size against the
budget. It needs `trunk`, which the Pi container doesn't carry, so it isn't part of the list
above; run it on the host after changing the UI.

## Static binary for a Raspberry Pi

```sh
cargo install cargo-zigbuild          # also needs zig on PATH
rustup target add aarch64-unknown-linux-musl
cargo zigbuild --release -p irori --target aarch64-unknown-linux-musl
scp target/aarch64-unknown-linux-musl/release/irori pi@raspberrypi.local:
ssh pi@raspberrypi.local ./irori serve --bind 0.0.0.0:8480 --allow-unauthenticated-lan
```

Irori has no login yet, so it only listens on `127.0.0.1` unless you pass
`--allow-unauthenticated-lan`. Anyone on your network can then open the UI and switch your
devices — the temporary API sends commands as well as reading state. The flag goes away once
authentication exists (ROADMAP D12, M1.5).

CI runs on every pull request (and on every push to `main`). It builds `x86_64` and `aarch64` musl binaries, smoke-tests the `aarch64` one under QEMU, and uploads them as workflow artifacts.

## Layout

```
assets/        brand: logo marks, banner, favicon, social card (see assets/README.md)
docs/specs/    specifications: entities.md, extensions.md, integrations.md
schemas/       JSON Schemas generated from irori-types (`cargo xtask schemas`)
fixtures/      golden examples, valid and invalid, checked by the tests
crates/        irori-types, irori-core, irori-integration, irori-rules, irori-recorder,
               irori-config, irori-api, irori-client, irori (the binary), and irori-ui
               (the Leptos web UI: wasm, built by `cargo xtask ui`, outside the workspace)
integrations/  irori-int-mqtt, irori-int-demo
extras/        irori-assist (opt-in AI, never in the default build)
xtask/         repository automation (`cargo xtask …`)
```

Most crates are empty shells for now; ROADMAP §2.1 describes what each will hold.
