#!/bin/sh

set -eu

PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH

LABEL=com.teslatlas.hub
BINARY_ROOT='/Library/Application Support/Teslatlas Hub'
# Consumed by scripts that source this file.
# shellcheck disable=SC2034
BINARY="$BINARY_ROOT/bin/teslatlas-hub"

installer_error() {
    printf '%s\n' "Teslatlas Hub installer: $*" >&2
    exit 1
}

require_root() {
    [ "$(/usr/bin/id -u)" -eq 0 ] || installer_error "must run as root"
}

read_console_user() {
    CONSOLE_USER=$(/usr/bin/stat -f '%Su' /dev/console) || installer_error "cannot identify console user"
    case "$CONSOLE_USER" in
        ''|root|loginwindow|_mbsetupuser)
            installer_error "a logged-in console user is required"
            ;;
    esac

    CONSOLE_UID=$(/usr/bin/id -u "$CONSOLE_USER" 2>/dev/null) \
        || installer_error "console user is not a local account"
    CONSOLE_GID=$(/usr/bin/id -g "$CONSOLE_USER" 2>/dev/null) \
        || installer_error "cannot read console user group"
    case "$CONSOLE_UID:$CONSOLE_GID" in
        *[!0-9:]*|:*) installer_error "console user identity is invalid" ;;
    esac
    [ "$CONSOLE_UID" -ge 501 ] || installer_error "console user is not a regular account"

    CONSOLE_HOME=$(
        /usr/bin/dscl . -read "/Users/$CONSOLE_USER" NFSHomeDirectory 2>/dev/null \
            | /usr/bin/awk -F ': ' '/^NFSHomeDirectory: / { print $2; exit }'
    )
    case "$CONSOLE_HOME" in
        /*) ;;
        *) installer_error "console user home is invalid" ;;
    esac
    [ -d "$CONSOLE_HOME" ] && [ ! -L "$CONSOLE_HOME" ] \
        || installer_error "console user home is unavailable"
    [ "$(/usr/bin/stat -f '%u' "$CONSOLE_HOME")" = "$CONSOLE_UID" ] \
        || installer_error "console user does not own its home"
}

refuse_other_user_launch_agents() {
    account_table=$(/usr/bin/dscl . -list /Users UniqueID 2>/dev/null) \
        || installer_error "cannot enumerate local users"
    for account in $(printf '%s\n' "$account_table" | /usr/bin/awk '$2 >= 501 { print $1 }'); do
        [ "$account" = "$CONSOLE_USER" ] && continue
        account_home=$(
            /usr/bin/dscl . -read "/Users/$account" NFSHomeDirectory 2>/dev/null \
                | /usr/bin/awk -F ': ' '/^NFSHomeDirectory: / { print $2; exit }'
        )
        case "$account_home" in
            /*) ;;
            *) installer_error "cannot inspect local user home: $account" ;;
        esac
        other_plist="$account_home/Library/LaunchAgents/$LABEL.plist"
        if [ -e "$other_plist" ] || [ -L "$other_plist" ]; then
            installer_error "shared Hub binary upgrade refused while another user has a Hub LaunchAgent: $account"
        fi
    done
}

service_target() {
    printf 'gui/%s/%s\n' "$CONSOLE_UID" "$LABEL"
}

service_domain() {
    printf 'gui/%s\n' "$CONSOLE_UID"
}

ensure_user_directory() {
    directory=$1
    mode=$2
    if [ -e "$directory" ] || [ -L "$directory" ]; then
        require_safe_owned_tree "$directory" "$CONSOLE_UID" "user directory"
        /bin/chmod "$mode" "$directory" \
            || installer_error "cannot protect user directory"
        return
    fi
    /usr/bin/install -d -o "$CONSOLE_UID" -g "$CONSOLE_GID" -m "$mode" "$directory" \
        || installer_error "cannot create user directory"
}

bootout_if_loaded() {
    target=$(service_target)
    if /bin/launchctl print "$target" >/dev/null 2>&1; then
        /bin/launchctl bootout "$target" \
            || installer_error "cannot stop existing Hub service"
        if /bin/launchctl print "$target" >/dev/null 2>&1; then
            installer_error "existing Hub service is still loaded"
        fi
    fi
}

service_is_loaded() {
    /bin/launchctl print "$(service_target)" >/dev/null 2>&1
}

upgrade_state_directory() {
    printf '/private/var/tmp/com.teslatlas.hub-upgrade-%s\n' "$CONSOLE_UID"
}

require_safe_upgrade_state() {
    directory=$1
    [ -d "$directory" ] && [ ! -L "$directory" ] \
        || installer_error "upgrade state is missing or unsafe"
    [ "$(/usr/bin/stat -f '%u:%g:%Lp' "$directory")" = '0:0:700' ] \
        || installer_error "upgrade state has unsafe ownership or permissions"
}

remove_upgrade_state() {
    directory=$1
    case "$directory" in
        /private/var/tmp/com.teslatlas.hub-upgrade-[0-9]*) ;;
        *) installer_error "refusing unsafe upgrade state path" ;;
    esac
    if [ -e "$directory" ] || [ -L "$directory" ]; then
        require_safe_upgrade_state "$directory"
        /usr/bin/find -x "$directory" -depth -delete \
            || installer_error "cannot remove upgrade state"
    fi
}

require_safe_regular_file() {
    path=$1
    expected_uid=$2
    description=$3
    [ -f "$path" ] && [ ! -L "$path" ] \
        || installer_error "$description is not a safe regular file"
    [ "$(/usr/bin/stat -f '%u' "$path")" = "$expected_uid" ] \
        || installer_error "$description has unexpected owner"
    mode=$(/usr/bin/stat -f '%Lp' "$path")
    case "$mode" in
        *[2367][0-7]|*[0-7][2367]) installer_error "$description is group/world writable" ;;
    esac
}

remove_owned_file() {
    path=$1
    expected_uid=$2
    description=$3
    if [ -e "$path" ] || [ -L "$path" ]; then
        require_safe_regular_file "$path" "$expected_uid" "$description"
        /bin/rm -f "$path" || installer_error "cannot remove $description"
    fi
}

require_safe_owned_tree() {
    directory=$1
    expected_uid=$2
    description=$3
    [ -d "$directory" ] && [ ! -L "$directory" ] \
        || installer_error "$description is not a safe directory"
    [ "$(/usr/bin/stat -f '%u' "$directory")" = "$expected_uid" ] \
        || installer_error "$description has unexpected owner"
    mode=$(/usr/bin/stat -f '%Lp' "$directory")
    case "$mode" in
        *[2367][0-7]|*[0-7][2367]) installer_error "$description is group/world writable" ;;
    esac
}

remove_owned_tree() {
    directory=$1
    expected_uid=$2
    allowed=$3
    description=$4
    [ "$directory" = "$allowed" ] || installer_error "refusing unsafe $description path"
    if [ -e "$directory" ] || [ -L "$directory" ]; then
        require_safe_owned_tree "$directory" "$expected_uid" "$description"
        /usr/bin/find -x "$directory" -depth -delete \
            || installer_error "cannot remove $description"
    fi
}

xml_sed_replacement() {
    printf '%s' "$1" | /usr/bin/sed \
        -e 's/&/\&amp;/g' \
        -e 's/</\&lt;/g' \
        -e 's/>/\&gt;/g' \
        -e 's/"/\&quot;/g' \
        -e "s/'/\&apos;/g" \
        -e 's/[\\&|]/\\&/g'
}
