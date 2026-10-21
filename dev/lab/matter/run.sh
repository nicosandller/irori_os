#!/bin/sh
# Eight Matter devices, one per [[matter]] row in dev/lab/home/catalog.toml, emulated with
# matter.js (`device.mjs`). The lab image carries Node.js for them in /opt/lab-node and their
# packages beside this script (`npm ci`, from package-lock.json).
#
# Each is commissionable on the network with the pairing code it prints. Add one from the
# Matter extension's "Add with a pairing code", with "Allow test devices" turned on in its
# settings: these use development certificates.
#
# Outside the lab image, say which Node.js (22.13 or later) and where the devices keep their
# state: IRORI_LAB_NODE=$(command -v node) IRORI_LAB_STATE=/tmp/lab-matter dev/lab/matter/run.sh
# (after `npm ci` here).
set -eu
dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
node="${IRORI_LAB_NODE:-/opt/lab-node/bin/node}"
state="${IRORI_LAB_STATE:-/var/lib/irori/lab/matter}"

if [ ! -x "$node" ] || [ ! -d "$dir/node_modules/@matter/main" ]; then
  echo "irori lab: no Matter devices: this image has no Node.js for them"
  exit 0
fi
mkdir -p "$state"

start() {
  name="$1"
  kind="$2"
  vendor="$3"
  model="$4"
  port="$5"
  discriminator="$6"
  passcode="$7"
  # matter.js logs every packet at its default level. Warnings are enough here.
  MATTER_LOG_LEVEL=warn MATTER_LOG_FORMAT=plain \
    "$node" "$dir/device.mjs" --kind "$kind" --name "$name" --vendor "$vendor" --model "$model" \
    --port "$port" --discriminator "$discriminator" --passcode "$passcode" \
    --serial "lab-$discriminator" --storage "$state/$discriminator" &
}

start "Outside wall switch" switch "SONOFF" "WiFi Smart Switch" 5540 3840 20202021
start "Living Room Dehumidifier" plug "Tapo" "Smart Wi-Fi Plug" 5541 3841 20202022
start "Closet motion sensor" occupancy "IKEA of Sweden" "MYGGSPRAY wrlss mtn sensor" 5542 3842 20202023
# A manual pairing code carries only a discriminator's top four bits, so the first three, which
# share theirs, are each found by trying all three. The rest have their own: they add in seconds.
start "Reading lamp" light "Signify Netherlands B.V." "Hue color lamp" 5543 2560 20202024
start "Front door lock" lock "Nuki" "Smart Lock Pro" 5544 2816 20202025
start "Bedroom blind" blind "IKEA of Sweden" "KADRILJ roller blind" 5545 3072 20202026
start "Hallway thermostat" thermostat "Eve Systems" "Eve Thermo" 5546 3328 20202027
start "Office fan" fan "Dreo" "Tower Fan" 5547 3584 20202028
wait
