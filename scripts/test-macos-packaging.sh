#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
# Literal dollar-sign patterns below inspect package source text.
# shellcheck disable=SC2016

set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
SCRIPTS="$ROOT/packaging/macos-service/scripts"
COMMON="$SCRIPTS/common.sh"
PREINSTALL="$SCRIPTS/preinstall"
POSTINSTALL="$SCRIPTS/postinstall"
UNINSTALL="$SCRIPTS/uninstall-macos-service.sh"
SUPERVISOR="$SCRIPTS/run-hub-service.sh"
PLIST="$ROOT/packaging/macos-service/com.teslatlas.hub.plist.in"
FLEET_TELEMETRY_EXAMPLE="$ROOT/packaging/macos-service/fleet-telemetry.json.example"
CLI_PLIST="$ROOT/packaging/com.teslatlas.hub.plist.in"
APP_BUILD="$ROOT/scripts/build-macos-app.sh"
ICON_BUILD="$ROOT/scripts/build-app-icon.sh"
APPKIT_TEST="$ROOT/scripts/test-macos-appkit.sh"
SERVICE_BUILD="$ROOT/scripts/build-macos-service-package.sh"
PROXY_BUILD="$ROOT/scripts/build-tesla-command-proxy.sh"
FLEET_TELEMETRY_BUILD="$ROOT/scripts/build-fleet-telemetry-bridge.sh"
APP_INFO="$ROOT/macos/TeslatlasHubApp/TeslatlasHubApp/Info.plist"
APP_PROJECT="$ROOT/macos/TeslatlasHubApp/project.yml"
APP_ICON="$ROOT/macos/TeslatlasHubApp/TeslatlasHubApp/Resources/AppIcon.icns"
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

for script in "$SCRIPTS/common.sh" "$PREINSTALL" "$POSTINSTALL" "$UNINSTALL" "$SUPERVISOR"; do
    /bin/sh -n "$script" || fail "invalid shell syntax: $script"
done
/bin/sh -n "$APP_BUILD" || fail "invalid macOS app build script"
/bin/sh -n "$ICON_BUILD" || fail "invalid app icon build script"
/bin/sh -n "$APPKIT_TEST" || fail "invalid AppKit test script"
/bin/sh -n "$SERVICE_BUILD" || fail "invalid macOS service build script"
/bin/sh -n "$PROXY_BUILD" || fail "invalid Tesla command proxy build script"
/bin/sh -n "$FLEET_TELEMETRY_BUILD" || fail "invalid Fleet Telemetry build script"
/usr/bin/plutil -lint "$PLIST" >/dev/null || fail "invalid LaunchAgent template"
/usr/bin/python3 -m json.tool "$FLEET_TELEMETRY_EXAMPLE" >/dev/null \
    || fail "invalid Fleet Telemetry receiver example"
/usr/bin/plutil -lint "$CLI_PLIST" >/dev/null || fail "invalid CLI LaunchAgent template"
[ -f "$APP_ICON" ] && [ ! -L "$APP_ICON" ] \
    || fail "Hub app icon is missing or unsafe"
/usr/bin/file "$APP_ICON" | /usr/bin/grep -Fq 'Mac OS X icon' \
    || fail "Hub app icon is not an ICNS file"
/usr/bin/grep -Fq '"$ICON_BUILD"' "$APP_BUILD" \
    || fail "macOS app build does not regenerate the icon from tracked source"
/usr/libexec/PlistBuddy -c 'Print :CFBundleIconFile' "$APP_INFO" | /usr/bin/grep -qx AppIcon \
    || fail "Hub app Info.plist has no app icon"
/usr/bin/grep -Fq 'CFBundleIconFile: AppIcon' "$APP_PROJECT" \
    || fail "Hub app project does not preserve the app icon"
/usr/bin/grep -q '<integer>63</integer>' "$PLIST" \
    || fail "packaged LaunchAgent does not set a private umask"
/usr/bin/grep -q '<integer>63</integer>' "$CLI_PLIST" \
    || fail "CLI LaunchAgent does not set a private umask"
/usr/bin/grep -Fq '@SUPERVISOR@' "$PLIST" \
    || fail "LaunchAgent does not use the Hub service supervisor"
if /usr/bin/grep -Fq '<string>serve</string>' "$PLIST"; then
    fail "LaunchAgent must not bypass the Fleet receiver supervisor"
fi
/usr/bin/grep -Fq 'require_safe_owned_tree "$directory" "$CONSOLE_UID" "user directory"' "$COMMON" \
    || fail "existing user directories are not safely admitted"

if /usr/bin/grep -q 'bootout_if_loaded' "$PREINSTALL"; then
    fail "preinstall must not stop the running service"
fi
/usr/bin/grep -q 'STATE_DIRECTORY/was-loaded' "$PREINSTALL" \
    || fail "preinstall does not record loaded state"
loaded_service_block=$(
    /usr/bin/awk '/^if service_is_loaded; then$/ { capture=1 } capture { print } capture && /^fi$/ { exit }' "$PREINSTALL"
)
printf '%s\n' "$loaded_service_block" | /usr/bin/grep -Fq 'require_safe_regular_file "$PLIST"' \
    || fail "preinstall does not validate the loaded legacy LaunchAgent"
if printf '%s\n' "$loaded_service_block" | /usr/bin/grep -Fq 'require_safe_regular_file "$BINARY"'; then
    fail "preinstall rejects a loaded legacy per-user Hub without a root binary"
fi
/usr/bin/grep -q 'STATE_DIRECTORY/teslatlas-hub' "$PREINSTALL" \
    || fail "preinstall does not preserve the old binary"
/usr/bin/grep -q 'STATE_DIRECTORY/launch-agent.plist' "$PREINSTALL" \
    || fail "preinstall does not preserve the old LaunchAgent"
/usr/bin/grep -Fq 'refuse_other_user_launch_agents' "$PREINSTALL" \
    || fail "preinstall does not refuse unsafe multi-user shared-binary upgrades"
/usr/bin/grep -Fq 'another user has a Hub LaunchAgent' "$COMMON" \
    || fail "multi-user upgrade refusal has no clear diagnostic"
/usr/bin/grep -Fq '/usr/bin/pgrep -u "$expected_uid" -x "$process_name"' "$PREINSTALL" \
    || fail "preinstall does not scope the running app check to the console user and exact name"
/usr/bin/grep -Fq '/usr/bin/pkill -TERM -u "$expected_uid" -x "$process_name"' "$PREINSTALL" \
    || fail "preinstall does not request a bounded clean app exit before replacement"
/usr/bin/grep -Fq '/usr/bin/pkill -KILL -u "$expected_uid" -x "$process_name"' "$PREINSTALL" \
    || fail "preinstall cannot finish replacing an unresponsive old app"
assert_before_fixed "stop_running_app_for_update 'Teslatlas Hub' \"\$CONSOLE_UID\"" \
    'STATE_DIRECTORY=$(upgrade_state_directory)' "$PREINSTALL"

gui_update_helper="$TEST_ROOT/gui-update-helper.sh"
/usr/bin/sed -n '/^# BEGIN TESTABLE GUI UPDATE HELPER$/,/^# END TESTABLE GUI UPDATE HELPER$/p' \
    "$PREINSTALL" > "$gui_update_helper"
# shellcheck source=/dev/null
. "$gui_update_helper"
test_process_name="tlhup$$"
/usr/bin/perl -e '$0 = shift; sleep 30' "$test_process_name" &
test_process_pid=$!
/bin/sleep 0.1
/usr/bin/pgrep -u "$(/usr/bin/id -u)" -x "$test_process_name" >/dev/null \
    || fail "GUI update helper fixture did not start"
stop_running_app_for_update "$test_process_name" "$(/usr/bin/id -u)" \
    || fail "GUI update helper did not stop the old app process"
if /bin/kill -0 "$test_process_pid" >/dev/null 2>&1; then
    fail "GUI update helper left the old app process running"
fi
wait "$test_process_pid" >/dev/null 2>&1 || true

assert_before_fixed '/usr/bin/plutil -lint "$rendered_plist"' '    stop_loaded_service_bounded' "$POSTINSTALL"
assert_before_fixed '    stop_loaded_service_bounded' '"$BINARY" --config "$CONFIG" preflight' "$POSTINSTALL"
assert_before_fixed 'if [ -f "$STATE_DIRECTORY/was-loaded" ]; then' '    stop_loaded_service_bounded' "$POSTINSTALL"
/usr/bin/grep -Fq '/bin/chmod 0700 "$LOG_DIRECTORY"' "$POSTINSTALL" \
    || fail "Hub log directory is not normalized to mode 0700"
/usr/bin/grep -Fq '/bin/chmod 0600 "$log"' "$POSTINSTALL" \
    || fail "existing Hub logs are not normalized to mode 0600"
/usr/bin/grep -A1 '<key>ThrottleInterval</key>' "$PLIST" \
    | /usr/bin/grep -Fq '<integer>30</integer>' \
    || fail "LaunchAgent restart failures are not throttled"
/usr/bin/grep -Fq '<string>--stdout-log</string>' "$PLIST" \
    || fail "LaunchAgent does not pass stdout log to the supervisor"
/usr/bin/grep -Fq '<string>--stderr-log</string>' "$PLIST" \
    || fail "LaunchAgent does not pass stderr log to the supervisor"
/usr/bin/grep -Fq 'compact_log "$STDOUT_LOG"' "$SUPERVISOR" \
    || fail "service supervisor does not compact stdout before launch"
/usr/bin/grep -Fq 'compact_log "$STDERR_LOG"' "$SUPERVISOR" \
    || fail "service supervisor does not compact stderr before launch"
/usr/bin/grep -Fq 'log_check_seconds=$((log_check_seconds + 1))' "$SUPERVISOR" \
    || fail "service supervisor does not schedule bounded active-log maintenance"
[ "$(/usr/bin/grep -Fc 'compact_log "$STDOUT_LOG"' "$SUPERVISOR")" -eq 2 ] \
    || fail "service supervisor does not compact stdout during long runs"
[ "$(/usr/bin/grep -Fc 'compact_log "$STDERR_LOG"' "$SUPERVISOR")" -eq 2 ] \
    || fail "service supervisor does not compact stderr during long runs"
if /usr/bin/grep -Fq 'exec "$HUB" --config "$CONFIG" serve' "$SUPERVISOR"; then
    fail "legacy service bypasses active log maintenance"
fi
/usr/bin/grep -Fq '[ -n "$child" ] || return 0' "$SUPERVISOR" \
    || fail "legacy service supervision cannot omit the Fleet receiver safely"
/usr/bin/grep -q 'completed.*-ne 1' "$POSTINSTALL" \
    || fail "postinstall has no failure rollback"
/usr/bin/grep -q 'launchctl bootstrap' "$POSTINSTALL" \
    || fail "postinstall cannot restart the prior service"
if /usr/bin/grep -Fq 'launchctl kickstart -k' "$POSTINSTALL"; then
    fail "postinstall kills the RunAtLoad service immediately after bootstrap"
fi
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
/usr/bin/grep -Fq 'update installed, but Hub needs attention and was left stopped' "$POSTINSTALL" \
    || fail "postinstall does not preserve an installed-but-stopped unhealthy update"
/usr/bin/grep -Fq 'if [ -e "$CONFIG" ] || [ -L "$CONFIG" ]' "$POSTINSTALL" \
    || fail "postinstall cannot distinguish missing setup from a failed migration"
/usr/bin/grep -Fq 'migration_handover_install_is_paused' "$POSTINSTALL" \
    || fail "postinstall has no install-stopped TeslaMate migration path"
/usr/bin/grep -Fq 'run_with_deadline "$preflight_timeout_seconds"' "$POSTINSTALL" \
    || fail "postinstall preflight has no wall-clock deadline"
/usr/bin/grep -Fq 'run_with_deadline "$status_timeout_seconds"' "$POSTINSTALL" \
    || fail "postinstall status has no wall-clock deadline"
/usr/bin/grep -Fq 'run_with_deadline "$service_timeout_seconds"' "$POSTINSTALL" \
    || fail "postinstall service actions have no wall-clock deadline"
/usr/bin/grep -Fq 'wait_for_service_unloaded 100 0.1' "$POSTINSTALL" \
    || fail "postinstall does not settle asynchronous launchd removal"
/usr/bin/grep -Fq 'rendered_plist="$STATE_DIRECTORY/launch-agent.rendered"' "$POSTINSTALL" \
    || fail "LaunchAgent is not rendered in root-owned upgrade state"
/usr/bin/grep -Fq '/bin/mv -fh "$temporary_plist" "$PLIST"' "$POSTINSTALL" \
    || fail "LaunchAgent installation can follow a destination symlink"
assert_before_fixed '/usr/sbin/chown "$CONSOLE_UID:$CONSOLE_GID" "$temporary_plist"' '/bin/mv -fh "$temporary_plist" "$PLIST"' "$POSTINSTALL"
/usr/bin/grep -Fq '@SUPERVISOR@' "$POSTINSTALL" \
    || fail "postinstall does not render the Hub service supervisor"
/usr/bin/grep -Fq 'installed Hub service supervisor' "$POSTINSTALL" \
    || fail "postinstall does not validate the Hub service supervisor"
/usr/bin/grep -Fq 'CURRENT_PROJECT_VERSION="$bundle_version"' "$APP_BUILD" \
    || fail "app build does not preserve prerelease build identity"
/usr/bin/grep -Fq 'TESLATLAS_HUB_VERSION="$version"' "$APP_BUILD" \
    || fail "app build does not expose exact Hub version"
/usr/bin/grep -Fq -- '--version "$package_version"' "$SERVICE_BUILD" \
    || fail "service package does not use mapped prerelease identity"
/usr/bin/grep -Fq 'require_hub_version "$binary" "$version"' "$SERVICE_BUILD" \
    || fail "service package does not bind package version to Hub binary"
for release_legal_file in ADDITIONAL_TERMS.md SOURCE_AVAILABILITY.md RELEASE_VERIFICATION.md; do
    /usr/bin/grep -Fq "$release_legal_file" "$APP_BUILD" \
        || fail "app bundle omits release legal file: $release_legal_file"
    /usr/bin/grep -Fq "$release_legal_file" "$SERVICE_BUILD" \
        || fail "service package omits release legal file: $release_legal_file"
done
/usr/bin/grep -Fq 'required_release_legal_file in LICENSE NOTICE THIRD_PARTY_NOTICES.md PROVENANCE.md' \
    "$APP_BUILD" || fail "app build does not require its release legal payload"
/usr/bin/grep -Fq 'required_release_legal_file in LICENSE NOTICE THIRD_PARTY_NOTICES.md PROVENANCE.md' \
    "$SERVICE_BUILD" || fail "service package does not require its release legal payload"
version_helper="$TEST_ROOT/version-helper.sh"
/usr/bin/sed -n \
    '/^# BEGIN TESTABLE HUB VERSION HELPER$/,/^# END TESTABLE HUB VERSION HELPER$/p' \
    "$SERVICE_BUILD" > "$version_helper"
# shellcheck source=/dev/null
. "$version_helper"
matching_binary="$TEST_ROOT/matching-hub"
mismatched_binary="$TEST_ROOT/mismatched-hub"
extra_output_binary="$TEST_ROOT/extra-output-hub"
stderr_binary="$TEST_ROOT/stderr-hub"
/usr/bin/printf '%s\n' '#!/bin/sh' \
    'printf "%s\n" "teslatlas-hub 1.0.0-beta.1"' > "$matching_binary"
/usr/bin/printf '%s\n' '#!/bin/sh' \
    'printf "%s\n" "teslatlas-hub 1.0.0-alpha.2"' > "$mismatched_binary"
/usr/bin/printf '%s\n' '#!/bin/sh' \
    'printf "teslatlas-hub 1.0.0-beta.1\n\n"' > "$extra_output_binary"
/usr/bin/printf '%s\n' '#!/bin/sh' \
    'printf "%s\n" "teslatlas-hub 1.0.0-beta.1"' \
    'printf "%s\n" warning >&2' > "$stderr_binary"
/bin/chmod 0700 "$matching_binary" "$mismatched_binary" \
    "$extra_output_binary" "$stderr_binary"
require_hub_version "$matching_binary" 1.0.0-beta.1 \
    || fail "matching Hub binary version was rejected"
if require_hub_version "$mismatched_binary" 1.0.0-beta.1; then
    fail "mismatched Hub binary version was accepted"
fi
if require_hub_version "$extra_output_binary" 1.0.0-beta.1; then
    fail "extra Hub version output was accepted"
fi
if require_hub_version "$stderr_binary" 1.0.0-beta.1; then
    fail "Hub version stderr was accepted"
fi
/usr/bin/grep -Fq '<key>TeslatlasHubVersion</key>' "$APP_INFO" \
    || fail "app bundle does not carry exact Hub version"
/usr/bin/grep -Fq 'AppIcon.icns' "$APP_BUILD" \
    || fail "app build does not require the app icon resource"
/usr/bin/grep -Fq 'CFBundleIconFile' "$APP_BUILD" \
    || fail "app build does not verify the app icon Info.plist value"
/usr/bin/grep -Fq -- '--proxy-binary "$PROXY_BINARY"' "$APP_BUILD" \
    || fail "app build does not package Tesla command proxy"
/usr/bin/grep -Fq -- '--fleet-telemetry-binary "$FLEET_TELEMETRY_BINARY"' "$APP_BUILD" \
    || fail "app build does not package the Fleet Telemetry receiver"
/usr/bin/grep -Fq 'DIST_PACKAGE="$DIST/TeslatlasHubService.pkg"' "$APP_BUILD" \
    || fail "app build does not produce the requested final package name"
/usr/bin/grep -Fq '"$ROOT/target/macos-app") ;;' "$APP_BUILD" \
    || fail "app build does not scope Xcode staging cleanup"
/usr/bin/grep -Fq '/usr/bin/find "$DERIVED" -depth -delete' "$APP_BUILD" \
    || fail "app build does not clean successful Xcode staging"
assert_before_fixed 'final installer package is missing' '/usr/bin/find "$DERIVED" -depth -delete' "$APP_BUILD"
if /usr/bin/grep -Fq -- '--app "$DIST_APP"' "$APP_BUILD" \
    || /usr/bin/grep -Fq '"$payload/Applications/Teslatlas Hub.app"' "$SERVICE_BUILD"; then
    fail "service-only package still contains the app"
fi
/usr/bin/grep -Fq 'external service package does not match the app' "$APP_BUILD" \
    || fail "app build does not bind external and embedded service packages"
/usr/bin/grep -Fq -- '--legal-bundle "$LEGAL_BUNDLE"' "$APP_BUILD" \
    || fail "app build does not pass the exact dependency legal bundle"
/usr/bin/grep -Fq 'share/dependency-legal' "$SERVICE_BUILD" \
    || fail "service package does not install the exact dependency legal bundle"
/usr/bin/grep -Fq 'RUSTUP=$(PATH="$CALLER_PATH" command -v rustup)' "$SERVICE_BUILD" \
    || fail "service package cannot resolve the pinned Rust toolchain from the caller"
/usr/bin/grep -Fq '"$RUSTUP" which --toolchain "$RUST_TOOLCHAIN" cargo' "$SERVICE_BUILD" \
    || fail "service package does not resolve pinned cargo"
/usr/bin/grep -Fq '"$RUSTUP" which --toolchain "$RUST_TOOLCHAIN" rustc' "$SERVICE_BUILD" \
    || fail "service package does not resolve pinned rustc"
/usr/bin/grep -Fq 'PATH="$RUST_TOOLCHAIN_BIN:/usr/bin:/bin:/usr/sbin:/sbin"' "$SERVICE_BUILD" \
    || fail "service package does not retain a bounded build-tool PATH"
/usr/bin/grep -Fq 'export PATH RUSTC' "$SERVICE_BUILD" \
    || fail "service package does not bind Cargo to its pinned compiler"
/usr/bin/grep -Fq '/usr/bin/open "$APP"' "$POSTINSTALL" \
    || fail "successful package install does not open the app"
/usr/bin/grep -Fq -- 'build-tesla-command-proxy.sh' "$APP_BUILD" \
    || fail "app build does not build pinned Tesla command proxy"
/usr/bin/grep -Fq -- 'build-fleet-telemetry-bridge.sh' "$APP_BUILD" \
    || fail "app build does not build the pinned Fleet Telemetry receiver"
/usr/bin/grep -Fq -- '--target darwin-arm64' "$APP_BUILD" \
    || fail "app build does not build an arm64 Fleet Telemetry receiver"
/usr/bin/grep -Fq -- '--proxy-binary PATH' "$SERVICE_BUILD" \
    || fail "service package has no proxy binary input"
/usr/bin/grep -Fq -- '--fleet-telemetry-binary PATH' "$SERVICE_BUILD" \
    || fail "service package has no Fleet Telemetry binary input"
/usr/bin/grep -Fq 'payload_proxy_binary=' "$SERVICE_BUILD" \
    || fail "service package does not install Tesla command proxy"
/usr/bin/grep -Fq 'payload_fleet_telemetry_binary=' "$SERVICE_BUILD" \
    || fail "service package does not install Fleet Telemetry receiver"
/usr/bin/grep -Fq 'run-hub-service.sh' "$SERVICE_BUILD" \
    || fail "service package does not install Fleet receiver supervision"
/usr/bin/grep -Fq 'tesla-http-proxy' "$PLIST" \
    && fail "proxy must not have a second LaunchAgent"
/usr/bin/grep -Fq '49977a18fd68567501d59e16a6c9e4a8b9348544' "$PROXY_BUILD" \
    || fail "proxy build is not pinned to the reviewed Tesla source"
/usr/bin/grep -Fq 'VERSION=v0.4.1' "$PROXY_BUILD" \
    || fail "proxy build has no reviewed Tesla version"
/usr/bin/grep -Fq "github.com/teslamotors/vehicle-command" "$PROXY_BUILD" \
    || fail "proxy build does not validate the Tesla module"
/usr/bin/grep -Fq 'GOARCH=arm64' "$PROXY_BUILD" \
    || fail "proxy build is not arm64-only"
/usr/bin/grep -Fq 'MACOSX_DEPLOYMENT_TARGET=13.0' "$PROXY_BUILD" \
    || fail "proxy build has no macOS 13 deployment target"
/usr/bin/grep -Fq 'target/upstream-cache' "$FLEET_TELEMETRY_BUILD" \
    || fail "Fleet Telemetry source archive is not cached inside the normal Hub target"
/usr/bin/grep -Fq 'cached upstream archive checksum mismatch' "$FLEET_TELEMETRY_BUILD" \
    || fail "Fleet Telemetry cached source is not revalidated before use"
/usr/bin/grep -Fq 'cache_installing=$(/usr/bin/mktemp' "$FLEET_TELEMETRY_BUILD" \
    || fail "Fleet Telemetry cache publication is not staged atomically"

/usr/bin/grep -Fq 'port": 8443' "$FLEET_TELEMETRY_EXAMPLE" \
    || fail "macOS Fleet Telemetry example must use an unprivileged port"
/usr/bin/grep -Fq 'TESLATLAS_FLEET_TELEMETRY_BEARER_FILE' "$SUPERVISOR" \
    || fail "Fleet receiver supervisor does not pass the ingress bearer privately"
/usr/bin/grep -Fq 'safe_user_file "$RECEIVER_CONFIG"' "$SUPERVISOR" \
    || fail "Fleet receiver supervisor accepts unsafe user config"
/usr/bin/grep -Fq 'safe_root_executable "$RECEIVER"' "$SUPERVISOR" \
    || fail "Fleet receiver supervisor accepts unsafe receiver binary"
/usr/bin/grep -Fq 'safe_root_executable "$PROXY"' "$SUPERVISOR" \
    || fail "Fleet receiver supervisor accepts unsafe command proxy binary"
/usr/bin/grep -Fq 'The Hub starts tesla-http-proxy itself' "$SUPERVISOR" \
    || fail "Fleet receiver supervisor does not retain Hub-owned proxy lifecycle"
if /usr/bin/grep -Fq '"$PROXY" &' "$SUPERVISOR"; then
    fail "Fleet receiver supervisor must not start a duplicate command proxy"
fi
/usr/bin/grep -Fq 'stop_child "$hub_pid"' "$SUPERVISOR" \
    || fail "Fleet receiver supervisor does not stop Hub with its receiver"
/usr/bin/grep -Fq 'stop_child "$receiver_pid"' "$SUPERVISOR" \
    || fail "Fleet receiver supervisor does not stop receiver with Hub"

SUPERVISOR_FUNCTIONS="$TEST_ROOT/supervisor-functions.sh"
/usr/bin/awk 'index($0, "if [ \"$#\" -ne 6 ]") == 1 { exit } { print }' \
    "$SUPERVISOR" >"$SUPERVISOR_FUNCTIONS"
# shellcheck source=/dev/null
. "$SUPERVISOR_FUNCTIONS"
single_token="$TEST_ROOT/fleet bearer # literal"
printf '%s\n' '[collector.fleet_telemetry]' \
    "hostname = 'telemetry.example.invalid'" \
    "ingest_token_path = '$single_token' # trailing comment" \
    >"$TEST_ROOT/single-quoted.toml"
[ "$(fleet_telemetry_bearer "$TEST_ROOT/single-quoted.toml")" = "$single_token" ] \
    || fail "Fleet receiver supervisor rejects a valid single-quoted TOML bearer path"
double_token="$TEST_ROOT/fleet-bearer-double"
printf '%s\n' '[collector.fleet_telemetry]' \
    'hostname = "telemetry.example.invalid"' \
    "ingest_token_path = \"$double_token\"" \
    >"$TEST_ROOT/double-quoted.toml"
[ "$(fleet_telemetry_bearer "$TEST_ROOT/double-quoted.toml")" = "$double_token" ] \
    || fail "Fleet receiver supervisor rejects a valid double-quoted TOML bearer path"

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

migration_helper="$TEST_ROOT/migration-install-helper.sh"
/usr/bin/sed -n '/^# BEGIN TESTABLE MIGRATION INSTALL HELPER$/,/^# END TESTABLE MIGRATION INSTALL HELPER$/p' \
    "$POSTINSTALL" > "$migration_helper"
# shellcheck source=/dev/null
. "$COMMON"
# shellcheck source=/dev/null
. "$migration_helper"

unload_checks=0
service_is_loaded() {
    unload_checks=$((unload_checks + 1))
    [ "$unload_checks" -lt 3 ]
}
wait_for_service_unloaded 3 0 \
    || fail "launchd unload settling rejected a transitional loaded state"
[ "$unload_checks" -eq 3 ] \
    || fail "launchd unload settling did not poll until absence"
service_is_loaded() {
    return 0
}
if wait_for_service_unloaded 3 0; then
    fail "launchd unload settling accepted a persistently loaded service"
fi

migration_home="$TEST_ROOT/migration-home"
migration_config="$migration_home/config.toml"
migration_marker="$migration_home/.teslamate-handover-pending"
/bin/mkdir -m 0700 "$migration_home"
/usr/bin/printf '%s\n' \
    'data_dir = "/tmp/hub"' \
    '' \
    '[collector]' \
    'provider = "legacy"' \
    'interval_seconds = 0' > "$migration_config"
/usr/bin/printf '%s\n' \
    '{"phase":"awaiting_verification","previousIntervalSeconds":60}' > "$migration_marker"
/bin/chmod 0600 "$migration_config" "$migration_marker"
test_uid=$(/usr/bin/id -u)
migration_handover_install_is_paused "$migration_marker" "$migration_config" "$test_uid" \
    || fail "fresh TeslaMate migration was not admitted as install-stopped"
hub_should_start_after_install 1 0 0 1 \
    && fail "fresh TeslaMate migration would start Hub before Swift handover"
hub_should_start_after_install 1 0 0 0 \
    || fail "normal fresh installation no longer starts Hub"
if hub_should_attempt_bootstrap 0 0; then
    fail "missing setup would be mistaken for a schema migration"
fi
if hub_should_attempt_bootstrap 1 1; then
    fail "ready setup would be migrated unnecessarily"
fi
hub_should_attempt_bootstrap 1 0 \
    || fail "configured setup needing migration would not be bootstrapped"
/usr/bin/printf '%s\n' \
    'data_dir = "/tmp/hub"' \
    '' \
    '[collector]' \
    'provider = "legacy"' \
    'interval_seconds = 60' > "$migration_config"
if migration_handover_install_is_paused "$migration_marker" "$migration_config" "$test_uid"; then
    fail "active collector configuration was accepted as install-stopped migration"
fi

macho_helper="$TEST_ROOT/macho-helper.sh"
/usr/bin/sed -n '/^# BEGIN TESTABLE MACH-O HELPER$/,/^# END TESTABLE MACH-O HELPER$/p' \
    "$SERVICE_BUILD" > "$macho_helper"
# shellcheck source=/dev/null
. "$macho_helper"
/usr/bin/printf '%s\n' 'int main(void) { return 0; }' \
    | /usr/bin/clang -arch arm64 -mmacosx-version-min=13.0 -x c - -o "$TEST_ROOT/executable"
/usr/bin/printf '%s\n' 'int sample(void) { return 0; }' \
    | /usr/bin/clang -arch arm64 -mmacosx-version-min=13.0 -dynamiclib -x c - -o "$TEST_ROOT/library.dylib"
/usr/bin/printf '%s\n' 'int sample(void) { return 0; }' \
    | /usr/bin/clang -arch arm64 -mmacosx-version-min=13.0 -c -x c - -o "$TEST_ROOT/member.o"
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
