#!/usr/bin/env bash
# Lays out a release's Linux downloads for `install/docker/Dockerfile`:
#
#   install/docker/stage.sh <dist> <out>
#
# <dist> holds what the release workflow built: `irori-linux-{x64,arm64}.tar.gz` and every
# official extension as `<bin>-<triple>.tar.gz`. <out> gets one folder per Docker platform:
#
#   <out>/amd64/irori                              the release binary
#   <out>/amd64/extensions/<id>/...                each official extension, unpacked
#   <out>/arm64/...
#   <out>/entrypoint.sh                           how the image starts Irori
#
# `<id>/` is laid out the way `packages.rs` installs from (`IRORI_OFFICIAL_PACKAGES`): manifest,
# icon, page, and the program in `bin/`. The image holds exactly the files the release ships;
# nothing is compiled here. A missing download fails the script rather than publishing an image
# without that extension.
set -euo pipefail

dist="${1:?usage: stage.sh <dist> <out>}"
out="${2:?usage: stage.sh <dist> <out>}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
catalog="$root/extensions/official.toml"

# `id` and `bin` of each `[[extension]]`, in order, as "id bin" lines.
extensions="$(awk '
    /^\[\[extension\]\]/ { if (id != "" && bin != "") print id, bin; id = ""; bin = "" }
    /^id *= */  { gsub(/^id *= *"|"$/, ""); id = $0 }
    /^bin *= */ { gsub(/^bin *= *"|"$/, ""); bin = $0 }
    END { if (id != "" && bin != "") print id, bin }
' "$catalog")"
[[ -n "$extensions" ]] || { echo "no extensions found in $catalog" >&2; exit 1; }

# Docker platform, the release's asset arch, and the Rust target the extensions were built for.
for row in "amd64 x64 x86_64-unknown-linux-musl" "arm64 arm64 aarch64-unknown-linux-musl"; do
    read -r platform arch triple <<<"$row"
    dest="$out/$platform"
    rm -rf "$dest"
    mkdir -p "$dest/extensions"
    tar -xzf "$dist/irori-linux-$arch.tar.gz" -C "$dest" irori
    while read -r id bin; do
        mkdir -p "$dest/extensions/$id"
        tar -xzf "$dist/$bin-$triple.tar.gz" -C "$dest/extensions/$id"
        [[ -x "$dest/extensions/$id/bin/$bin" ]] || {
            echo "$bin-$triple.tar.gz has no bin/$bin" >&2
            exit 1
        }
    done <<<"$extensions"
    echo "staged $platform: irori and $(wc -l <<<"$extensions" | tr -d ' ') extensions"
done
install -m755 "$root/install/docker/entrypoint.sh" "$out/entrypoint.sh"
