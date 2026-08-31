#!/bin/sh
set -eu

fail() {
    echo "spool format is incompatible with candidate Edge binary" >&2
    exit 78
}

[ "$#" -ge 4 ] || fail
edge_binary=$1
spool_directory=$2
shift 2
[ "$1" = "--" ] || fail
shift
[ "$#" -ge 1 ] || fail

if [ -L "$spool_directory" ]; then
    fail
fi
if [ -e "$spool_directory" ] && [ ! -d "$spool_directory" ]; then
    fail
fi

format_marker=$spool_directory/FORMAT
if [ -L "$format_marker" ]; then
    fail
fi
if [ -e "$format_marker" ]; then
    [ -f "$format_marker" ] || fail
    marker_bytes=$(/usr/bin/wc -c < "$format_marker" | /usr/bin/tr -d '[:space:]')
    case "$marker_bytes" in
        ''|*[!0-9]*) fail ;;
    esac
    [ "$marker_bytes" -le 8 ] || fail
    marker_format=
    IFS= read -r marker_format < "$format_marker" || fail
    case "$marker_format" in
        ''|*[!0-9]*) fail ;;
    esac
    [ "$marker_bytes" = "$((${#marker_format} + 1))" ] || fail
    candidate_format=$("$edge_binary" storage-format 2>/dev/null) || fail
    [ "$candidate_format" = "$marker_format" ] || fail
fi

exec "$edge_binary" "$@"
