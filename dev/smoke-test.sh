#!/usr/bin/env bash
# Smoke-tests a running irori server. Used by CI and by `dev/pi smoke`.
#
#   dev/smoke-test.sh [BASE_URL] [TIMEOUT_SECONDS]
#
# Waits for /api/health, checks the database is in WAL mode, checks `/` serves the UI when the
# `ui` feature is compiled in (and a 404 when it isn't), and, with the demo extension compiled
# in, that it's running and its devices are listed.
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
  echo "ui: served ($index)"
else
  [[ "$index" == 404* ]] || fail "expected 404 at / in a build without the ui feature (got: $index)"
  echo "ui: not compiled in, / returns 404 as expected"
fi

if grep -q '"features":\[[^]]*"int-demo"' <<<"$health"; then
  # "Running" comes a moment before the demo has described its devices, so wait for both.
  until curl -fsS --max-time 2 "$base_url/api/dev/extensions" 2>/dev/null | grep -q '"demo":{"state":"running"' \
    && curl -fsS --max-time 2 "$base_url/api/dev/states" 2>/dev/null | grep -q '"entity_id":"light.demo_lamp"'; do
    ((SECONDS < deadline)) || fail "the demo extension isn't running with its devices listed within ${timeout}s"
    sleep 0.2
  done
  echo "extensions: demo running, its devices are listed"
fi

echo "smoke test passed"
