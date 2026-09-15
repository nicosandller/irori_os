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
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/target \
    cargo build --locked --release -p irori $FEATURES \
    && cp /target/release/irori /irori

FROM debian:${DEBIAN_RELEASE}-slim AS pi
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# Run unprivileged, the way the systemd unit will (M1.7).
RUN useradd --system --home-dir /var/lib/irori --create-home --shell /usr/sbin/nologin irori
COPY --from=build /irori /usr/local/bin/irori
# Fails the build if the binary isn't really static (it was built against musl, this is glibc).
RUN irori version
USER irori
ENV IRORI_DATA=/var/lib/irori \
    IRORI_BIND=0.0.0.0:8480
EXPOSE 8480
HEALTHCHECK --interval=5s --timeout=2s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8480/api/health || exit 1
ENTRYPOINT ["irori"]
CMD ["serve"]
