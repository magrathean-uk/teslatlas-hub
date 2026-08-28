#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only

set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd)
# shellcheck source=common.sh
. "$SCRIPT_DIR/common.sh"

delete_data=0
case "${1-}" in
    '') ;;
    --delete-data) delete_data=1 ;;
    *) installer_error "usage: uninstall-macos-service.sh [--delete-data]" ;;
esac
[ "$#" -le 1 ] || installer_error "usage: uninstall-macos-service.sh [--delete-data]"

require_root
read_console_user

PLIST="$CONSOLE_HOME/Library/LaunchAgents/$LABEL.plist"
LOG_DIRECTORY="$CONSOLE_HOME/Library/Logs/Teslatlas Hub"
USER_ROOT="$CONSOLE_HOME/Library/Application Support/Teslatlas Hub"

other_user=
account_table=$(/usr/bin/dscl . -list /Users UniqueID 2>/dev/null) \
    || installer_error "cannot enumerate local users"
for account in $(printf '%s\n' "$account_table" | /usr/bin/awk '$2 >= 501 { print $1 }'); do
    [ "$account" = "$CONSOLE_USER" ] && continue
    account_uid=$(/usr/bin/id -u "$account" 2>/dev/null) \
        || installer_error "cannot inspect local user: $account"
    account_home=$(
        /usr/bin/dscl . -read "/Users/$account" NFSHomeDirectory 2>/dev/null \
            | /usr/bin/awk -F ': ' '/^NFSHomeDirectory: / { print $2; exit }'
    )
    case "$account_home" in
        /*)
            if [ -f "$account_home/Library/LaunchAgents/$LABEL.plist" ] \
                || /bin/launchctl print "gui/$account_uid/$LABEL" >/dev/null 2>&1; then
                other_user=$account
                break
            fi
            ;;
        *) installer_error "cannot inspect local user home: $account" ;;
    esac
done

# Validate every deletion target before changing service or filesystem state.
if [ -e "$PLIST" ] || [ -L "$PLIST" ]; then
    require_safe_regular_file "$PLIST" "$CONSOLE_UID" "LaunchAgent"
fi
if [ -e "$LOG_DIRECTORY" ] || [ -L "$LOG_DIRECTORY" ]; then
    require_safe_owned_tree "$LOG_DIRECTORY" "$CONSOLE_UID" "Hub logs"
fi
if [ "$delete_data" -eq 1 ] && { [ -e "$USER_ROOT" ] || [ -L "$USER_ROOT" ]; }; then
    require_safe_owned_tree "$USER_ROOT" "$CONSOLE_UID" "Hub data"
fi
if [ -z "$other_user" ] && { [ -e "$BINARY_ROOT" ] || [ -L "$BINARY_ROOT" ]; }; then
    require_safe_owned_tree "$BINARY_ROOT" 0 "Hub service payload"
fi

bootout_if_loaded
remove_owned_file "$PLIST" "$CONSOLE_UID" "LaunchAgent"
remove_owned_tree "$LOG_DIRECTORY" "$CONSOLE_UID" "$CONSOLE_HOME/Library/Logs/Teslatlas Hub" "Hub logs"
if [ "$delete_data" -eq 1 ]; then
    remove_owned_tree "$USER_ROOT" "$CONSOLE_UID" "$CONSOLE_HOME/Library/Application Support/Teslatlas Hub" "Hub data"
fi
if [ -z "$other_user" ]; then
    remove_owned_tree "$BINARY_ROOT" 0 '/Library/Application Support/Teslatlas Hub' "Hub service payload"
    /usr/sbin/pkgutil --forget com.teslatlas.hub.service >/dev/null 2>&1 || true
else
    printf '%s\n' "Shared Hub service payload retained for: $other_user"
fi

printf '%s\n' "Teslatlas Hub service uninstalled."
if [ "$delete_data" -eq 0 ]; then
    printf '%s\n' "Hub database and configuration preserved at: $USER_ROOT"
fi
