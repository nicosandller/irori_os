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
# Refresh each one from the image. Its settings, and Zigbee's network key, live
# under extension-data/ and are not touched.
#
# Node and Zigbee2MQTT live in the package directory (`runtime/` and `z2m/`).
# Deleting that directory on every start made the next start download Node and
# run pnpm again. Park those two directories beside the package — same volume,
# so the move is a rename — and put them back after every chown. A recursive
# chown of that tree is slower than the healthcheck's start window, and `mv`
# already keeps the irori uid. A settings change still reinstalls: the
# extension compares the version itself.
data="${IRORI_DATA:-/var/lib/irori}"
packages="${IRORI_OFFICIAL_PACKAGES:-/usr/share/irori/extensions}"

# Before any lab write. A non-lab image started with IRORI_LAB=1 must not seed
# rooms and device names and then exit, or the next plain start keeps them.
if [ "${IRORI_LAB:-}" = "1" ] && [ ! -x /usr/local/bin/irori-lab-esphome ]; then
  echo "irori lab: this image has no emulators. Rebuild with \`dev/pi up --lab\`." >&2
  exit 1
fi

if [ -d "$data/extensions" ]; then
  for installed in "$data"/extensions/*/; do
    [ -d "$installed" ] || continue
    id=$(basename "$installed")
    bundled="$packages/$id"
    [ -d "$bundled" ] || continue
    # A previous start can have died after the park and before the move back.
    # Keep whatever is already parked; do not delete it first.
    hold="$data/.package-refresh/$id"
    mkdir -p "$hold"
    for keep in runtime z2m; do
      if [ -d "$installed/$keep" ]; then
        rm -rf "$hold/$keep"
        mv "$installed/$keep" "$hold/$keep"
      fi
    done
    rm -rf "$installed"
    cp -r "$bundled" "$data/extensions/$id"
    # The copy is the binary and the manifest. The parked trees are not in it.
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
  # Own the volume root and what this start just created. Not the whole tree:
  # Zigbee's Node install is parked under it until the chowns are done.
  chown irori:irori /var/lib/irori
  chown -R irori:irori "$config" /var/lib/irori/lab
fi

# Put the parked trees back now, after every chown, including one that died
# last time with them still parked and the package already replaced.
if [ -d "$data/.package-refresh" ]; then
  for hold in "$data"/.package-refresh/*/; do
    [ -d "$hold" ] || continue
    id=$(basename "$hold")
    dest="$data/extensions/$id"
    if [ ! -d "$dest" ]; then
      bundled="$packages/$id"
      [ -d "$bundled" ] || continue
      cp -r "$bundled" "$dest"
      chown -R irori:irori "$dest"
    fi
    for keep in runtime z2m; do
      if [ -d "$hold/$keep" ]; then
        rm -rf "$dest/$keep"
        mv "$hold/$keep" "$dest/$keep"
      fi
    done
    rm -rf "$hold"
  done
  rmdir "$data/.package-refresh" 2>/dev/null || true
fi

if [ "${IRORI_LAB:-}" = "1" ]; then
  echo "irori lab: Zigbee dongle at /dev/zigbee0"
  echo "irori lab: in the Zigbee settings, set serial port /dev/zigbee0 and zigbee2mqtt_version 2.14.1"
  # ncp.py drops to irori once /dev/zigbee0 exists. ESPHome and Matter never need
  # root, so they drop before they start.
  IRORI_LAB_UID="$(id -u irori)" IRORI_LAB_GID="$(id -g irori)" \
    python3 /usr/share/irori/lab/zigbee/ncp.py /dev/zigbee0 /var/lib/irori/lab/zigbee.json &
  setpriv --reuid=irori --regid=irori --init-groups --inh-caps=-all \
    /usr/local/bin/irori-lab-esphome &
  setpriv --reuid=irori --regid=irori --init-groups --inh-caps=-all \
    /usr/share/irori/lab/matter/run.sh &
fi

exec setpriv --reuid=irori --regid=irori --init-groups --inh-caps=-all \
  /usr/local/bin/irori "$@"
