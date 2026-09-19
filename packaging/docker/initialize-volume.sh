#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

if [ "$#" -eq 0 ]; then
    volume_root=/var/lib/teslatlas-hub
    volume_uid=10001
    volume_gid=10001
    install_command=install
    link_command=ln
    stat_command=stat
    guard_uid=0
    guard_gid=0
    test_mode=0
elif [ "$#" -eq 4 ] && [ "$1" = "--test-only" ] && [ "${TESLATLAS_HUB_VOLUME_INIT_TEST_ONLY:-}" = 1 ]; then
    volume_root=$2
    volume_uid=$3
    volume_gid=$4
    install_command=$(command -v ginstall || command -v install)
    link_command=$(command -v gln || command -v ln)
    stat_command=$(command -v gstat || command -v stat)
    guard_uid=$volume_uid
    guard_gid=$volume_gid
    test_mode=1
else
    echo "usage: initialize-volume.sh" >&2
    exit 2
fi

case "$volume_root" in
    /*) ;;
    *) echo "volume root must be absolute" >&2; exit 2 ;;
esac
case "$volume_uid:$volume_gid" in
    *[!0-9:]* | :* | *:) echo "volume uid/gid must be numeric" >&2; exit 2 ;;
esac

sentinel="$volume_root/.teslatlas-volume-initialized"
temporary=
cleanup() {
    if [ -n "$temporary" ]; then
        rm -f -- "$temporary"
    fi
}
trap cleanup EXIT HUP INT TERM

# Production temporarily makes the mounted directory root-only. That prevents
# UID 10001 from replacing the private temporary inode while root prepares it.
# The mountpoint itself cannot be renamed through the read-only image parent.
"$install_command" -d -o "$guard_uid" -g "$guard_gid" -m 0700 "$volume_root"

if [ -e "$sentinel" ] || [ -L "$sentinel" ]; then
    # Idempotency is allowed only for the exact regular sentinel we created.
    # A symlink, directory, device, non-empty file, or wrong metadata fails
    # without opening or changing the existing entry.
    [ ! -L "$sentinel" ]
    [ -f "$sentinel" ]
    [ "$("$stat_command" -c '%u:%g:%a:%s:%h' "$sentinel")" = "$volume_uid:$volume_gid:600:0:1" ]
else
    # Prepare and verify the private inode completely before publishing it.
    # The hard link is the final sentinel mutation and -T refuses to follow or
    # replace an entry that appears at the destination.
    temporary=$(mktemp "$volume_root/.teslatlas-volume-initialized.tmp.XXXXXX")
    chmod 0600 "$temporary"
    chown "$volume_uid:$volume_gid" "$temporary"
    [ ! -L "$temporary" ]
    [ -f "$temporary" ]
    [ "$("$stat_command" -c '%u:%g:%a:%s:%h' "$temporary")" = "$volume_uid:$volume_gid:600:0:1" ]
    if [ "$test_mode" -eq 1 ] && [ -n "${TESLATLAS_HUB_VOLUME_INIT_TEST_RACE_TARGET:-}" ]; then
        "$link_command" -s -- "$TESLATLAS_HUB_VOLUME_INIT_TEST_RACE_TARGET" "$sentinel"
    fi
    "$link_command" -T -- "$temporary" "$sentinel"
    [ "$("$stat_command" -c '%d:%i' "$temporary")" = "$("$stat_command" -c '%d:%i' "$sentinel")" ]
    rm -f -- "$temporary"
    temporary=
fi

"$install_command" -d -o "$volume_uid" -g "$volume_gid" -m 0700 "$volume_root"
[ "$("$stat_command" -c '%u:%g:%a' "$volume_root")" = "$volume_uid:$volume_gid:700" ]
[ ! -L "$sentinel" ]
[ -f "$sentinel" ]
[ "$("$stat_command" -c '%u:%g:%a:%s:%h' "$sentinel")" = "$volume_uid:$volume_gid:600:0:1" ]
