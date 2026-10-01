#!/usr/bin/env bash
# Move dev/Dockerfile's cargo cache mounts to a directory on the host, or back.
#
# The mounts (the registry and /target) are what make the next compile incremental,
# and they live in the BuildKit builder. A GitHub Actions runner throws the builder
# away at the end of the job. This copies them out so actions/cache can keep them,
# and copies them back in at the start of a later job.
#
# The mount id is the target path when the Dockerfile does not set one. These paths
# have to stay the same strings dev/Dockerfile uses.
set -euo pipefail

mode=${1:?usage: dev/ci-cargo-cache.sh inject|extract <dir>}
root=${2:?usage: dev/ci-cargo-cache.sh inject|extract <dir>}

case "$mode" in
  inject | extract) ;;
  *)
    echo "usage: dev/ci-cargo-cache.sh inject|extract <dir>" >&2
    exit 1
    ;;
esac

# registry is the downloaded crates. target is the compiled artifacts, including
# the wasm pages. target-lab is the emulator, and CI does not build it.
mounts=(
  "/usr/local/cargo/registry|registry"
  "/target|target"
)

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

nonempty() {
  [[ -d "$1" ]] || return 1
  local entry
  for entry in "$1"/* "$1"/.[!.]* "$1"/..?*; do
    [[ -e "$entry" ]] && return 0
  done
  return 1
}

# A changing file in the context, so a builder that already ran this copy does
# not skip it. The stamp is not part of the cargo directory.
stamp() {
  date -u +%Y-%m-%dT%H:%M:%SZ > "$tmp/stamp"
}

inject_one() {
  local mount=$1 host=$2
  if ! nonempty "$host"; then
    echo "no cargo cache to inject at $host"
    return 0
  fi
  # The restored directory is a named build context, so the host does not
  # copy it once more before BuildKit reads it. STAMP is referenced so a
  # builder that already ran this step does not skip the copy into the mount.
  local when
  when=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  mkdir -p "$tmp/empty"
  cat > "$tmp/Dancefile" <<EOF
# syntax=docker/dockerfile:1
FROM busybox:1.36.1
ARG STAMP
RUN --mount=type=bind,from=cargocache,source=.,target=/incoming \\
    --mount=type=cache,target=${mount} \\
    echo "\$STAMP" >/dev/null \\
    && cp -a /incoming/. ${mount}/
EOF
  docker buildx build \
    --build-context "cargocache=${host}" \
    --build-arg "STAMP=${when}" \
    --output type=cacheonly \
    -f "$tmp/Dancefile" \
    "$tmp/empty"
  echo "injected $host into $mount"
}

extract_one() {
  local mount=$1 host=$2
  stamp
  cat > "$tmp/Dancefile" <<EOF
# syntax=docker/dockerfile:1
FROM busybox:1.36.1 AS src
COPY stamp /stamp
RUN --mount=type=cache,target=${mount} \\
    mkdir -p /export \\
    && cp -a ${mount}/. /export/
FROM scratch
COPY --from=src /export /
EOF
  cp "$tmp/stamp" "$tmp/stamp-in-context"
  mkdir -p "$tmp/extract-context"
  cp "$tmp/stamp" "$tmp/extract-context/stamp"
  rm -rf "$host"
  mkdir -p "$host"
  docker buildx build --output "type=local,dest=${host}" -f "$tmp/Dancefile" "$tmp/extract-context"
  echo "extracted $mount to $host"
}

for pair in "${mounts[@]}"; do
  mount=${pair%%|*}
  name=${pair#*|}
  host=$root/$name
  case "$mode" in
    inject) inject_one "$mount" "$host" ;;
    extract) extract_one "$mount" "$host" ;;
  esac
done
