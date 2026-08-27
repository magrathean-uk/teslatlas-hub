#!/bin/sh

# The LaunchAgent runs this as the logged-in user. It keeps the optional Fleet
# receiver in the same failure domain as the Hub without granting either
# process root privileges. The receiver listens on the port configured in its
# user-owned JSON (the shipped example uses 8443, not privileged 443).

set -eu

PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH

BINARY_ROOT='/Library/Application Support/Teslatlas Hub'
HUB="$BINARY_ROOT/bin/teslatlas-hub"
PROXY="$BINARY_ROOT/bin/tesla-http-proxy"
RECEIVER="$BINARY_ROOT/bin/fleet-telemetry"

usage() {
    printf '%s\n' 'usage: run-hub-service.sh --config PATH --stdout-log PATH --stderr-log PATH' >&2
    exit 64
}

safe_user_file() {
    path=$1
    [ -f "$path" ] && [ ! -L "$path" ] || return 1
    [ "$(/usr/bin/stat -f '%u' "$path")" = "$(/usr/bin/id -u)" ] || return 1
    mode=$(/usr/bin/stat -f '%Lp' "$path") || return 1
    case "$mode" in
        *[2367][0-7]|*[0-7][2367]) return 1 ;;
    esac
}

safe_root_executable() {
    path=$1
    [ -f "$path" ] && [ ! -L "$path" ] && [ -x "$path" ] || return 1
    [ "$(/usr/bin/stat -f '%u' "$path")" = 0 ] || return 1
    mode=$(/usr/bin/stat -f '%Lp' "$path") || return 1
    case "$mode" in
        *[2367][0-7]|*[0-7][2367]) return 1 ;;
    esac
}

# launchd appends to these files but does not rotate them. Compact oversized
# logs in place before every service launch. Keeping the inode avoids racing
# launchd's already-open descriptor.
compact_log() {
    path=$1
    safe_user_file "$path" || {
        printf '%s\n' 'Teslatlas Hub service: log is not a private regular file' >&2
        exit 1
    }
    bytes=$(/usr/bin/stat -f '%z' "$path") || exit 1
    [ "$bytes" -le 1048576 ] && return 0
    directory=$(/usr/bin/dirname "$path")
    temporary=$(/usr/bin/mktemp "$directory/.hub-log.XXXXXX") || exit 1
    /bin/chmod 0600 "$temporary" || {
        /bin/rm -f "$temporary"
        exit 1
    }
    if /usr/bin/tail -c 524288 "$path" >"$temporary" \
            && /bin/cat "$temporary" >"$path"; then
        /bin/rm -f "$temporary"
        return 0
    fi
    /bin/rm -f "$temporary"
    exit 1
}

fleet_telemetry_bearer() {
    config=$1
    /usr/bin/awk '
        BEGIN { single_quote = sprintf("%c", 39) }
        /^[[:space:]]*\[[^]]+\][[:space:]]*(#.*)?$/ {
            telemetry = ($0 ~ /^[[:space:]]*\[collector\.fleet_telemetry\][[:space:]]*(#.*)?$/)
            next
        }
        telemetry && /^[[:space:]]*hostname[[:space:]]*=/ { hostname = 1 }
        telemetry && /^[[:space:]]*ingest_token_path[[:space:]]*=/ {
            value = $0
            sub(/^[^=]*=[[:space:]]*/, "", value)
            sub(/[[:space:]]+$/, "", value)
            quote = substr(value, 1, 1)
            rest = substr(value, 2)
            if ((quote == "\"" || quote == single_quote) &&
                    !(quote == "\"" && rest ~ /\\/)) {
                closing = index(rest, quote)
                if (closing > 1) {
                    candidate = substr(rest, 1, closing - 1)
                    tail = substr(rest, closing + 1)
                    if (tail ~ /^[[:space:]]*(#.*)?$/) bearer = candidate
                }
            }
        }
        END {
            if (hostname && bearer != "") print bearer
        }
    ' "$config"
}

if [ "$#" -ne 6 ] || [ "$1" != '--config' ] \
        || [ "$3" != '--stdout-log' ] || [ "$5" != '--stderr-log' ]; then
    usage
fi
CONFIG=$2
STDOUT_LOG=$4
STDERR_LOG=$6
safe_user_file "$CONFIG" || {
    printf '%s\n' 'Teslatlas Hub service: configuration is not a private regular file' >&2
    exit 1
}
compact_log "$STDOUT_LOG"
compact_log "$STDERR_LOG"
safe_root_executable "$HUB" || {
    printf '%s\n' 'Teslatlas Hub service: Hub executable is unsafe' >&2
    exit 1
}

DATA_ROOT=$(/usr/bin/dirname "$CONFIG")
RECEIVER_CONFIG="$DATA_ROOT/fleet-telemetry.json"
if [ ! -e "$RECEIVER_CONFIG" ] && [ ! -L "$RECEIVER_CONFIG" ]; then
    exec "$HUB" --config "$CONFIG" serve
fi

safe_user_file "$RECEIVER_CONFIG" || {
    printf '%s\n' 'Teslatlas Hub service: Fleet receiver config is not a private regular file' >&2
    exit 1
}
safe_root_executable "$RECEIVER" || {
    printf '%s\n' 'Teslatlas Hub service: Fleet receiver executable is unsafe' >&2
    exit 1
}
safe_root_executable "$PROXY" || {
    printf '%s\n' 'Teslatlas Hub service: Tesla command proxy executable is unsafe' >&2
    exit 1
}
BEARER=$(fleet_telemetry_bearer "$CONFIG")
case "$BEARER" in
    /*) ;;
    *)
        printf '%s\n' 'Teslatlas Hub service: Fleet receiver config exists but native Fleet Telemetry is not enabled' >&2
        exit 1
        ;;
esac
safe_user_file "$BEARER" || {
    printf '%s\n' 'Teslatlas Hub service: Fleet Telemetry bearer is not a private regular file' >&2
    exit 1
}

# The Hub starts tesla-http-proxy itself with its already validated private key,
# TLS certificate/key, and session-cache paths. Do not start a second proxy
# here: a duplicate would race its loopback listener and leak operational
# details into this service wrapper. The Hub exiting therefore also stops its
# proxy before this wrapper stops the receiver.
"$HUB" --config "$CONFIG" serve &
hub_pid=$!
TESLATLAS_FLEET_TELEMETRY_BEARER_FILE="$BEARER" \
    "$RECEIVER" -config="$RECEIVER_CONFIG" &
receiver_pid=$!

stop_child() {
    child=$1
    if /bin/kill -0 "$child" >/dev/null 2>&1; then
        /bin/kill -TERM "$child" >/dev/null 2>&1 || true
        /bin/sleep 2
        /bin/kill -KILL "$child" >/dev/null 2>&1 || true
    fi
    wait "$child" >/dev/null 2>&1 || true
}

finish() {
    status=$?
    trap - EXIT HUP INT TERM
    stop_child "$hub_pid"
    stop_child "$receiver_pid"
    exit "$status"
}
trap finish EXIT HUP INT TERM

while /bin/kill -0 "$hub_pid" >/dev/null 2>&1 \
    && /bin/kill -0 "$receiver_pid" >/dev/null 2>&1; do
    /bin/sleep 1
done

if ! /bin/kill -0 "$hub_pid" >/dev/null 2>&1; then
    wait "$hub_pid"
    exit $?
fi
wait "$receiver_pid"
