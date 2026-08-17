#!/bin/sh

set -eu

PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH

LABEL=com.teslatlas.hub

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
        [ -d "$directory" ] && [ ! -L "$directory" ] \
            || installer_error "unsafe user directory"
        [ "$(/usr/bin/stat -f '%u' "$directory")" = "$CONSOLE_UID" ] \
            || installer_error "user directory has unexpected owner"
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

xml_sed_replacement() {
    printf '%s' "$1" | /usr/bin/sed \
        -e 's/&/\&amp;/g' \
        -e 's/</\&lt;/g' \
        -e 's/>/\&gt;/g' \
        -e 's/"/\&quot;/g' \
        -e "s/'/\&apos;/g" \
        -e 's/[\\&|]/\\&/g'
}
