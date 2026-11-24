#!/bin/sh
set -eu

data="${GAMESYNC_DATA:-/var/lib/gamesync}"

# Docker creates an empty bind mount as root. Initialize only an empty
# directory; never recursively change ownership of an existing data set.
if ! gosu 10001:10001 test -w "$data" || ! gosu 10001:10001 test -x "$data"; then
    if [ ! -d "$data" ] || ! entries=$(ls -A "$data") || [ -n "$entries" ]; then
        echo "GameSync cannot write to $data. Use a dedicated empty directory or grant UID 10001 access to the existing data." >&2
        exit 1
    fi
    chown 10001:10001 "$data"
    chmod 0700 "$data"
fi

exec gosu 10001:10001 /usr/local/bin/gamesync-server "$@"
