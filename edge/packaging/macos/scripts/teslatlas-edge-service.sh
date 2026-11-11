#!/bin/sh
set -eu

PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH

usage() {
    echo "usage: $0 edge|receiver start|stop|restart|status" >&2
    exit 64
}
[ "$#" -eq 2 ] || usage
role=$1
action=$2
case "$role" in
    edge)
        label=uk.co.magrathean.teslatlas-edge
        plist='/Library/Application Support/Teslatlas Edge/launchagents/uk.co.magrathean.teslatlas-edge.plist'
        required='/Users/Shared/TeslatlasEdge/config.toml'
        ;;
    receiver)
        label=uk.co.magrathean.teslatlas-fleet-telemetry
        plist='/Library/Application Support/Teslatlas Edge/launchagents/uk.co.magrathean.teslatlas-fleet-telemetry.plist'
        required='/Users/Shared/TeslatlasEdge/fleet-telemetry.json'
        ;;
    *) usage ;;
esac
case "$action" in start|stop|restart|status) ;; *) usage ;; esac

uid=$(/usr/bin/stat -f '%u' /dev/console)
case "$uid" in ''|0|*[!0-9]*) echo 'a signed-in console user is required' >&2; exit 1 ;; esac
domain="gui/$uid"
target="$domain/$label"
loaded() { /bin/launchctl print "$target" >/dev/null 2>&1; }
wait_unloaded() {
    attempt=1
    while [ "$attempt" -le 50 ]; do
        loaded || return 0
        [ "$attempt" -eq 50 ] || /bin/sleep 0.1
        attempt=$((attempt + 1))
    done
    return 1
}

case "$action" in
    status)
        if loaded; then
            echo "$role active"
        else
            echo "$role inactive"
            exit 3
        fi
        ;;
    start)
        [ -f "$required" ] && [ ! -L "$required" ] || {
            echo "required configuration is missing: $required" >&2
            exit 1
        }
        [ -f "$plist" ] && [ ! -L "$plist" ] || {
            echo "LaunchAgent is missing or unsafe: $plist" >&2
            exit 1
        }
        loaded || /bin/launchctl bootstrap "$domain" "$plist"
        loaded || { echo "LaunchAgent did not load: $label" >&2; exit 1; }
        ;;
    stop)
        if loaded; then
            /bin/launchctl bootout "$target"
            wait_unloaded || { echo "LaunchAgent did not stop: $label" >&2; exit 1; }
        fi
        ;;
    restart)
        "$0" "$role" stop
        "$0" "$role" start
        ;;
esac
