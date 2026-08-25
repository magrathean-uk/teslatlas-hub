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
APP_BUILD="$ROOT/scripts/build-macos-app.sh"
SERVICE_BUILD="$ROOT/scripts/build-macos-service-package.sh"
APP_INFO="$ROOT/macos/TeslatlasHubApp/TeslatlasHubApp/Info.plist"
TEST_ROOT=$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/teslatlas-hub-macos-package-test.XXXXXX")
trap '/usr/bin/find "$TEST_ROOT" -depth -delete' EXIT HUP INT TERM

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
/bin/sh -n "$APP_BUILD" || fail "invalid macOS app build script"
/bin/sh -n "$SERVICE_BUILD" || fail "invalid macOS service build script"
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
/usr/bin/grep -Fq 'refuse_other_user_launch_agents' "$PREINSTALL" \
    || fail "preinstall does not refuse unsafe multi-user shared-binary upgrades"
/usr/bin/grep -Fq 'another user has a Hub LaunchAgent' "$COMMON" \
    || fail "multi-user upgrade refusal has no clear diagnostic"

assert_before_fixed '/usr/bin/plutil -lint "$rendered_plist"' '    stop_loaded_service_bounded' "$POSTINSTALL"
assert_before_fixed '    stop_loaded_service_bounded' '"$BINARY" --config "$CONFIG" preflight' "$POSTINSTALL"
assert_before_fixed 'if [ -f "$STATE_DIRECTORY/was-loaded" ]; then' '    stop_loaded_service_bounded' "$POSTINSTALL"
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
/usr/bin/grep -Fq '"$BINARY" --config "$CONFIG" bootstrap' "$POSTINSTALL" \
    || fail "postinstall cannot perform a forward-only schema migration"
/usr/bin/grep -Fq '[ "$completed" -ne 1 ] && [ "$forward_only" -eq 0 ]' "$POSTINSTALL" \
    || fail "postinstall can restore an old binary after schema migration"
/usr/bin/grep -Fq 'TESLATLAS_FORWARD_ONLY_UPGRADE:' "$POSTINSTALL" \
    || fail "postinstall does not expose the forward-only failure boundary"
assert_before_fixed '"$BINARY" --config "$CONFIG" preflight' '"$BINARY" --config "$CONFIG" bootstrap' "$POSTINSTALL"
/usr/bin/grep -Fq '"ready":[[:space:]]*true' "$POSTINSTALL" \
    || fail "postinstall has no bounded readiness check"
/usr/bin/grep -Fq 'new Hub did not become ready' "$POSTINSTALL" \
    || fail "postinstall does not fail an unhealthy update"
/usr/bin/grep -Fq 'run_with_deadline "$preflight_timeout_seconds"' "$POSTINSTALL" \
    || fail "postinstall preflight has no wall-clock deadline"
/usr/bin/grep -Fq 'run_with_deadline "$status_timeout_seconds"' "$POSTINSTALL" \
    || fail "postinstall status has no wall-clock deadline"
/usr/bin/grep -Fq 'run_with_deadline "$service_timeout_seconds"' "$POSTINSTALL" \
    || fail "postinstall service actions have no wall-clock deadline"
/usr/bin/grep -Fq 'rendered_plist="$STATE_DIRECTORY/launch-agent.rendered"' "$POSTINSTALL" \
    || fail "LaunchAgent is not rendered in root-owned upgrade state"
/usr/bin/grep -Fq '/bin/mv -fh "$temporary_plist" "$PLIST"' "$POSTINSTALL" \
    || fail "LaunchAgent installation can follow a destination symlink"
assert_before_fixed '/usr/sbin/chown "$CONSOLE_UID:$CONSOLE_GID" "$temporary_plist"' '/bin/mv -fh "$temporary_plist" "$PLIST"' "$POSTINSTALL"
/usr/bin/grep -Fq 'CURRENT_PROJECT_VERSION="$bundle_version"' "$APP_BUILD" \
    || fail "app build does not preserve prerelease build identity"
/usr/bin/grep -Fq 'TESLATLAS_HUB_VERSION="$version"' "$APP_BUILD" \
    || fail "app build does not expose exact Hub version"
/usr/bin/grep -Fq -- '--version "$package_version"' "$SERVICE_BUILD" \
    || fail "service package does not use mapped prerelease identity"
/usr/bin/grep -Fq '<key>TeslatlasHubVersion</key>' "$APP_INFO" \
    || fail "app bundle does not carry exact Hub version"

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

deadline_helper="$TEST_ROOT/deadline-helper.sh"
/usr/bin/sed -n '/^# BEGIN TESTABLE DEADLINE HELPER$/,/^# END TESTABLE DEADLINE HELPER$/p' \
    "$POSTINSTALL" > "$deadline_helper"
# shellcheck source=/dev/null
. "$deadline_helper"
# shellcheck disable=SC2034 # consumed by the sourced deadline helper
STATE_DIRECTORY="$TEST_ROOT"
# shellcheck disable=SC2034 # consumed by the sourced deadline helper
deadline_sequence=0
run_with_deadline 1 1 /usr/bin/true || fail "deadline helper rejected a completed command"
started=$(/bin/date +%s)
if run_with_deadline 1 1 /bin/sh -c 'trap "" TERM; while :; do /bin/sleep 1; done' \
    >/dev/null 2>&1; then
    fail "deadline helper accepted a hung command"
else
    deadline_status=$?
fi
[ "$deadline_status" -eq 124 ] || fail "deadline helper returned the wrong timeout status"
elapsed=$(( $(/bin/date +%s) - started ))
[ "$elapsed" -le 4 ] || fail "deadline helper exceeded its termination grace"
[ -z "$(/usr/bin/find "$TEST_ROOT" -name 'deadline-expired.*' -print -quit)" ] \
    || fail "deadline helper retained timeout state"

macho_helper="$TEST_ROOT/macho-helper.sh"
/usr/bin/sed -n '/^# BEGIN TESTABLE MACH-O HELPER$/,/^# END TESTABLE MACH-O HELPER$/p' \
    "$SERVICE_BUILD" > "$macho_helper"
# shellcheck source=/dev/null
. "$macho_helper"
/usr/bin/printf '%s\n' 'int main(void) { return 0; }' \
    | /usr/bin/clang -arch arm64 -mmacosx-version-min=12.0 -x c - -o "$TEST_ROOT/executable"
/usr/bin/printf '%s\n' 'int sample(void) { return 0; }' \
    | /usr/bin/clang -arch arm64 -mmacosx-version-min=12.0 -dynamiclib -x c - -o "$TEST_ROOT/library.dylib"
/usr/bin/printf '%s\n' 'int sample(void) { return 0; }' \
    | /usr/bin/clang -arch arm64 -mmacosx-version-min=12.0 -c -x c - -o "$TEST_ROOT/member.o"
/usr/bin/ar rcs "$TEST_ROOT/library.a" "$TEST_ROOT/member.o"
/bin/chmod +x "$TEST_ROOT/library.dylib" "$TEST_ROOT/library.a"
is_executable_macho "$TEST_ROOT/executable" || fail "Mach-O executable was rejected"
if is_executable_macho "$TEST_ROOT/library.dylib"; then
    fail "Mach-O dylib was accepted as a service executable"
fi
if is_executable_macho "$TEST_ROOT/library.a"; then
    fail "Mach-O archive was accepted as a service executable"
fi

printf '%s\n' 'macOS packaging source checks passed'
