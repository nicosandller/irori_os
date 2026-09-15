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

## Checks (same as CI)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo xtask check-deps                # crate dependency rules, ROADMAP §2.1
```

## Static binary for a Raspberry Pi

```sh
cargo install cargo-zigbuild          # also needs zig on PATH
rustup target add aarch64-unknown-linux-musl
cargo zigbuild --release -p irori --target aarch64-unknown-linux-musl
scp target/aarch64-unknown-linux-musl/release/irori pi@raspberrypi.local:
ssh pi@raspberrypi.local ./irori serve --bind 0.0.0.0:8480
```

CI builds `x86_64` and `aarch64` musl binaries on every push, smoke-tests the `aarch64` one under QEMU, and uploads them as workflow artifacts.

## Layout

```
crates/        irori-types, irori-core, irori-integration, irori-rules, irori-recorder,
               irori-config, irori-api, irori-client, irori-ui, irori (the binary)
integrations/  irori-int-mqtt, irori-int-demo
extras/        irori-assist (opt-in AI, never in the default build)
xtask/         repository automation (`cargo xtask …`)
```

Most crates are empty shells for now; ROADMAP §2.1 describes what each will hold.
