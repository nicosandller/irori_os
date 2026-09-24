# syntax=docker/dockerfile:1
#
# Raspberry Pi-like environment for running and testing Irori on a dev machine.
# See dev/README.md. Build context is the repository root.
#
# Stages:
#   toolchain  Rust on Alpine (musl): runs cargo inside Linux arm64
#   build      compiles the static irori binary, the same kind of artifact a Pi runs
#   pi         Debian slim (what Raspberry Pi OS is based on) with only the binary

ARG DEBIAN_RELEASE=trixie

FROM rust:1-alpine AS toolchain
RUN apk add --no-cache musl-dev bash curl
WORKDIR /src
# Install the toolchain pinned by the repo once, in a cached layer.
COPY rust-toolchain.toml .
RUN rustup toolchain install && rustup show active-toolchain
ENV CARGO_TARGET_DIR=/target

FROM toolchain AS build
# Extra cargo flags, e.g. "--no-default-features" for the barebones build.
ARG FEATURES=""
COPY . .
# The default build's `ui` feature only embeds whatever `cargo xtask ui` has already put in
# `crates/irori/ui/` (`crates/irori/src/server.rs`'s own doc comment on this) — a plain `cargo
# build` does not build it, so without this the server always falls back to the placeholder
# page. Skipped for the barebones build, which doesn't compile the `ui` feature in at all.
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
      && cargo install wasm-bindgen-cli --version 0.2.128 --locked --profile dev \
      && cargo xtask ui ; \
    fi
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/target \
    cargo build --locked --release -p irori $FEATURES \
      -p irori-protocol-demo -p irori-protocol-esphome -p irori-protocol-mqtt \
      -p irori-protocol-zigbee \
    && cp /target/release/irori /irori \
    && mkdir -p /official/demo/bin /official/esphome/bin /official/mqtt/bin /official/zigbee/bin \
    && cp extensions/demo/irori-extension.toml extensions/demo/icon.svg /official/demo/ \
    && cp /target/release/irori-ext-demo /official/demo/bin/ \
    && cp extensions/protocols/esphome/irori-extension.toml extensions/protocols/esphome/icon.svg /official/esphome/ \
    && cp /target/release/irori-ext-esphome /official/esphome/bin/ \
    && cp extensions/protocols/mqtt/irori-extension.toml extensions/protocols/mqtt/config.schema.json /official/mqtt/ \
    && cp /target/release/irori-ext-mqtt /official/mqtt/bin/ \
    && cp extensions/protocols/zigbee/irori-extension.toml extensions/protocols/zigbee/config.schema.json /official/zigbee/ \
    && cp /target/release/irori-ext-zigbee /official/zigbee/bin/

FROM debian:${DEBIAN_RELEASE}-slim AS pi
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# Run unprivileged, the way the systemd unit will (M1.7).
RUN useradd --system --home-dir /var/lib/irori --create-home --shell /usr/sbin/nologin irori
COPY --from=build /irori /usr/local/bin/irori
COPY --from=build /official /usr/share/irori/extensions
# Fails the build if the binary isn't really static (it was built against musl, this is glibc).
RUN irori version
USER irori
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
ENTRYPOINT ["irori"]
CMD ["serve"]
