# syntax=docker/dockerfile:1
#
# Irori's published image: `ghcr.io/nicosandller/irori:<version>`, for Linux on Intel/AMD
# (amd64) and on ARM (arm64, a Raspberry Pi 4 or 5, Home Assistant Green or Yellow). The
# Home Assistant app (`install/home-assistant/irori`) runs this image, and so does
# `install/docker/compose.yaml`.
#
# Nothing is compiled here. The release workflow builds the binary and the extensions once, for
# every target, and `install/docker/stage.sh` lays its Linux downloads (and `entrypoint.sh`) out
# as the build context:
#
#   install/docker/stage.sh dist stage
#   docker buildx build --platform linux/amd64,linux/arm64 -f install/docker/Dockerfile stage
#
# So the image holds exactly the binary a native install downloads. The defaults are a native
# install's too (`install/irori.service`): listening on this machine only, 127.0.0.1:8480, with
# the home in /var/lib/irori. Whoever runs the image opens it wider, as the Home Assistant app does.
#
# This is not `dev/Dockerfile`, which builds from source for the dev/pi environment.

FROM debian:trixie-slim

ARG TARGETARCH

# ca-certificates: Irori and its extensions reach GitHub and cloud services over https. tar:
# installing an extension unpacks it with GNU tar (`packages.rs` reads its verbose listing). curl:
# the health check. setpriv (util-linux, already in the base) drops to the `irori` user.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl tar \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home-dir /var/lib/irori --create-home --shell /usr/sbin/nologin irori

COPY ${TARGETARCH}/irori /usr/local/bin/irori
# The official extensions, where Install finds them first: Install copies one from here into the
# home, with no download. Updating the image brings newer packages, and the Extensions page then
# offers each installed one its Update.
COPY ${TARGETARCH}/extensions /usr/share/irori/extensions
COPY entrypoint.sh /usr/local/bin/irori-entrypoint
RUN chmod 755 /usr/local/bin/irori /usr/local/bin/irori-entrypoint \
    && irori version

# The same settings as the systemd unit. The binary sits where its user can't write, so Irori
# says when a newer release exists and leaves installing it to whoever runs the image.
ENV IRORI_DATA=/var/lib/irori \
    IRORI_CONFIG=/var/lib/irori/config \
    IRORI_BIND=127.0.0.1:8480 \
    IRORI_OFFICIAL_PACKAGES=/usr/share/irori/extensions

EXPOSE 8480
# Asks Irori on the port it listens on, whichever address that is.
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${IRORI_BIND##*:}/api/health" || exit 1
ENTRYPOINT ["/usr/local/bin/irori-entrypoint"]
CMD ["serve"]
