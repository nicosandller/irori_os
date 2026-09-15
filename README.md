# Irori

A fast, modular, single-binary smart home core written in Rust.

- **Why and what:** [INSPIRATION.md](INSPIRATION.md)
- **How and when:** [ROADMAP.md](ROADMAP.md)

> Status: Phase 0, milestone M0.1 (workspace and toolchain). Nothing here controls a home yet.

## Build and run

Requires stable Rust (pinned via `rust-toolchain.toml`).

```sh
cargo run -- serve                    # http://127.0.0.1:8480
cargo run -- serve --bind 0.0.0.0:8480 --data /var/lib/irori
cargo run -- version --json
```

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
```

`dev/pi check` runs exactly this list on Linux arm64 in Docker.

## Static binary for a Raspberry Pi

```sh
cargo install cargo-zigbuild          # also needs zig on PATH
rustup target add aarch64-unknown-linux-musl
cargo zigbuild --release -p irori --target aarch64-unknown-linux-musl
scp target/aarch64-unknown-linux-musl/release/irori pi@raspberrypi.local:
ssh pi@raspberrypi.local ./irori serve --bind 0.0.0.0:8480
```

CI runs on every pull request (and on every push to `main`). It builds `x86_64` and `aarch64` musl binaries, smoke-tests the `aarch64` one under QEMU, and uploads them as workflow artifacts.

## Layout

```
crates/        irori-types, irori-core, irori-integration, irori-rules, irori-recorder,
               irori-config, irori-api, irori-client, irori-ui, irori (the binary)
integrations/  irori-int-mqtt, irori-int-demo
extras/        irori-assist (opt-in AI, never in the default build)
xtask/         repository automation (`cargo xtask …`)
```

Most crates are empty shells for now; ROADMAP §2.1 describes what each will hold.
