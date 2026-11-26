#!/bin/sh
# Starts Irori as the unprivileged `irori` user, whoever started the container.
#
# Home Assistant starts an app as root, with its data folder (`/data`) owned by root; compose's
# named volume starts out owned by `irori`. Run as root, this makes the data and config folders
# Irori's and drops to `irori` before Irori runs. Run as anyone else (`--user`), it changes
# nothing and starts Irori as that user.
set -eu

if [ "$(id -u)" = "0" ]; then
    for dir in "${IRORI_DATA:-}" "${IRORI_CONFIG:-}"; do
        [ -n "$dir" ] || continue
        mkdir -p "$dir"
        # Only when it isn't Irori's yet: a large database isn't walked on every start.
        if [ "$(stat -c %U "$dir")" != "irori" ]; then
            chown -R irori:irori "$dir"
        fi
    done
    exec setpriv --reuid=irori --regid=irori --init-groups /usr/local/bin/irori "$@"
fi
exec /usr/local/bin/irori "$@"
