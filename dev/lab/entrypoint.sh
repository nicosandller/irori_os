#!/bin/sh
# Dev-image entrypoint. With IRORI_LAB=1, start the emulators, then run irori as
# the unprivileged user either way. Production installs do not use this script.
set -eu

if [ "${IRORI_LAB:-}" = "1" ]; then
  config="${IRORI_CONFIG:-/var/lib/irori/config}"
  mkdir -p "$config" /var/lib/irori/lab
  if [ ! -f "$config/areas.toml" ]; then
    cp /usr/share/irori/lab/home/areas.toml "$config/areas.toml"
    cp /usr/share/irori/lab/home/devices.toml "$config/devices.toml"
    echo "irori lab: seeded rooms and device names into $config"
  fi
  chown -R irori:irori /var/lib/irori
  echo "irori lab: Zigbee dongle at /dev/zigbee0"
  echo "irori lab: in the Zigbee settings, set serial port /dev/zigbee0 and zigbee2mqtt_version 2.14.1"
  IRORI_LAB_UID="$(id -u irori)" IRORI_LAB_GID="$(id -g irori)" \
    python3 /usr/share/irori/lab/zigbee/ncp.py /dev/zigbee0 /var/lib/irori/lab/zigbee.json &
  /usr/local/bin/irori-lab-esphome &
  /usr/share/irori/lab/matter/run.sh &
fi

# Docker starts this script as root, so HOME is /root. setpriv changes the uid and
# leaves the environment. Zigbee's installer then runs Corepack, which writes its
# cache under $HOME/.cache — /root/.cache, which the irori user cannot open.
export HOME=/var/lib/irori
export USER=irori
export LOGNAME=irori
exec setpriv --reuid=irori --regid=irori --init-groups --inh-caps=-all \
  /usr/local/bin/irori "$@"
