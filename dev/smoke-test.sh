#!/usr/bin/env bash
# Smoke-tests a running irori server. Used by CI and by `dev/pi smoke`.
#
#   dev/smoke-test.sh [BASE_URL] [TIMEOUT_SECONDS]
#
# Waits for /api/health, checks the database is in WAL mode, checks `/` serves the UI when the
# `ui` feature is compiled in (and a 404 when it isn't), and, with the demo extension compiled
# in, that it's running and its devices are listed.
#
# It also makes a room called "Smoke test room" and removes it again, which is the only way to
# prove the server can actually write its config directory (a permissions problem shows up
# nowhere else). Against your own instance that means one room appears and disappears.
set -euo pipefail

base_url="${1:-http://127.0.0.1:8480}"
timeout="${2:-30}"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

health=""
deadline=$((SECONDS + timeout))
until health="$(curl -fsS --max-time 2 "$base_url/api/health" 2>/dev/null)"; do
  ((SECONDS < deadline)) || fail "no healthy response from $base_url/api/health within ${timeout}s"
  sleep 0.2
done
echo "health: $health"

grep -q '"status":"ok"' <<<"$health" || fail "status is not ok"
grep -q '"journal_mode":"wal"' <<<"$health" || fail "database is not in WAL mode"

index="$(curl -sS --max-time 5 -o /dev/null -w '%{http_code} %{content_type}' "$base_url/")"
if grep -q '"features":\[[^]]*"ui"' <<<"$health"; then
  [[ "$index" == "200 text/html"* ]] || fail "expected the UI at / (got: $index)"
  # Which page: the Leptos app when the binary was built after `cargo xtask ui`, the placeholder
  # in crates/irori/assets/ otherwise. Both are a served UI, so neither is a failure here.
  page="$(curl -sS --max-time 5 "$base_url/")"
  if grep -q 'irori-ui' <<<"$page"; then
    echo "ui: the Leptos app ($index)"
  else
    echo "ui: the placeholder page, built without \`cargo xtask ui\` ($index)"
  fi
else
  [[ "$index" == 404* ]] || fail "expected 404 at / in a build without the ui feature (got: $index)"
  echo "ui: not compiled in, / returns 404 as expected"
fi

if grep -q '"features":\[[^]]*"int-demo"' <<<"$health"; then
  # "Running" comes a moment before the demo has described its devices, so wait for both.
  # Each response is captured first: piping into `grep -q` can kill curl with SIGPIPE once it
  # matches, which `set -o pipefail` would report as a failure.
  demo_ready() {
    local extensions states
    extensions="$(curl -fsS --max-time 2 "$base_url/api/dev/extensions" 2>/dev/null)" || return 1
    states="$(curl -fsS --max-time 2 "$base_url/api/dev/states" 2>/dev/null)" || return 1
    # Matched inside the extension's own object, so the check doesn't depend on which order
    # the fields happen to be serialized in.
    grep -qE '"demo":\{[^{}]*"state":"running"' <<<"$extensions" &&
      grep -q '"entity_id":"light.demo_lamp"' <<<"$states"
  }
  until demo_ready; do
    ((SECONDS < deadline)) || fail "the demo extension isn't running with its devices listed within ${timeout}s"
    sleep 0.2
  done
  echo "extensions: demo running, its devices are listed"
fi

if grep -q '"features":\[[^]]*"int-esphome"' <<<"$health"; then
  # It should be running whether or not there's an ESPHome device on this network: with none,
  # it sits listening. Devices are a property of the network, so they aren't asserted here.
  extensions="$(curl -fsS --max-time 2 "$base_url/api/dev/extensions" 2>/dev/null)" || \
    fail "can't read the extensions view"
  grep -qE '"esphome":\{[^{}]*"state":"running"' <<<"$extensions" ||
    fail "the esphome extension isn't running: $extensions"
  echo "extensions: esphome running"
fi

# The config directory: a room can be made, is listed, and can be removed again. Writing is the
# part worth testing — a read-only or missing directory fails here and nowhere else.
room="$(curl -fsS --max-time 5 -X POST "$base_url/api/dev/areas" \
  -H 'content-type: application/json' -d '{"name":"Smoke test room"}')" ||
  fail "couldn't make a room: is the config directory writable?"
# Its id is whatever the server chose: against an instance that already has one (a persistent
# home, or an earlier run of this script that didn't finish) it will be smoke_test_room_2. Take
# it from the answer rather than assuming, or the room is made and then never cleaned up.
room_id="$(sed -n 's/.*"id":"\([^"]*\)".*/\1/p' <<<"$room")"
[[ -n "$room_id" ]] || fail "unexpected answer making a room: $room"
# From here on the room exists, so every exit has to remove it.
trap 'curl -fsS --max-time 5 -o /dev/null -X DELETE "$base_url/api/dev/areas/$room_id" || true' EXIT

areas="$(curl -fsS --max-time 5 "$base_url/api/dev/areas")" || fail "can't list rooms"
grep -q "\"id\":\"$room_id\"" <<<"$areas" || fail "the room wasn't listed: $areas"

curl -fsS --max-time 5 -o /dev/null -X DELETE "$base_url/api/dev/areas/$room_id" ||
  fail "couldn't remove the room again"
trap - EXIT
areas="$(curl -fsS --max-time 5 "$base_url/api/dev/areas")" || fail "can't list rooms"
grep -q "\"id\":\"$room_id\"" <<<"$areas" && fail "the room is still there: $areas"
echo "config: a room was made, listed, and removed ($room_id)"

echo "smoke test passed"
