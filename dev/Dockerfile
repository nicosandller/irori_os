# syntax=docker/dockerfile:1
#
# Raspberry Pi-like environment for running and testing Irori on a dev machine.
# See dev/README.md. Build context is the repository root.
#
# Stages:
#   toolchain  Rust on Alpine (musl): runs cargo inside Linux arm64
#   wasmtools  trunk and wasm-bindgen, cached independently of the source tree
#   ui         the wasm pages, from only the inputs those pages compile
#   build      the static irori binary, the same kind of artifact a Pi runs
#   pi         Debian slim (what Raspberry Pi OS is based on) with only the binary
#
# This image is not the release artifact. CI keeps fat LTO. Here the release
# profile is overridden so a one-line change does not pay a full-program link,
# and the wasm pages are dev builds. `dev/pi check` uses the toolchain stage,
# which does not carry those overrides.

ARG DEBIAN_RELEASE=trixie

FROM rust:1-alpine AS toolchain
RUN apk add --no-cache musl-dev bash curl
WORKDIR /src
# Install the toolchain pinned by the repo once, in a cached layer.
COPY rust-toolchain.toml .
RUN rustup toolchain install && rustup show active-toolchain
ENV CARGO_TARGET_DIR=/target

FROM toolchain AS wasmtools
# Extra cargo flags, e.g. "--no-default-features" for the barebones build.
ARG FEATURES=""
# The wasm build tools, installed *before* any source copy on purpose: nothing here reads the
# source tree, so keeping them above the copy makes this ~570 MB layer a function of the pinned
# toolchain alone. Run after the copy, every source change invalidated it and left a fresh copy
# of the layer behind in the build cache — a new PR under `dev/pi review` is a source change, and
# six of those came to fill a 20 GB Colima disk.
#
# `trunk` is downloaded prebuilt rather than `cargo install`ed: its own release build (LTO,
# one codegen unit) needs more memory than this environment reliably has to spare, and gets
# OOM-killed partway through the link step — a prebuilt binary needs no compiling at all.
#
# `wasm-bindgen-cli`, by contrast, has no prebuilt musl binary to download: trunk's own
# auto-fetch grabs a glibc one, which this Alpine (musl) toolchain can't run at all ("No such
# file or directory" is musl's exec failing to find glibc's dynamic linker, not a missing file).
# Installing the exact version `crates/irori-ui/Cargo.lock` pins builds it natively for musl
# instead, and trunk uses that already-on-PATH copy rather than fetching its own. `--profile
# dev` for the same memory reason `trunk` itself is never compiled here.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/target \
    if [ "$FEATURES" != "--no-default-features" ]; then \
      rustup target add wasm32-unknown-unknown \
      && curl --fail --location --silent --show-error \
         "https://github.com/trunk-rs/trunk/releases/latest/download/trunk-$(uname -m)-unknown-linux-musl.tar.gz" \
      | tar xz -C /usr/local/bin trunk \
      && cargo install wasm-bindgen-cli --version 0.2.128 --locked --profile dev ; \
    fi

# Pages only. A change under crates/irori/src does not rebuild this stage.
# `cargo xtask` still has to load the whole workspace, so every member's manifest
# is here. Only the crates xtask and the two wasm pages actually compile have sources.
FROM wasmtools AS ui
ARG FEATURES=""
ENV IRORI_TRUNK_RELEASE=0 \
    SOURCE_DATE_EPOCH=0
COPY Cargo.toml Cargo.lock ./
# `cargo xtask` is an alias. The full source copy used to bring this in; this
# stage has to name it or cargo reports there is no such command.
COPY .cargo/config.toml .cargo/config.toml
COPY xtask xtask
COPY crates/irori-types crates/irori-types
COPY crates/irori-rules crates/irori-rules
COPY crates/irori-flow-types crates/irori-flow-types
COPY crates/irori-protocol crates/irori-protocol
COPY crates/irori-ha-discovery crates/irori-ha-discovery
COPY extensions/protocols/mqtt extensions/protocols/mqtt
COPY extensions/protocols/zigbee extensions/protocols/zigbee
COPY crates/irori-ui crates/irori-ui
COPY crates/irori-ui-kit crates/irori-ui-kit
COPY extensions/automations/app extensions/automations/app
COPY extensions/official.toml extensions/official.toml
COPY extensions/automations/irori-extension.toml extensions/automations/irori-extension.toml
COPY extensions/demo/irori-extension.toml extensions/demo/irori-extension.toml
COPY extensions/protocols/esphome/irori-extension.toml extensions/protocols/esphome/irori-extension.toml
# Manifests for members this stage does not compile. Without them cargo refuses to load the workspace.
COPY crates/irori/Cargo.toml crates/irori/Cargo.toml
COPY crates/irori-core/Cargo.toml crates/irori-core/Cargo.toml
COPY crates/irori-flows/Cargo.toml crates/irori-flows/Cargo.toml
COPY crates/irori-recorder/Cargo.toml crates/irori-recorder/Cargo.toml
COPY crates/irori-config/Cargo.toml crates/irori-config/Cargo.toml
COPY crates/irori-api/Cargo.toml crates/irori-api/Cargo.toml
COPY crates/irori-client/Cargo.toml crates/irori-client/Cargo.toml
COPY crates/irori-helpers/Cargo.toml crates/irori-helpers/Cargo.toml
COPY extensions/demo/Cargo.toml extensions/demo/Cargo.toml
COPY extensions/automations/Cargo.toml extensions/automations/Cargo.toml
COPY extensions/protocols/esphome/Cargo.toml extensions/protocols/esphome/Cargo.toml
COPY extras/irori-assist/Cargo.toml extras/irori-assist/Cargo.toml
# Cargo will not load a workspace member that has no target. These crates are not
# compiled here; the stub only lets `cargo xtask` parse the workspace.
RUN for dir in \
      crates/irori crates/irori-core crates/irori-flows crates/irori-recorder \
      crates/irori-config crates/irori-api crates/irori-client crates/irori-helpers \
      extensions/demo extensions/automations extensions/protocols/esphome \
      extras/irori-assist ; do \
      mkdir -p "$dir/src" ; \
      if [ ! -f "$dir/src/lib.rs" ] && [ ! -f "$dir/src/main.rs" ]; then \
        echo '// Not compiled in this stage.' > "$dir/src/lib.rs" ; \
      fi ; \
      if grep -q 'path = "src/main.rs"' "$dir/Cargo.toml" && [ ! -f "$dir/src/main.rs" ]; then \
        echo 'fn main() {}' > "$dir/src/main.rs" ; \
      fi ; \
    done
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/target \
    mkdir -p /ui-out/embedded /ui-out/automations \
    && if [ "$FEATURES" != "--no-default-features" ]; then \
         cargo xtask ui \
         && cp -a crates/irori/ui/. /ui-out/embedded/ \
         && cp -a extensions/automations/app/dist/. /ui-out/automations/ ; \
       else \
         touch /ui-out/embedded/.gitkeep /ui-out/automations/.gitkeep ; \
       fi

FROM wasmtools AS build
ARG FEATURES=""
ARG LAB=0
# Fat LTO and one codegen unit are for the binaries CI ships. This image is a
# dev stand-in: opt-level and strip stay, the link does not. The split that
# linked `irori` alone existed because six fat-LTO links OOM a 4 GB VM.
ENV CARGO_PROFILE_RELEASE_LTO=off \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
    SOURCE_DATE_EPOCH=0
COPY . .
# The host's crates/irori/ui is dockerignored. These are the pages from the ui stage.
COPY --from=ui /ui-out/embedded/ crates/irori/ui/
COPY --from=ui /ui-out/automations/ extensions/automations/app/dist/
# Barebones is irori without the ui feature and without extension binaries. The
# catalog the smoke test reads is compiled into the binary from official.toml.
# The lab emulator is its own workspace and is not part of a plain or barebones image.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/target \
    --mount=type=cache,target=/target-lab \
    mkdir -p /official /lab-out \
    && if [ "$FEATURES" = "--no-default-features" ]; then \
         cargo build --locked --release -p irori --no-default-features \
         && cp /target/release/irori /irori \
         && touch /official/.keep /lab-out/.keep ; \
       else \
         cargo build --locked --release \
           -p irori \
           -p irori-protocol-demo -p irori-protocol-esphome -p irori-protocol-mqtt \
           -p irori-protocol-zigbee -p irori-engine-automations \
         && cp /target/release/irori /irori \
         && mkdir -p /official/demo/bin /official/esphome/bin /official/mqtt/bin /official/zigbee/bin \
           /official/automations/bin \
         && cp extensions/demo/irori-extension.toml extensions/demo/icon.svg /official/demo/ \
         && cp /target/release/irori-ext-demo /official/demo/bin/ \
         && cp extensions/protocols/esphome/irori-extension.toml extensions/protocols/esphome/icon.svg /official/esphome/ \
         && cp /target/release/irori-ext-esphome /official/esphome/bin/ \
         && cp extensions/protocols/mqtt/irori-extension.toml extensions/protocols/mqtt/config.schema.json /official/mqtt/ \
         && cp /target/release/irori-ext-mqtt /official/mqtt/bin/ \
         && cp extensions/protocols/zigbee/irori-extension.toml extensions/protocols/zigbee/config.schema.json /official/zigbee/ \
         && cp /target/release/irori-ext-zigbee /official/zigbee/bin/ \
         && cp extensions/automations/irori-extension.toml extensions/automations/icon.svg /official/automations/ \
         && cp /target/release/irori-ext-automations /official/automations/bin/ \
         && if [ -f extensions/automations/app/dist/index.html ]; then \
              cp -r extensions/automations/app/dist /official/automations/app ; \
            fi \
         && if [ "$LAB" = "1" ]; then \
              CARGO_TARGET_DIR=/target-lab \
                cargo build --locked --manifest-path dev/lab/esphome/Cargo.toml \
              && cp /target-lab/debug/irori-lab-esphome /lab-out/irori-lab-esphome ; \
            else \
              touch /lab-out/.keep ; \
            fi ; \
       fi

FROM debian:${DEBIAN_RELEASE}-slim AS pi
ARG LAB=0
# python3 is only there for the lab coordinator. setpriv (util-linux) drops to the
# irori user in every mode.
RUN apt-get update \
    && packages="curl ca-certificates util-linux" \
    && if [ "$LAB" = "1" ]; then packages="$packages python3"; fi \
    && apt-get install -y --no-install-recommends $packages \
    && rm -rf /var/lib/apt/lists/*
# Run unprivileged, the way the systemd unit will (M1.7).
RUN useradd --system --home-dir /var/lib/irori --create-home --shell /usr/sbin/nologin irori
COPY --from=build /irori /usr/local/bin/irori
COPY --from=build /lab-out/ /tmp/lab-out/
RUN if [ -f /tmp/lab-out/irori-lab-esphome ]; then \
      mv /tmp/lab-out/irori-lab-esphome /usr/local/bin/irori-lab-esphome \
      && chmod 755 /usr/local/bin/irori-lab-esphome ; \
    fi \
    && rm -rf /tmp/lab-out
COPY --from=build /official /usr/share/irori/extensions
COPY dev/lab /usr/share/irori/lab
COPY dev/lab/entrypoint.sh /usr/local/bin/irori-lab-entrypoint
RUN chmod 755 /usr/local/bin/irori-lab-entrypoint /usr/share/irori/lab/matter/run.sh
# Fails the build if the binary isn't really static (it was built against musl, this is glibc).
RUN irori version
# Root only so the lab entrypoint can create /dev/zigbee0 and refresh installed
# official packages, then it drops to irori. With IRORI_LAB unset that drop and
# the refresh are all it does.
# Listening on all interfaces is needed inside a container. It stays private: compose publishes
# the port on the host's 127.0.0.1 only. The flag goes away when auth lands (ROADMAP D12, M1.5).
# Both absolute: the container has no WORKDIR, so the relative defaults would land in `/`, which
# the unprivileged user can't write. The config dir sits inside the data dir because that's the
# one volume compose persists — different paths, one thing to mount.
ENV IRORI_DATA=/var/lib/irori \
    IRORI_CONFIG=/var/lib/irori/config \
    IRORI_BIND=0.0.0.0:8480 \
    # The published port and health-check are pinned to 8480, so this must never auto-step to
    # a neighbour that compose isn't publishing. A fallback equal to the bind says "fail loudly".
    IRORI_BIND_FALLBACK=0.0.0.0:8480 \
    IRORI_ALLOW_UNAUTHENTICATED_LAN=true \
    IRORI_OFFICIAL_PACKAGES=/usr/share/irori/extensions
EXPOSE 8480
HEALTHCHECK --interval=5s --timeout=2s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8480/api/health || exit 1
ENTRYPOINT ["/usr/local/bin/irori-lab-entrypoint"]
CMD ["serve"]
