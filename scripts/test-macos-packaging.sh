#!/bin/sh
# Literal dollar-sign patterns below inspect package source text.
# shellcheck disable=SC2016

set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
SCRIPTS="$ROOT/packaging/macos-service/scripts"
COMMON="$SCRIPTS/common.sh"
PREINSTALL="$SCRIPTS/preinstall"
POSTINSTALL="$SCRIPTS/postinstall"
UNINSTALL="$SCRIPTS/uninstall-macos-service.sh"
PLIST="$ROOT/packaging/macos-service/com.teslatlas.hub.plist.in"
CLI_PLIST="$ROOT/packaging/com.teslatlas.hub.plist.in"

fail() {
    printf '%s\n' "test-macos-packaging: $*" >&2
    exit 1
}

line_of() {
    /usr/bin/grep -n -m 1 "$1" "$2" | /usr/bin/cut -d: -f1
}

assert_before() {
    first=$(line_of "$1" "$3") || fail "missing pattern: $1"
    second=$(line_of "$2" "$3") || fail "missing pattern: $2"
    [ "$first" -lt "$second" ] || fail "wrong operation order: $1 must precede $2"
}

line_of_fixed() {
    /usr/bin/grep -Fn -m 1 "$1" "$2" | /usr/bin/cut -d: -f1
}

assert_before_fixed() {
    first=$(line_of_fixed "$1" "$3") || fail "missing text: $1"
    second=$(line_of_fixed "$2" "$3") || fail "missing text: $2"
    [ "$first" -lt "$second" ] || fail "wrong operation order: $1 must precede $2"
}

for script in "$SCRIPTS/common.sh" "$PREINSTALL" "$POSTINSTALL" "$UNINSTALL"; do
    /bin/sh -n "$script" || fail "invalid shell syntax: $script"
done
/usr/bin/plutil -lint "$PLIST" >/dev/null || fail "invalid LaunchAgent template"
/usr/bin/plutil -lint "$CLI_PLIST" >/dev/null || fail "invalid CLI LaunchAgent template"
/usr/bin/grep -q '<integer>63</integer>' "$PLIST" \
    || fail "packaged LaunchAgent does not set a private umask"
/usr/bin/grep -q '<integer>63</integer>' "$CLI_PLIST" \
    || fail "CLI LaunchAgent does not set a private umask"
/usr/bin/grep -Fq 'require_safe_owned_tree "$directory" "$CONSOLE_UID" "user directory"' "$COMMON" \
    || fail "existing user directories are not safely admitted"

if /usr/bin/grep -q 'bootout_if_loaded' "$PREINSTALL"; then
    fail "preinstall must not stop the running service"
fi
/usr/bin/grep -q 'STATE_DIRECTORY/was-loaded' "$PREINSTALL" \
    || fail "preinstall does not record loaded state"
/usr/bin/grep -q 'STATE_DIRECTORY/teslatlas-hub' "$PREINSTALL" \
    || fail "preinstall does not preserve the old binary"
/usr/bin/grep -q 'STATE_DIRECTORY/launch-agent.plist' "$PREINSTALL" \
    || fail "preinstall does not preserve the old LaunchAgent"

assert_before 'plutil -lint' 'bootout_if_loaded' "$POSTINSTALL"
assert_before 'bootout_if_loaded' 'preflight >/dev/null' "$POSTINSTALL"
assert_before_fixed 'if [ -f "$STATE_DIRECTORY/was-loaded" ]; then' 'if service_is_loaded; then' "$POSTINSTALL"
/usr/bin/grep -Fq '/bin/chmod 0700 "$LOG_DIRECTORY"' "$POSTINSTALL" \
    || fail "Hub log directory is not normalized to mode 0700"
/usr/bin/grep -Fq '/bin/chmod 0600 "$log"' "$POSTINSTALL" \
    || fail "existing Hub logs are not normalized to mode 0600"
/usr/bin/grep -q 'completed.*-ne 1' "$POSTINSTALL" \
    || fail "postinstall has no failure rollback"
/usr/bin/grep -q 'launchctl bootstrap' "$POSTINSTALL" \
    || fail "postinstall cannot restart the prior service"
/usr/bin/grep -q 'previously_installed' "$POSTINSTALL" \
    || fail "postinstall does not preserve an intentionally stopped upgrade"

/usr/bin/grep -q 'delete_data=0' "$UNINSTALL" \
    || fail "uninstall must preserve data by default"
/usr/bin/grep -q -- '--delete-data' "$UNINSTALL" \
    || fail "uninstall has no explicit data-deletion option"
if /usr/bin/grep -Fq 'another local user still has Teslatlas Hub installed' "$UNINSTALL"; then
    fail "another user must not block current-user cleanup"
fi
assert_before_fixed 'remove_owned_file "$PLIST"' 'if [ -z "$other_user" ]; then' "$UNINSTALL"
/usr/bin/grep -Fq 'Shared Hub service payload retained for:' "$UNINSTALL" \
    || fail "multi-user uninstall does not retain the shared payload"

printf '%s\n' 'macOS packaging source checks passed'
