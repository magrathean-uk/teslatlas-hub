#!/bin/sh
set -eu

PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH

case "$#" in
    0) delete_data=0 ;;
    1) [ "$1" = --delete-data ] || { echo "usage: $0 [--delete-data]" >&2; exit 64; }; delete_data=1 ;;
    *) echo "usage: $0 [--delete-data]" >&2; exit 64 ;;
esac
[ "$(/usr/bin/id -u)" -eq 0 ] || { echo 'uninstaller must run as root' >&2; exit 1; }

data_root='/Users/Shared/TesatlasEdge'
agent_root='/Library/Application Support/Tesatlas Edge/launchagents'
labels='uk.co.magrathean.tesatlas-edge uk.co.magrathean.tesatlas-fleet-telemetry'
uid=$(/usr/bin/stat -f '%u' /dev/console)
case "$uid" in ''|0|*[!0-9]*) echo 'a signed-in console user is required' >&2; exit 1 ;; esac

safe_payload() {
    path=$1
    [ ! -e "$path" ] && [ ! -L "$path" ] && return 0
    [ -f "$path" ] && [ ! -L "$path" ] || { echo "unsafe package payload: $path" >&2; exit 1; }
    [ "$(/usr/bin/stat -f '%u:%g' "$path")" = '0:0' ] || {
        echo "package payload is not root-owned: $path" >&2
        exit 1
    }
    mode=$(/usr/bin/stat -f '%Lp' "$path")
    case "$mode" in
        *[2367][0-7]|*[0-7][2367]) echo "package payload is writable by group or world: $path" >&2; exit 1 ;;
    esac
}

if [ "$delete_data" -eq 1 ]; then
    [ -d "$data_root" ] && [ ! -L "$data_root" ] || { echo 'Edge state directory is missing or unsafe' >&2; exit 1; }
    [ "$(/usr/bin/stat -f '%u' "$data_root")" = "$uid" ] || {
        echo 'Edge state directory is not owned by the console user' >&2
        exit 1
    }
    data_mode=$(/usr/bin/stat -f '%Lp' "$data_root")
    case "$data_mode" in
        *[2367][0-7]|*[0-7][2367]) echo 'Edge state directory is writable by group or world' >&2; exit 1 ;;
    esac
fi

for label in $labels; do
    target="gui/$uid/$label"
    if /bin/launchctl print "$target" >/dev/null 2>&1; then
        /bin/launchctl bootout "$target"
        attempt=1
        while /bin/launchctl print "$target" >/dev/null 2>&1; do
            [ "$attempt" -lt 50 ] || { echo "LaunchAgent did not stop: $label" >&2; exit 1; }
            /bin/sleep 0.1
            attempt=$((attempt + 1))
        done
    fi
done

for path in \
    /usr/local/libexec/tesatlas-edge/tesatlas-edge \
    /usr/local/libexec/tesatlas-edge/tesatlas-fleet-telemetry \
    /usr/local/libexec/tesatlas-edge/run-with-spool-format-guard.sh \
    /usr/local/libexec/tesatlas-edge/tesatlas-edge-service.sh \
    /usr/local/libexec/tesatlas-edge/uninstall-tesatlas-edge.sh \
    "$agent_root/uk.co.magrathean.tesatlas-edge.plist" \
    "$agent_root/uk.co.magrathean.tesatlas-fleet-telemetry.plist"; do
    safe_payload "$path"
    [ ! -e "$path" ] || /bin/rm -f "$path"
done
/bin/rmdir /usr/local/libexec/tesatlas-edge 2>/dev/null || true
/bin/rmdir "$agent_root" 2>/dev/null || true
/bin/rmdir '/Library/Application Support/Tesatlas Edge' 2>/dev/null || true

if [ "$delete_data" -eq 1 ]; then
    /usr/bin/find -x "$data_root" -mindepth 1 -depth -delete
    /bin/rmdir "$data_root"
else
    echo "Tesatlas Edge removed; state preserved at $data_root"
fi
