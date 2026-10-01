#!/bin/sh
# Dev-image entrypoint. With IRORI_LAB=1, start the emulators, then run irori as
# the unprivileged user either way. Production installs do not use this script.
set -eu

# Docker starts this script as root, so HOME is /root. setpriv changes the uid and
# leaves the environment. Zigbee's installer then runs Corepack, which writes its
# cache under $HOME/.cache — /root/.cache, which the irori user cannot open.
export HOME=/var/lib/irori
export USER=irori
export LOGNAME=irori

# Installing copies a package into the data volume, which outlives the image, so an
# official extension installed before a rebuild would keep running its old files.
# Refresh each one from the image. Its data and its settings live elsewhere.
data="${IRORI_DATA:-/var/lib/irori}"
packages="${IRORI_OFFICIAL_PACKAGES:-/usr/share/irori/extensions}"
if [ -d "$data/extensions" ]; then
  for installed in "$data"/extensions/*/; do
    [ -d "$installed" ] || continue
    id=$(basename "$installed")
    bundled="$packages/$id"
    [ -d "$bundled" ] || continue
    rm -rf "$installed"
    cp -r "$bundled" "$data/extensions/$id"
    chown -R irori:irori "$data/extensions/$id"
  done
fi

if [ "${IRORI_LAB:-}" = "1" ]; then
  config="${IRORI_CONFIG:-/var/lib/irori/config}"
  mkdir -p "$config" /var/lib/irori/lab
  # Seed each file on its own. A volume can already have device names and no rooms
  # file; copying both because areas.toml is missing would replace those names.
  if [ ! -f "$config/areas.toml" ]; then
    cp /usr/share/irori/lab/home/areas.toml "$config/areas.toml"
    echo "irori lab: seeded rooms into $config"
  fi
  if [ ! -f "$config/devices.toml" ]; then
    cp /usr/share/irori/lab/home/devices.toml "$config/devices.toml"
    echo "irori lab: seeded device names into $config"
  fi
  chown -R irori:irori /var/lib/irori
  echo "irori lab: Zigbee dongle at /dev/zigbee0"
  echo "irori lab: in the Zigbee settings, set serial port /dev/zigbee0 and zigbee2mqtt_version 2.14.1"
  # The coordinator stays root only long enough to create /dev/zigbee0. The two
  # emulators that accept network connections do not need that, so they drop first.
  IRORI_LAB_UID="$(id -u irori)" IRORI_LAB_GID="$(id -g irori)" \
    python3 /usr/share/irori/lab/zigbee/ncp.py /dev/zigbee0 /var/lib/irori/lab/zigbee.json &
  setpriv --reuid=irori --regid=irori --init-groups --inh-caps=-all \
    /usr/local/bin/irori-lab-esphome &
  setpriv --reuid=irori --regid=irori --init-groups --inh-caps=-all \
    /usr/share/irori/lab/matter/run.sh &
fi

exec setpriv --reuid=irori --regid=irori --init-groups --inh-caps=-all \
  /usr/local/bin/irori "$@"
