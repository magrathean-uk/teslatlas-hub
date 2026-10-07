#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only

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

user_file_status() {
    path=$1
    expected_uid=$2
    [ -e "$path" ] || { [ -L "$path" ] && printf '%s\n' symlink || printf '%s\n' missing; return; }
    [ ! -L "$path" ] || { printf '%s\n' symlink; return; }
    [ -f "$path" ] || { printf '%s\n' not_regular; return; }
    [ "$(/usr/bin/stat -f '%u' "$path")" = "$expected_uid" ] || { printf '%s\n' wrong_owner; return; }
    [ -r "$path" ] || { printf '%s\n' unreadable; return; }
    mode=$(/usr/bin/stat -f '%Lp' "$path") || { printf '%s\n' unreadable; return; }
    case "$mode" in
        *[1-7][0-7]|*[0-7][1-7]) printf '%s\n' unsafe_permissions ;;
        *) printf '%s\n' safe ;;
    esac
}

configuration_blocked() {
    status=$(user_file_status "$1" "$(/usr/bin/id -u)")
    [ "$status" = safe ] && return 1
    printf '%s\n' "Teslatlas Hub service: configuration blocked ($status); correct the file and start Hub again" >&2
    return 0
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
configuration_blocked "$CONFIG" && exit 0
compact_log "$STDOUT_LOG"
compact_log "$STDERR_LOG"
safe_root_executable "$HUB" || {
    printf '%s\n' 'Teslatlas Hub service: Hub executable is unsafe' >&2
    exit 1
}

DATA_ROOT=$(/usr/bin/dirname "$CONFIG")
RECEIVER_CONFIG="$DATA_ROOT/fleet-telemetry.json"
receiver_pid=
if [ ! -e "$RECEIVER_CONFIG" ] && [ ! -L "$RECEIVER_CONFIG" ]; then
    "$HUB" --config "$CONFIG" serve &
    hub_pid=$!
else
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

    # The Hub starts tesla-http-proxy itself with its already validated private
    # key, TLS certificate/key, and session-cache paths. Do not start a second
    # proxy here: a duplicate would race its loopback listener.
    "$HUB" --config "$CONFIG" serve &
    hub_pid=$!
    TESLATLAS_FLEET_TELEMETRY_BEARER_FILE="$BEARER" \
        "$RECEIVER" -config="$RECEIVER_CONFIG" &
    receiver_pid=$!
fi

stop_child() {
    child=$1
    allowance=$2
    [ -n "$child" ] || return 0
    # Use the shell's still-owned job, rather than signal a numeric PID after
    # the shell may have reaped it. No new background job starts during finish.
    job=$(jobs -l | /usr/bin/awk -v wanted="$child" '
        $2 == wanted && ($3 == "Running" || $3 == "Stopped") {
            gsub(/[^0-9]/, "", $1); print "%" $1
        }')
    if [ -n "$job" ]; then
        kill -TERM "$job" >/dev/null 2>&1 || true
        remaining=$allowance
        while [ "$remaining" -gt 0 ]; do
            running_job=$(jobs -l | /usr/bin/awk -v wanted="$child" '
                $2 == wanted && ($3 == "Running" || $3 == "Stopped") {
                    gsub(/[^0-9]/, "", $1); print "%" $1
                }')
            [ "$running_job" = "$job" ] || break
            /bin/sleep 1
            remaining=$((remaining - 1))
        done
        kill -KILL "$job" >/dev/null 2>&1 || true
    fi
    wait "$child" >/dev/null 2>&1 || true
}

finish() {
    status=$?
    trap - EXIT HUP INT TERM
    # Rust permits serial server/collector waits of 120s each and a 5s proxy
    # wait. Keep the packaged launchd ExitTimeOut above this complete budget.
    stop_child "$hub_pid" 250
    stop_child "$receiver_pid" 5
    exit "$status"
}
trap finish EXIT HUP INT TERM

log_check_seconds=0
while /bin/kill -0 "$hub_pid" >/dev/null 2>&1 \
    && { [ -z "$receiver_pid" ] || /bin/kill -0 "$receiver_pid" >/dev/null 2>&1; }; do
    /bin/sleep 1
    log_check_seconds=$((log_check_seconds + 1))
    if [ "$log_check_seconds" -ge 30 ]; then
        compact_log "$STDOUT_LOG"
        compact_log "$STDERR_LOG"
        log_check_seconds=0
    fi
done

if [ -z "$receiver_pid" ] || ! /bin/kill -0 "$hub_pid" >/dev/null 2>&1; then
    wait "$hub_pid"
    exit $?
fi
wait "$receiver_pid"
