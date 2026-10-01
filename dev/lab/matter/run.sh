#!/bin/sh
# Three Matter nodes, one per row in dev/lab/home/catalog.toml.
# The binaries are not compiled here. Drop a pinned linux-arm64 build of each
# sample in this directory (no BLE) and this script starts them.
set -eu
dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"

start() {
  name="$1"
  bin="$2"
  port="$3"
  discriminator="$4"
  passcode="$5"
  if [ ! -x "$dir/$bin" ]; then
    echo "irori lab: Matter node \"$name\" is not running ($bin is not pinned)"
    echo "irori lab:   discriminator $discriminator  passcode $passcode  port $port"
    return
  fi
  echo "irori lab: Matter node \"$name\" on port $port"
  "$dir/$bin" --port "$port" --discriminator "$discriminator" --passcode "$passcode" &
}

start "Outside wall switch" chip-lighting-app 5540 3840 20202021
start "Living Room Dehumidifier" chip-lighting-app 5541 3841 20202022
start "Closet motion sensor" chip-all-clusters-app 5542 3842 20202023
