#!/bin/sh
# Three Matter devices, one per [[matter]] row in dev/lab/home/catalog.toml, emulated with
# matter.js (`device.mjs`). The lab image carries Node.js for them in /opt/lab-node and their
# packages beside this script (`npm ci`, from package-lock.json).
#
# Each is commissionable on the network with the pairing code it prints. Add one from the
# Matter extension's "Add with a pairing code", with "Allow test devices" turned on in its
# settings: these use development certificates.
set -eu
dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
node=/opt/lab-node/bin/node
state=/var/lib/irori/lab/matter

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
wait
