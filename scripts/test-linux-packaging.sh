#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-hub-package-test.XXXXXX")
trap 'find "$test_root" -depth -delete' EXIT HUP INT TERM
maintainer_bin="$test_root/maintainer-bin"
elf_bin="$test_root/elf-bin"
systemd_dir="$test_root/systemd"
systemctl_log="$test_root/systemctl.log"
hub_status_log="$test_root/hub-status.log"
maintainer_state="$test_root/maintainer.state"
upgrade_backup="$test_root/upgrade-backup"
config_file="$test_root/config.toml"
installed_binary="$test_root/teslatlas-hub"
installed_unit="$test_root/teslatlas-hub.service"
installed_terminal_failure_target="$test_root/teslatlas-hub-terminal-failure.target"
installed_proxy="$test_root/tesla-http-proxy"
installed_fleet="$test_root/fleet-telemetry"
installed_proxy_unit="$test_root/teslatlas-command-proxy.service"
installed_fleet_unit="$test_root/teslatlas-fleet-telemetry.service"
installed_proxy_config="$test_root/command-proxy.env"
installed_fleet_config="$test_root/fleet-telemetry.json"
schema_upgraded="$test_root/schema-upgraded"
captured_control="$test_root/control"
mkdir -p "$maintainer_bin" "$elf_bin" "$systemd_dir"
export TESLATLAS_HUB_MAINTAINER_STATE_FILE="$maintainer_state"
export TESLATLAS_HUB_HEALTH_ATTEMPTS=2
export TESLATLAS_HUB_HEALTH_DELAY_SECONDS=0
export TESLATLAS_HUB_HEALTH_TIMEOUT_SECONDS=1
export TESLATLAS_HUB_RUNUSER="$maintainer_bin/runuser"
export TESLATLAS_HUB_TIMEOUT="$maintainer_bin/timeout"
export TESLATLAS_HUB_UPGRADE_BACKUP_DIR="$upgrade_backup"
export TESLATLAS_HUB_CONFIG="$config_file"
export TESLATLAS_HUB_BINARY="$installed_binary"
export TESLATLAS_HUB_UNIT_FILE="$installed_unit"
export TESLATLAS_HUB_TERMINAL_FAILURE_TARGET_FILE="$installed_terminal_failure_target"
export TESLATLAS_COMMAND_PROXY_BINARY="$installed_proxy"
export TESLATLAS_FLEET_TELEMETRY_BINARY="$installed_fleet"
export TESLATLAS_COMMAND_PROXY_UNIT_FILE="$installed_proxy_unit"
export TESLATLAS_FLEET_TELEMETRY_UNIT_FILE="$installed_fleet_unit"
export TESLATLAS_COMMAND_PROXY_CONFIG="$installed_proxy_config"
export TESLATLAS_FLEET_TELEMETRY_CONFIG="$installed_fleet_config"
export HUB_SCHEMA_UPGRADED_FILE="$schema_upgraded"

grep -Fqx 'StateDirectoryMode=0700' "$root/packaging/linux/teslatlas-hub.service" || {
    echo 'test-linux-packaging: systemd state directory is not private' >&2
    exit 1
}
for unit in \
    "$root/packaging/linux/teslatlas-hub.service" \
    "$root/packaging/linux/teslatlas-command-proxy.service" \
    "$root/packaging/linux/teslatlas-fleet-telemetry.service"; do
    grep -Fqx 'StartLimitIntervalSec=300' "$unit" \
        || { echo "test-linux-packaging: missing service restart interval limit: $unit" >&2; exit 1; }
    grep -Fqx 'StartLimitBurst=5' "$unit" \
        || { echo "test-linux-packaging: missing service restart burst limit: $unit" >&2; exit 1; }
done
for setting in \
    'UMask=0077' \
    'CapabilityBoundingSet=' \
    'NoNewPrivileges=true' \
    'PrivateDevices=true' \
    'PrivateTmp=true' \
    'ProtectControlGroups=true' \
    'ProtectHome=true' \
    'ProtectKernelModules=true' \
    'ProtectKernelTunables=true' \
    'ProtectSystem=strict' \
    'RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6' \
    'RestrictSUIDSGID=true' \
    'SystemCallArchitectures=native'; do
    grep -Fqx "$setting" "$root/packaging/linux/teslatlas-hub.service" \
        || { echo "test-linux-packaging: missing Hub hardening: $setting" >&2; exit 1; }
done
fail() {
    echo "test-linux-packaging: $*" >&2
    if [ -f "$test_root/package-output" ]; then
        sed -n '1,20p' "$test_root/package-output" >&2
    fi
    exit 1
}

require_log() {
    grep -Fqx "$1" "$systemctl_log" || fail "missing systemctl call: $1"
}

test_sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

grep -Fqx 'Before=teslatlas-hub.service' \
    "$root/packaging/linux/teslatlas-command-proxy.service" \
    || fail 'command proxy is not ordered before Hub'
grep -Fq 'ExecStartPost=/bin/bash -ec' \
    "$root/packaging/linux/teslatlas-command-proxy.service" \
    || fail 'command proxy has no bounded listener readiness gate'
grep -Fq '$${TESLA_HTTP_PROXY_HOST}' \
    "$root/packaging/linux/teslatlas-command-proxy.service" \
    || fail 'command proxy readiness does not use configured host'
grep -Fq '$${TESLA_HTTP_PROXY_PORT}' \
    "$root/packaging/linux/teslatlas-command-proxy.service" \
    || fail 'command proxy readiness does not use configured port'
grep -Fqx 'TimeoutStartSec=20' "$root/packaging/linux/teslatlas-command-proxy.service" \
    || fail 'command proxy readiness is not time bounded'
grep -Fqx 'Wants=network-online.target teslatlas-command-proxy.service teslatlas-fleet-telemetry.service' \
    "$root/packaging/linux/teslatlas-hub.service" \
    || fail 'Hub does not pull in configured sidecars'
grep -Fqx 'OnFailure=teslatlas-hub-terminal-failure.target' \
    "$root/packaging/linux/teslatlas-hub.service" \
    || fail 'Hub does not stop companions after restart exhaustion'
grep -Fqx 'Restart=always' "$root/packaging/linux/teslatlas-hub.service" \
    || fail 'Hub does not self-heal after an unexpected clean exit'
grep -Fqx 'RestartMode=direct' "$root/packaging/linux/teslatlas-hub.service" \
    || fail 'Hub transient restarts incorrectly notify companion units'
for sidecar_unit in "$root/packaging/linux/teslatlas-command-proxy.service" \
    "$root/packaging/linux/teslatlas-fleet-telemetry.service"; do
    grep -Fqx 'PartOf=teslatlas-hub.service' "$sidecar_unit" \
        || fail "sidecar does not follow Hub stop/restart: $sidecar_unit"
    if grep -Fqx 'BindsTo=teslatlas-hub.service' "$sidecar_unit"; then
        fail "sidecar cannot run independently during Fleet setup: $sidecar_unit"
    fi
done
grep -Fqx 'Conflicts=teslatlas-command-proxy.service teslatlas-fleet-telemetry.service' \
    "$root/packaging/linux/teslatlas-hub-terminal-failure.target" \
    || fail 'terminal failure target does not stop both companions'
grep -Fqx 'StopWhenUnneeded=yes' \
    "$root/packaging/linux/teslatlas-hub-terminal-failure.target" \
    || fail 'terminal failure target can block later recovery'
grep -Fqx 'RefuseManualStart=yes' \
    "$root/packaging/linux/teslatlas-hub-terminal-failure.target" \
    || fail 'terminal failure target accepts unsafe manual activation'
grep -Fqx 'TESLA_HTTP_PROXY_PORT=4445' "$root/packaging/linux/command-proxy.env" \
    || fail 'packaged command proxy port is not 4445'

for utility in getent adduser chown chmod; do
    printf '%s\n' '#!/bin/sh' 'exit 0' > "$maintainer_bin/$utility"
    chmod 0755 "$maintainer_bin/$utility"
done
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'case " $* " in *" -d "*) exit 0 ;; esac' \
    'previous=' \
    'last=' \
    'for value in "$@"; do previous=$last; last=$value; done' \
    'if [ "$previous" = /dev/null ]; then' \
    '  mkdir -p "$(dirname -- "$last")"' \
    '  : > "$last"' \
    'elif [ -f "$previous" ]; then' \
    '  mkdir -p "$(dirname -- "$last")"' \
    '  cp "$previous" "$last"' \
    'fi' > "$maintainer_bin/install"
chmod 0755 "$maintainer_bin/install"
printf '%s\n' \
    '#!/bin/sh' \
    'shift' \
    'exec "$@"' > "$maintainer_bin/timeout"
chmod 0755 "$maintainer_bin/timeout"
printf '%s\n' \
    '#!/bin/sh' \
    'printf "%s\\n" "$*" >> "$HUB_STATUS_LOG"' \
    'case " $* " in' \
    '  *" bootstrap "*) [ "${HUB_BOOTSTRAP_READY:-1}" = 1 ] || exit 1; : > "$HUB_SCHEMA_UPGRADED_FILE"; exit 0 ;;' \
    '  *" preflight "*)' \
    '    [ "${HUB_PREFLIGHT_READY:-1}" = 1 ] || exit 1' \
    '    if [ "${HUB_SCHEMA_UPGRADE:-0}" = 1 ] && [ ! -f "$HUB_SCHEMA_UPGRADED_FILE" ]; then exit 1; fi' \
    '    exit 0' \
    '    ;;' \
    '  *" doctor "*) [ "${HUB_DOCTOR_READY:-1}" = 1 ]; exit ;;' \
    'esac' \
    'if [ "${HUB_UNCONFIGURED:-0}" = 1 ]; then' \
    '  printf "%s\n" "{\"ready\":false,\"vehicles\":[]}"' \
    '  exit 0' \
    'fi' \
    'if [ "${HUB_READY:-1}" = 1 ]; then' \
    '  printf "%s\\n" "{\"ready\":true}"' \
    'else' \
    '  printf "%s\\n" "{\"ready\":false}"' \
    'fi' > "$maintainer_bin/runuser"
chmod 0755 "$maintainer_bin/runuser"
printf '%s\n' \
    '#!/bin/sh' \
    'printf "%s\\n" "$*" >> "$SYSTEMCTL_LOG"' \
    'service_name=${3:-${2:-}}' \
    'case "$service_name" in' \
    '  teslatlas-hub.service)' \
    '    present=${SYSTEMCTL_HUB_PRESENT:-1}' \
    '    active=${SYSTEMCTL_HUB_ACTIVE:-${SYSTEMCTL_ACTIVE:-0}}' \
    '    enabled=${SYSTEMCTL_HUB_ENABLED:-${SYSTEMCTL_ENABLED:-0}}' \
    '    ;;' \
    '  teslatlas-command-proxy.service)' \
    '    present=${SYSTEMCTL_PROXY_PRESENT:-0}' \
    '    active=${SYSTEMCTL_PROXY_ACTIVE:-0}' \
    '    enabled=${SYSTEMCTL_PROXY_ENABLED:-0}' \
    '    ;;' \
    '  teslatlas-fleet-telemetry.service)' \
    '    present=${SYSTEMCTL_TELEMETRY_PRESENT:-0}' \
    '    active=${SYSTEMCTL_TELEMETRY_ACTIVE:-0}' \
    '    enabled=${SYSTEMCTL_TELEMETRY_ENABLED:-0}' \
    '    ;;' \
    '  *) present=0; active=0; enabled=0 ;;' \
    'esac' \
    'case "$1" in' \
    '  cat) [ "$present" = 1 ] ;;' \
    '  is-active)' \
    '    if [ "${SYSTEMCTL_FAIL_AFTER_START:-0}" = 1 ] && grep -Fq "start teslatlas-hub.service" "$SYSTEMCTL_LOG"; then exit 1; fi' \
    '    [ "$active" = 1 ]' \
    '    ;;' \
    '  is-enabled) [ "$enabled" = 1 ] ;;' \
    '  start) [ "${SYSTEMCTL_FAIL_START_SERVICE:-}" != "$service_name" ] ;;' \
    '  stop) [ "${SYSTEMCTL_FAIL_STOP_SERVICE:-}" != "$service_name" ] ;;' \
    '  *) exit 0 ;;' \
    'esac' > "$maintainer_bin/systemctl"
chmod 0755 "$maintainer_bin/systemctl"

for script in preinst postinst prerm postrm; do
    sed "s|/run/systemd/system|$systemd_dir|g" "$root/packaging/linux/$script" > "$test_root/$script"
    chmod 0755 "$test_root/$script"
done

printf '%s\n' 'data_dir = "/var/lib/teslatlas-hub"' > "$config_file"

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" SYSTEMCTL_ACTIVE=0 \
    sh "$test_root/postinst" configure ''
require_log 'daemon-reload'
if grep -Fqx 'restart teslatlas-hub.service' "$systemctl_log"; then
    fail 'inactive initial install restarted service'
fi
grep -Fqx '[geocoder]' "$config_file" || fail 'minimal config did not disable geocoder explicitly'
grep -Fqx '[terrain]' "$config_file" || fail 'minimal config did not disable terrain explicitly'

printf '%s\n' \
    'data_dir = "/srv/teslatlas-custom"' \
    '[geocoder]' \
    'provider = "offline"' \
    '[terrain]' \
    'enabled = true' > "$config_file"
: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" SYSTEMCTL_ACTIVE=0 \
    sh "$test_root/postinst" configure ''
[ "$(grep -Ec '^[[:space:]]*enabled[[:space:]]*=' "$config_file")" -eq 2 ] \
    || fail 'existing offline sections did not receive exactly one missing default'
awk '
    /^\[geocoder\]$/ { section = "geocoder"; next }
    /^\[terrain\]$/ { section = "terrain"; next }
    /^\[/ { section = "" }
    section == "geocoder" && /^[[:space:]]*enabled[[:space:]]*=[[:space:]]*false/ { geocoder = 1 }
    section == "terrain" && /^[[:space:]]*enabled[[:space:]]*=[[:space:]]*true/ { terrain = 1 }
    END { exit !(geocoder && terrain) }
' "$config_file" || fail 'offline defaults did not preserve explicit settings'

: > "$systemctl_log"
printf '%s\n' old-binary > "$installed_binary"
printf '%s\n' old-unit > "$installed_unit"
printf '%s\n' old-terminal-failure-target > "$installed_terminal_failure_target"
if PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 \
    SYSTEMCTL_PROXY_PRESENT=1 SYSTEMCTL_PROXY_ACTIVE=1 SYSTEMCTL_PROXY_ENABLED=1 \
    SYSTEMCTL_TELEMETRY_PRESENT=1 SYSTEMCTL_TELEMETRY_ACTIVE=1 \
    SYSTEMCTL_TELEMETRY_ENABLED=1 \
    SYSTEMCTL_FAIL_STOP_SERVICE=teslatlas-command-proxy.service \
    sh "$test_root/preinst" upgrade 1.0.0; then
    fail 'failed service stop was accepted during upgrade preparation'
fi
require_log 'start teslatlas-command-proxy.service'
require_log 'start teslatlas-hub.service'
require_log 'start teslatlas-fleet-telemetry.service'
[ ! -e "$maintainer_state" ] || fail 'recovered preinst failure retained service state'
[ ! -e "$upgrade_backup" ] || fail 'recovered preinst failure retained payload backup'

: > "$systemctl_log"
: > "$hub_status_log"
printf '%s\n' old-binary > "$installed_binary"
printf '%s\n' old-unit > "$installed_unit"
printf '%s\n' old-terminal-failure-target > "$installed_terminal_failure_target"
printf '%s\n' old-proxy > "$installed_proxy"
printf '%s\n' old-fleet > "$installed_fleet"
printf '%s\n' old-proxy-unit > "$installed_proxy_unit"
printf '%s\n' old-fleet-unit > "$installed_fleet_unit"
printf '%s\n' old-proxy-config > "$installed_proxy_config"
printf '%s\n' old-fleet-config > "$installed_fleet_config"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 \
    SYSTEMCTL_PROXY_PRESENT=1 SYSTEMCTL_PROXY_ACTIVE=1 SYSTEMCTL_PROXY_ENABLED=1 \
    SYSTEMCTL_TELEMETRY_PRESENT=1 SYSTEMCTL_TELEMETRY_ACTIVE=1 \
    SYSTEMCTL_TELEMETRY_ENABLED=1 \
    sh "$test_root/preinst" upgrade 1.0.0
[ "$(cat "$maintainer_state")" = '1 1 1 1 1 1 1 1 1' ] \
    || fail 'upgrade did not preserve Hub and sidecar state'
require_log 'stop teslatlas-fleet-telemetry.service'
require_log 'stop teslatlas-hub.service'
require_log 'stop teslatlas-command-proxy.service'
printf '%s\n' new-binary > "$installed_binary"
printf '%s\n' new-unit > "$installed_unit"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" HUB_STATUS_LOG="$hub_status_log" \
    SYSTEMCTL_ACTIVE=1 HUB_READY=1 \
    SYSTEMCTL_PROXY_PRESENT=1 SYSTEMCTL_PROXY_ACTIVE=1 \
    SYSTEMCTL_TELEMETRY_PRESENT=1 SYSTEMCTL_TELEMETRY_ACTIVE=1 \
    sh "$test_root/postinst" configure 1.0.0
require_log 'daemon-reload'
require_log 'start teslatlas-command-proxy.service'
require_log 'start teslatlas-hub.service'
require_log 'start teslatlas-fleet-telemetry.service'
grep -Fqx -- "-u teslatlas -- $installed_binary --config $config_file status" \
    "$hub_status_log" || fail 'upgrade did not query Hub readiness as the service user'
[ ! -e "$upgrade_backup" ] || fail 'successful upgrade retained old payload'

: > "$systemctl_log"
: > "$hub_status_log"
printf '%s\n' old-binary > "$installed_binary"
printf '%s\n' old-unit > "$installed_unit"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 \
    sh "$test_root/preinst" upgrade 1.0.0
printf '%s\n' same-schema-unhealthy > "$installed_binary"
printf '%s\n' same-schema-unit > "$installed_unit"
printf '%s\n' new-terminal-failure-target > "$installed_terminal_failure_target"
printf '%s\n' new-proxy > "$installed_proxy"
printf '%s\n' new-fleet > "$installed_fleet"
printf '%s\n' new-proxy-unit > "$installed_proxy_unit"
printf '%s\n' new-fleet-unit > "$installed_fleet_unit"
printf '%s\n' new-proxy-config > "$installed_proxy_config"
printf '%s\n' new-fleet-config > "$installed_fleet_config"
if PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" HUB_STATUS_LOG="$hub_status_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 HUB_READY=0 \
    sh "$test_root/postinst" configure 1.0.0; then
    fail 'same-schema unhealthy Hub was accepted'
fi
grep -Fqx old-binary "$installed_binary" \
    || fail 'same-schema health failure did not restore old binary'
grep -Fqx old-terminal-failure-target "$installed_terminal_failure_target" \
    || fail 'same-schema health failure did not restore terminal failure target'
for restored_pair in \
    "$installed_proxy:old-proxy" \
    "$installed_fleet:old-fleet" \
    "$installed_proxy_unit:old-proxy-unit" \
    "$installed_fleet_unit:old-fleet-unit" \
    "$installed_proxy_config:old-proxy-config" \
    "$installed_fleet_config:old-fleet-config"; do
    restored_path=${restored_pair%%:*}
    restored_value=${restored_pair#*:}
    grep -Fqx "$restored_value" "$restored_path" \
        || fail "same-schema health failure did not restore $restored_path"
done

: > "$systemctl_log"
: > "$hub_status_log"
rm -f "$installed_proxy" "$installed_fleet" \
    "$installed_proxy_unit" "$installed_fleet_unit" \
    "$installed_proxy_config" "$installed_fleet_config" \
    "$installed_terminal_failure_target"
printf '%s\n' old-binary > "$installed_binary"
printf '%s\n' old-unit > "$installed_unit"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 \
    sh "$test_root/preinst" upgrade 1.0.0
printf '%s\n' new-binary > "$installed_binary"
printf '%s\n' new-unit > "$installed_unit"
printf '%s\n' newly-unpacked > "$installed_terminal_failure_target"
for new_sidecar_path in \
    "$installed_proxy" "$installed_fleet" \
    "$installed_proxy_unit" "$installed_fleet_unit" \
    "$installed_proxy_config" "$installed_fleet_config"; do
    printf '%s\n' newly-unpacked > "$new_sidecar_path"
done
if PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" HUB_STATUS_LOG="$hub_status_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 HUB_READY=0 \
    sh "$test_root/postinst" configure 1.0.0; then
    fail 'same-schema unhealthy Hub with new sidecars was accepted'
fi
for absent_sidecar_path in \
    "$installed_proxy" "$installed_fleet" \
    "$installed_proxy_unit" "$installed_fleet_unit" \
    "$installed_proxy_config" "$installed_fleet_config" \
    "$installed_terminal_failure_target"; do
    [ ! -e "$absent_sidecar_path" ] \
        || fail "rollback retained newly introduced sidecar: $absent_sidecar_path"
done

: > "$systemctl_log"
: > "$hub_status_log"
printf '%s\n' old-binary > "$installed_binary"
printf '%s\n' old-unit > "$installed_unit"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 \
    sh "$test_root/preinst" upgrade 1.0.0
printf '%s\n' migrated-binary > "$installed_binary"
printf '%s\n' migrated-unit > "$installed_unit"
rm -f "$schema_upgraded"
printf '%s\n' \
    'data_dir = "/srv/teslatlas-custom"' \
    '[geocoder]' \
    'enabled = false' \
    '[terrain]' \
    'enabled = false' > "$config_file"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" HUB_STATUS_LOG="$hub_status_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 HUB_SCHEMA_UPGRADE=1 HUB_READY=1 \
    sh "$test_root/postinst" configure 1.0.0
grep -Fq ' bootstrap' "$hub_status_log" || fail 'schema-changing upgrade did not run bounded bootstrap'
grep -Fqx -- "-u teslatlas -- $installed_binary --config $config_file bootstrap" \
    "$hub_status_log" || fail 'custom data_dir upgrade did not cross the bootstrap boundary'
grep -Fqx migrated-binary "$installed_binary" || fail 'successful schema upgrade restored old binary'
[ ! -e "$upgrade_backup" ] || fail 'successful schema upgrade retained old payload'

: > "$systemctl_log"
: > "$hub_status_log"
printf '%s\n' old-binary > "$installed_binary"
printf '%s\n' old-unit > "$installed_unit"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=0 SYSTEMCTL_ENABLED=0 \
    sh "$test_root/preinst" upgrade 1.0.0
printf '%s\n' unconfigured-binary > "$installed_binary"
printf '%s\n' unconfigured-unit > "$installed_unit"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" HUB_STATUS_LOG="$hub_status_log" \
    SYSTEMCTL_ACTIVE=0 SYSTEMCTL_ENABLED=0 HUB_PREFLIGHT_READY=0 \
    HUB_UNCONFIGURED=1 HUB_DOCTOR_READY=1 \
    sh "$test_root/postinst" configure 1.0.0
grep -Fq ' bootstrap' "$hub_status_log" \
    || fail 'unconfigured upgrade did not run bounded bootstrap'
grep -Fq ' doctor' "$hub_status_log" \
    || fail 'unconfigured upgrade did not validate the catalogue'
grep -Fqx unconfigured-binary "$installed_binary" \
    || fail 'unconfigured upgrade restored the old binary'
[ ! -e "$upgrade_backup" ] || fail 'unconfigured upgrade retained old payload'
require_log 'stop teslatlas-hub.service'

: > "$systemctl_log"
: > "$hub_status_log"
printf '%s\n' old-binary > "$installed_binary"
printf '%s\n' old-unit > "$installed_unit"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 \
    sh "$test_root/preinst" upgrade 1.0.0
printf '%s\n' unconfigured-active-binary > "$installed_binary"
printf '%s\n' unconfigured-active-unit > "$installed_unit"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" HUB_STATUS_LOG="$hub_status_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 HUB_PREFLIGHT_READY=0 \
    HUB_UNCONFIGURED=1 HUB_DOCTOR_READY=1 \
    sh "$test_root/postinst" configure 1.0.0
require_log 'start teslatlas-hub.service'
grep -Fqx unconfigured-active-binary "$installed_binary" \
    || fail 'active unconfigured upgrade restored the old binary'
[ ! -e "$upgrade_backup" ] || fail 'active unconfigured upgrade retained old payload'

: > "$systemctl_log"
: > "$hub_status_log"
printf '%s\n' old-binary > "$installed_binary"
printf '%s\n' old-unit > "$installed_unit"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 \
    sh "$test_root/preinst" upgrade 1.0.0
printf '%s\n' migration-failed-binary > "$installed_binary"
printf '%s\n' migration-failed-unit > "$installed_unit"
if PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" HUB_STATUS_LOG="$hub_status_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 HUB_PREFLIGHT_READY=0 HUB_BOOTSTRAP_READY=0 \
    sh "$test_root/postinst" configure 1.0.0; then
    fail 'failed schema migration was accepted'
fi
grep -Fqx migration-failed-binary "$installed_binary" \
    || fail 'failed schema migration restored incompatible old binary'
[ ! -e "$upgrade_backup" ] || fail 'failed schema migration retained old payload backup'
require_log 'stop teslatlas-hub.service'

: > "$systemctl_log"
mkdir -p "$upgrade_backup"
printf '%s\n' old-binary > "$upgrade_backup/teslatlas-hub"
printf '%s\n' old-unit > "$upgrade_backup/teslatlas-hub.service"
: > "$upgrade_backup/forward-only"
printf '%s\n' '1 1' > "$maintainer_state"
printf '%s\n' forward-only-new > "$installed_binary"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    sh "$test_root/postrm" failed-upgrade
grep -Fqx forward-only-new "$installed_binary" \
    || fail 'dpkg failed-upgrade restored old payload after migration boundary'
[ ! -e "$upgrade_backup" ] || fail 'dpkg failed-upgrade retained forward-only backup'
[ ! -e "$maintainer_state" ] || fail 'dpkg failed-upgrade retained forward-only state'
require_log 'stop teslatlas-hub.service'

: > "$systemctl_log"
: > "$hub_status_log"
printf '%s\n' old-binary > "$installed_binary"
printf '%s\n' old-unit > "$installed_unit"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 \
    sh "$test_root/preinst" upgrade 1.0.0
printf '%s\n' unhealthy-binary > "$installed_binary"
rm -f "$schema_upgraded"
if PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" HUB_STATUS_LOG="$hub_status_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 HUB_SCHEMA_UPGRADE=1 HUB_READY=0 \
    sh "$test_root/postinst" configure 1.0.0; then
    fail 'active but unready Hub was accepted'
fi
grep -Fqx unhealthy-binary "$installed_binary" \
    || fail 'post-migration health failure restored incompatible old binary'
[ ! -e "$upgrade_backup" ] || fail 'post-migration health failure retained old payload backup'

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 \
    SYSTEMCTL_PROXY_PRESENT=1 SYSTEMCTL_PROXY_ACTIVE=1 SYSTEMCTL_PROXY_ENABLED=1 \
    SYSTEMCTL_TELEMETRY_PRESENT=1 SYSTEMCTL_TELEMETRY_ACTIVE=1 \
    SYSTEMCTL_TELEMETRY_ENABLED=1 \
    sh "$test_root/prerm" remove
require_log 'is-active --quiet teslatlas-hub.service'
require_log 'is-enabled --quiet teslatlas-hub.service'
require_log 'is-active --quiet teslatlas-command-proxy.service'
require_log 'is-enabled --quiet teslatlas-command-proxy.service'
require_log 'is-active --quiet teslatlas-fleet-telemetry.service'
require_log 'is-enabled --quiet teslatlas-fleet-telemetry.service'
require_log 'stop teslatlas-fleet-telemetry.service'
require_log 'disable teslatlas-fleet-telemetry.service'
require_log 'stop teslatlas-hub.service'
require_log 'disable teslatlas-hub.service'
require_log 'stop teslatlas-command-proxy.service'
require_log 'disable teslatlas-command-proxy.service'
[ "$(cat "$maintainer_state")" = '1 1 1 1 1 1 1 1 1' ] \
    || fail 'remove did not preserve active/enabled Hub and sidecar state'

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=1 \
    SYSTEMCTL_PROXY_PRESENT=1 SYSTEMCTL_PROXY_ACTIVE=1 \
    SYSTEMCTL_TELEMETRY_PRESENT=1 SYSTEMCTL_TELEMETRY_ACTIVE=1 \
    sh "$test_root/postinst" abort-remove
require_log 'daemon-reload'
require_log 'enable teslatlas-hub.service'
require_log 'enable teslatlas-command-proxy.service'
require_log 'enable teslatlas-fleet-telemetry.service'
require_log 'start teslatlas-command-proxy.service'
require_log 'start teslatlas-hub.service'
require_log 'start teslatlas-fleet-telemetry.service'
[ ! -e "$maintainer_state" ] || fail 'successful abort-remove retained rollback state'

printf '%s\n' '1 1 1 1 1 1 1 1 1' > "$maintainer_state"
: > "$systemctl_log"
if PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_PROXY_PRESENT=1 SYSTEMCTL_PROXY_ACTIVE=0 \
    SYSTEMCTL_TELEMETRY_PRESENT=1 SYSTEMCTL_TELEMETRY_ACTIVE=1 \
    sh "$test_root/postinst" abort-remove; then
    fail 'inactive command proxy passed bounded sidecar restore health checks'
fi
[ "$(grep -Fc 'is-active --quiet teslatlas-command-proxy.service' "$systemctl_log")" -eq 2 ] \
    || fail 'command proxy restore health check was not bounded'
[ -f "$maintainer_state" ] || fail 'failed sidecar restore discarded rollback state'
rm -f "$maintainer_state"

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=0 SYSTEMCTL_ENABLED=1 \
    sh "$test_root/prerm" deconfigure
[ "$(cat "$maintainer_state")" = '1 0 1 0 0 0 0 0 0' ] \
    || fail 'deconfigure did not preserve inactive/enabled state'

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    sh "$test_root/postinst" abort-deconfigure
require_log 'enable teslatlas-hub.service'
require_log 'stop teslatlas-hub.service'
[ ! -e "$maintainer_state" ] || fail 'successful abort-deconfigure retained rollback state'

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=0 \
    sh "$test_root/prerm" remove
[ "$(cat "$maintainer_state")" = '1 1 0 0 0 0 0 0 0' ] \
    || fail 'remove did not preserve active/disabled state'

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    SYSTEMCTL_ACTIVE=1 SYSTEMCTL_ENABLED=0 \
    sh "$test_root/postrm" abort-upgrade
require_log 'disable teslatlas-hub.service'
require_log 'start teslatlas-hub.service'
[ ! -e "$maintainer_state" ] || fail 'successful abort-upgrade retained rollback state'

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    sh "$test_root/postinst" abort-upgrade
require_log 'daemon-reload'

for action in abort-install abort-upgrade failed-upgrade; do
    : > "$systemctl_log"
    PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
        sh "$test_root/postrm" "$action"
    require_log 'daemon-reload'
done

: > "$systemctl_log"
PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
    sh "$test_root/prerm" upgrade
[ ! -s "$systemctl_log" ] || fail 'upgrade prerm changed service state'

for action in remove purge; do
    printf '%s\n' '1 1' > "$maintainer_state"
    : > "$systemctl_log"
    PATH="$maintainer_bin:$PATH" SYSTEMCTL_LOG="$systemctl_log" \
        sh "$test_root/postrm" "$action"
    require_log 'daemon-reload'
    require_log 'stop teslatlas-fleet-telemetry.service'
    require_log 'stop teslatlas-hub.service'
    require_log 'stop teslatlas-command-proxy.service'
    if grep -Fqx 'disable teslatlas-hub.service' "$systemctl_log"; then
        fail "$action tried to disable an already removed unit"
    fi
    [ ! -e "$maintainer_state" ] || fail "$action retained rollback state"
done

printf '%s\n' \
    '#!/bin/sh' \
    'path=${2-}' \
    'mode=${FAKE_ELF_MODE:-good_amd64}' \
    'case "${path##*/}" in' \
    '  fake-command-proxy) mode=${FAKE_PROXY_ELF_MODE:-$mode} ;;' \
    '  fake-fleet-telemetry) mode=${FAKE_FLEET_ELF_MODE:-$mode} ;;' \
    'esac' \
    'case "$1" in' \
    '  -h)' \
    '    case "$mode" in' \
    "      good_amd64|good_amd64_static|bad_endian|bad_abi|bad_interpreter|dynamic_without_interpreter|relocatable|shared_object) machine='Advanced Micro Devices X86-64' ;;" \
    "      good_arm64|good_arm64_static) machine='AArch64' ;;" \
    '    esac' \
    '    data="2'"'"'s complement, little endian"' \
    '    [ "$mode" = bad_endian ] && data="2'"'"'s complement, big endian"' \
    '    abi="UNIX - System V"' \
    '    [ "$mode" = bad_abi ] && abi="UNIX - Solaris"' \
    '    type="EXEC (Executable file)"' \
    '    case "$mode" in good_amd64|good_arm64) type="DYN (Position-Independent Executable file)" ;; esac' \
    '    [ "$mode" = relocatable ] && type="REL (Relocatable file)"' \
    '    [ "$mode" = shared_object ] && type="DYN (Shared object file)"' \
    '    printf "  Class: ELF64\\n  Data: %s\\n  OS/ABI: %s\\n  Type: %s\\n  Machine: %s\\n" "$data" "$abi" "$type" "$machine"' \
    '    ;;' \
    '  -l)' \
    '    case "$mode" in' \
    '      good_amd64) printf "Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]\\n" ;;' \
    '      good_arm64) printf "Requesting program interpreter: /lib/ld-linux-aarch64.so.1]\\n" ;;' \
    '      bad_endian|bad_abi) printf "Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]\\n" ;;' \
    '      bad_interpreter) printf "Requesting program interpreter: /bad/loader]\\n" ;;' \
    '    esac' \
    '    ;;' \
    '  -d)' \
    '    [ "$mode" = dynamic_without_interpreter ] && printf " 0x0000000000000001 (NEEDED) Shared library: [libc.so.6]\\n"' \
    '    ;;' \
    'esac' > "$elf_bin/readelf"
chmod 0755 "$elf_bin/readelf"
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'while [ "$#" -gt 2 ]; do' \
    '  case "$1" in' \
    '    -D) shift ;;' \
    '    -m) shift 2 ;;' \
    '    *) exit 64 ;;' \
    '  esac' \
    'done' \
    'mkdir -p "$(dirname -- "$2")"' \
    'cp "$1" "$2"' > "$elf_bin/install"
chmod 0755 "$elf_bin/install"
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    '[ "$1" = -O ] || { echo "missing -O" >&2; exit 1; }' \
    'shift' \
    '[ -f debian/control ] || { echo "missing debian/control" >&2; exit 1; }' \
    'count=0' \
    'while [ "$#" -gt 0 ]; do' \
    '  [ "$1" = -e ] && [ "$#" -ge 2 ] || { echo "invalid -e arguments" >&2; exit 1; }' \
    '  [ -f "$2" ] || { echo "missing ELF: $2" >&2; exit 1; }' \
    '  count=$((count + 1))' \
    '  shift 2' \
    'done' \
    'printf "%s\\n" "$count" > "${0%/*}/../shlibdeps-count"' \
    'case "${FAKE_SHLIBDEPS_MODE:-good}" in' \
    '  good) printf "%s\\n" "shlibs:Depends=libc6 (>= 2.38), libgcc-s1 (>= 3.0)" ;;' \
    '  static) printf "%s\\n" "shlibs:Depends=" ;;' \
    '  missing) printf "%s\\n" "misc:Depends=ignored" ;;' \
    '  duplicate) printf "%s\\n" "shlibs:Depends=libc6" "shlibs:Depends=libgcc-s1" ;;' \
    '  fail) exit 1 ;;' \
    '  *) exit 64 ;;' \
    'esac' > "$elf_bin/dpkg-shlibdeps"
chmod 0755 "$elf_bin/dpkg-shlibdeps"
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    '[ "$1" = --root-owner-group ] && shift' \
    '[ "$1" = --build ] && shift' \
    '[ -f "$1/DEBIAN/preinst" ]' \
    '[ -f "$1/DEBIAN/postrm" ]' \
    'cmp "$EXPECTED_TERMINAL_FAILURE_TARGET" "$1/lib/systemd/system/teslatlas-hub-terminal-failure.target"' \
    'cmp "$EXPECTED_LICENSE" "$1/usr/share/doc/teslatlas-hub/copyright"' \
    'cmp "$EXPECTED_NOTICE" "$1/usr/share/doc/teslatlas-hub/NOTICE"' \
    'cmp "$EXPECTED_THIRD_PARTY_NOTICES" "$1/usr/share/doc/teslatlas-hub/THIRD_PARTY_NOTICES.md"' \
    'cmp "$EXPECTED_PROVENANCE" "$1/usr/share/doc/teslatlas-hub/PROVENANCE.md"' \
    'cmp "$EXPECTED_ADDITIONAL_TERMS" "$1/usr/share/doc/teslatlas-hub/ADDITIONAL_TERMS.md"' \
    'cmp "$EXPECTED_SOURCE_AVAILABILITY" "$1/usr/share/doc/teslatlas-hub/SOURCE_AVAILABILITY.md"' \
    'cmp "$EXPECTED_RELEASE_VERIFICATION" "$1/usr/share/doc/teslatlas-hub/RELEASE_VERIFICATION.md"' \
    'for legal_component in "$EXPECTED_LEGAL_BUNDLE"/*; do' \
    '  cmp "$legal_component" "$1/usr/share/doc/teslatlas-hub/dependency-legal/$(basename "$legal_component")"' \
    'done' \
    'if [ "${EXPECT_SIDECARS:-0}" = 1 ]; then' \
    '  cmp "$EXPECTED_PROXY" "$1/usr/lib/teslatlas-hub/tesla-http-proxy"' \
    '  cmp "$EXPECTED_FLEET" "$1/usr/lib/teslatlas-hub/fleet-telemetry"' \
    '  grep -Fqx "$EXPECTED_PROXY_SHA  tesla-http-proxy" "$1/usr/share/doc/teslatlas-hub/SIDECAR_SHA256SUMS"' \
    '  grep -Fqx "$EXPECTED_FLEET_SHA  fleet-telemetry" "$1/usr/share/doc/teslatlas-hub/SIDECAR_SHA256SUMS"' \
    '  cmp "$EXPECTED_SIDECAR_LOCK" "$1/usr/share/doc/teslatlas-hub/SIDECAR_BUILD_LOCK"' \
    '  grep -Fqx "ExecStart=/usr/lib/teslatlas-hub/tesla-http-proxy" "$1/lib/systemd/system/teslatlas-command-proxy.service"' \
    '  grep -Fqx "ExecStart=/usr/lib/teslatlas-hub/fleet-telemetry -config=/etc/teslatlas-hub/fleet-telemetry.json" "$1/lib/systemd/system/teslatlas-fleet-telemetry.service"' \
    '  grep -Fqx "CapabilityBoundingSet=CAP_NET_BIND_SERVICE" "$1/lib/systemd/system/teslatlas-fleet-telemetry.service"' \
    '  grep -Fqx "NoNewPrivileges=true" "$1/lib/systemd/system/teslatlas-command-proxy.service"' \
    '  grep -Fqx "NoNewPrivileges=true" "$1/lib/systemd/system/teslatlas-fleet-telemetry.service"' \
    '  grep -Fqx "/etc/teslatlas-hub/command-proxy.env" "$1/DEBIAN/conffiles"' \
    '  grep -Fqx "/etc/teslatlas-hub/fleet-telemetry.json" "$1/DEBIAN/conffiles"' \
    '  grep -Fq '"'"'"V": ["teslatlas"]'"'"' "$1/etc/teslatlas-hub/fleet-telemetry.json"' \
    '  grep -Fq '"'"'"connectivity": ["teslatlas"]'"'"' "$1/etc/teslatlas-hub/fleet-telemetry.json"' \
    '  grep -Fq '"'"'"V": "teslatlas"'"'"' "$1/etc/teslatlas-hub/fleet-telemetry.json"' \
    '  ! grep -Eq '"'"'"(alerts|errors)"[[:space:]]*:'"'"' "$1/etc/teslatlas-hub/fleet-telemetry.json"' \
    '  [ ! -e "$1/etc/systemd/system/multi-user.target.wants/teslatlas-command-proxy.service" ]' \
    '  [ ! -e "$1/etc/systemd/system/multi-user.target.wants/teslatlas-fleet-telemetry.service" ]' \
    'else' \
    '  [ ! -e "$1/usr/lib/teslatlas-hub/tesla-http-proxy" ]' \
    '  [ ! -e "$1/usr/lib/teslatlas-hub/fleet-telemetry" ]' \
    'fi' \
    'cp "$1/DEBIAN/control" "$CAPTURED_CONTROL"' \
    ': > "$2"' > "$elf_bin/dpkg-deb"
chmod 0755 "$elf_bin/dpkg-deb"
printf '%s\n' \
    '#!/bin/sh' \
    'printf "%s\n" "teslatlas-hub ${FAKE_HUB_VERSION:-1.0.0}"' \
    '[ "${FAKE_HUB_EXTRA_NEWLINE:-0}" != 1 ] || printf "\n"' \
    '[ "${FAKE_HUB_STDERR:-0}" != 1 ] || printf "%s\n" warning >&2' \
    > "$test_root/fake-binary"
chmod 0755 "$test_root/fake-binary"
: > "$test_root/fake-command-proxy"
: > "$test_root/fake-fleet-telemetry"
package_fixture="$test_root/package-fixture"
mkdir -p "$package_fixture/scripts" "$package_fixture/packaging"
cp "$root/scripts/build-deb.sh" "$package_fixture/scripts/build-deb.sh"
cat >"$package_fixture/scripts/legal-bundle.py" <<'PY'
#!/usr/bin/env python3
import argparse
from pathlib import Path
import sys

parser = argparse.ArgumentParser()
parser.add_argument("--repo")
parser.add_argument("--verify-dir", type=Path, required=True)
parser.add_argument("--go-proxy-evidence")
parser.add_argument("--fleet-telemetry-evidence")
args = parser.parse_args()
sidecars = (args.go_proxy_evidence is not None, args.fleet_telemetry_evidence is not None)
if sidecars[0] != sidecars[1] or not args.verify_dir.is_dir() or args.verify_dir.is_symlink():
    raise SystemExit(1)
base = {"RUST_THIRD_PARTY_NOTICES.generated.md", "rust-dependency-inventory.json",
        "rust-sbom.spdx.json", "legal-bundle-manifest.json"}
extra = {"FLEET_TELEMETRY_THIRD_PARTY_NOTICES.generated.md",
         "GO_THIRD_PARTY_NOTICES.generated.md", "fleet-telemetry-bridge-lock.json",
         "fleet-telemetry-dependency-inventory.json", "fleet-telemetry-legal-lock.json",
         "fleet-telemetry-license-material.tar.gz", "fleet-telemetry-sbom.spdx.json",
         "go-dependency-inventory.json", "go-sbom.spdx.json"}
expected = base | (extra if sidecars[0] else set())
if {path.name for path in args.verify_dir.iterdir()} != expected:
    raise SystemExit(1)
if any(not path.is_file() or path.is_symlink() for path in args.verify_dir.iterdir()):
    raise SystemExit(1)
PY
chmod 0755 "$package_fixture/scripts/legal-bundle.py"
cp -R "$root/packaging/linux" "$package_fixture/packaging/linux"
mkdir -p "$package_fixture/docs/legal" "$package_fixture/docs/releases"
for package_file in LICENSE NOTICE docs/legal/third-party-notices.md \
    docs/legal/provenance.md docs/legal/additional-terms.md \
    docs/legal/source-availability.md docs/releases/verification.md; do
    cp "$root/$package_file" "$package_fixture/$package_file"
done
package_proxy_sha=$(test_sha256 "$test_root/fake-command-proxy")
package_fleet_sha=$(test_sha256 "$test_root/fake-fleet-telemetry")
printf '%s\n' \
    '# teslatlas-linux-sidecars/v1' \
    '# architecture tesla-http-proxy-sha256 fleet-telemetry-sha256' \
    "amd64 $package_proxy_sha $package_fleet_sha" \
    "arm64 $package_proxy_sha $package_fleet_sha" \
    > "$package_fixture/packaging/linux/sidecar-sha256.lock"
package_builder="$package_fixture/scripts/build-deb.sh"
legal_bundle_base="$test_root/legal-bundle-base"
legal_bundle_sidecar="$test_root/legal-bundle-sidecar"
mkdir "$legal_bundle_base" "$legal_bundle_sidecar" \
    "$test_root/go-evidence" "$test_root/fleet-evidence"
for legal_component in RUST_THIRD_PARTY_NOTICES.generated.md \
    rust-dependency-inventory.json rust-sbom.spdx.json legal-bundle-manifest.json; do
    printf '%s\n' "$legal_component fixture" >"$legal_bundle_base/$legal_component"
    cp "$legal_bundle_base/$legal_component" "$legal_bundle_sidecar/$legal_component"
done
for legal_component in FLEET_TELEMETRY_THIRD_PARTY_NOTICES.generated.md \
    GO_THIRD_PARTY_NOTICES.generated.md fleet-telemetry-bridge-lock.json \
    fleet-telemetry-dependency-inventory.json fleet-telemetry-legal-lock.json \
    fleet-telemetry-license-material.tar.gz fleet-telemetry-sbom.spdx.json \
    go-dependency-inventory.json go-sbom.spdx.json; do
    printf '%s\n' "$legal_component fixture" >"$legal_bundle_sidecar/$legal_component"
done

run_package() {
    (
        cd "$test_root"
        package_elf_mode=$1
        package_architecture=$2
        package_shlibdeps_mode=${3:-good}
        package_version=${4:-1.0.0}
        package_sidecars=${5:-0}
        package_proxy_mode=${6:-$package_elf_mode}
        package_fleet_mode=${7:-$package_elf_mode}
        set -- --binary fake-binary --version "$package_version" \
            --architecture "$package_architecture" --output "$test_root/output.deb" \
            --legal-bundle "$legal_bundle_base"
        if [ "$package_sidecars" = 1 ]; then
            set -- "$@" --command-proxy-binary fake-command-proxy \
                --fleet-telemetry-binary fake-fleet-telemetry \
                --legal-bundle "$legal_bundle_sidecar" \
                --go-proxy-evidence "$test_root/go-evidence" \
                --fleet-telemetry-evidence "$test_root/fleet-evidence"
        fi
        FAKE_HUB_VERSION=$package_version \
            FAKE_ELF_MODE=$package_elf_mode FAKE_SHLIBDEPS_MODE=$package_shlibdeps_mode \
            FAKE_PROXY_ELF_MODE=$package_proxy_mode FAKE_FLEET_ELF_MODE=$package_fleet_mode \
            EXPECT_SIDECARS=$package_sidecars \
            CAPTURED_CONTROL="$captured_control" EXPECTED_LICENSE="$root/LICENSE" \
            EXPECTED_NOTICE="$root/NOTICE" EXPECTED_THIRD_PARTY_NOTICES="$root/docs/legal/third-party-notices.md" \
            EXPECTED_PROVENANCE="$root/docs/legal/provenance.md" \
            EXPECTED_ADDITIONAL_TERMS="$root/docs/legal/additional-terms.md" \
            EXPECTED_SOURCE_AVAILABILITY="$root/docs/legal/source-availability.md" \
            EXPECTED_RELEASE_VERIFICATION="$root/docs/releases/verification.md" \
            EXPECTED_TERMINAL_FAILURE_TARGET="$root/packaging/linux/teslatlas-hub-terminal-failure.target" \
            EXPECTED_LEGAL_BUNDLE="$([ "$package_sidecars" = 1 ] && printf %s "$legal_bundle_sidecar" || printf %s "$legal_bundle_base")" \
            EXPECTED_PROXY="$test_root/fake-command-proxy" \
            EXPECTED_FLEET="$test_root/fake-fleet-telemetry" \
            EXPECTED_PROXY_SHA="$package_proxy_sha" EXPECTED_FLEET_SHA="$package_fleet_sha" \
            EXPECTED_SIDECAR_LOCK="$package_fixture/packaging/linux/sidecar-sha256.lock" \
            PATH="$elf_bin:$PATH" TMPDIR="$test_root" \
            sh "$package_builder" "$@"
    ) > "$test_root/package-output" 2>&1
}

ln -s fake-binary "$test_root/fake-binary-link"
if (
    cd "$test_root"
    PATH="$elf_bin:$PATH" TMPDIR="$test_root" \
        sh "$package_builder" --binary fake-binary-link \
        --version 1.0.0 --architecture amd64 --output "$test_root/symlink.deb"
) >"$test_root/symlink-output" 2>&1; then
    fail 'symlinked binary accepted'
fi

for lone_option in --command-proxy-binary --fleet-telemetry-binary; do
    if (
        cd "$test_root"
        PATH="$elf_bin:$PATH" TMPDIR="$test_root" \
            sh "$package_builder" --binary fake-binary \
            --version 1.0.0 --architecture amd64 --output "$test_root/lone.deb" \
            "$lone_option" fake-command-proxy
    ) >"$test_root/lone-output" 2>&1; then
        fail "accepted unpaired sidecar option: $lone_option"
    fi
done

printf '%s' changed > "$test_root/fake-command-proxy"
if (
    cd "$test_root"
    PATH="$elf_bin:$PATH" TMPDIR="$test_root" \
        sh "$package_builder" --binary fake-binary \
        --version 1.0.0 --architecture amd64 --output "$test_root/bad-digest.deb" \
        --command-proxy-binary fake-command-proxy \
        --fleet-telemetry-binary fake-fleet-telemetry
) >"$test_root/bad-digest-output" 2>&1; then
    fail 'mismatched sidecar digests accepted'
fi
: > "$test_root/fake-command-proxy"

run_package good_amd64 amd64 || fail 'valid amd64 ELF rejected'
grep -Fqx 'Version: 1.0.0-1' "$captured_control" \
    || fail 'stable semver was not mapped to a Debian revision'
grep -Fqx 'Depends: adduser, ca-certificates, systemd (>= 254), libc6 (>= 2.38), libgcc-s1 (>= 3.0)' \
    "$captured_control" || fail 'amd64 shared-library dependencies missing from control'
grep -Fqx 'Description: Self-hosted multi-car Tesla telemetry hub' "$captured_control" \
    || fail 'package description does not describe multi-car support'
run_package good_arm64 arm64 || fail 'valid dynamic arm64 ELF rejected'
grep -Fqx 'Depends: adduser, ca-certificates, systemd (>= 254), libc6 (>= 2.38), libgcc-s1 (>= 3.0)' \
    "$captured_control" || fail 'arm64 shared-library dependencies missing from control'
run_package good_arm64_static arm64 || fail 'valid static arm64 ELF rejected'
grep -Fqx 'Depends: adduser, ca-certificates, systemd (>= 254)' "$captured_control" \
    || fail 'static arm64 package gained shared-library dependencies'
run_package good_amd64 amd64 good 1.0.0-beta.1 || fail 'valid prerelease rejected'
grep -Fqx 'Version: 1.0.0~beta.1-1' "$captured_control" \
    || fail 'prerelease does not sort before the stable Debian package'
if (
    cd "$test_root"
    FAKE_HUB_VERSION=1.0.0-alpha.2 PATH="$elf_bin:$PATH" TMPDIR="$test_root" \
        sh "$package_builder" --binary fake-binary --version 1.0.0-beta.1 \
        --architecture amd64 --output "$test_root/version-mismatch.deb" \
        --legal-bundle "$legal_bundle_base"
) >"$test_root/version-mismatch-output" 2>&1; then
    fail 'mismatched Hub binary version accepted'
fi
grep -Fq 'binary version does not match package version' \
    "$test_root/version-mismatch-output" \
    || fail 'mismatched Hub binary version has no stable diagnostic'
for noisy_version_variable in FAKE_HUB_EXTRA_NEWLINE FAKE_HUB_STDERR; do
    if (
        cd "$test_root"
        env "$noisy_version_variable=1" FAKE_HUB_VERSION=1.0.0 \
            PATH="$elf_bin:$PATH" TMPDIR="$test_root" \
            sh "$package_builder" --binary fake-binary --version 1.0.0 \
            --architecture amd64 --output "$test_root/noisy-version.deb" \
            --legal-bundle "$legal_bundle_base"
    ) >"$test_root/noisy-version-output" 2>&1; then
        fail "noisy Hub version output accepted: $noisy_version_variable"
    fi
    grep -Fq 'binary version does not match package version' \
        "$test_root/noisy-version-output" \
        || fail "noisy Hub version output has no stable diagnostic: $noisy_version_variable"
done
run_package good_amd64 amd64 good 1.0.0 1 good_amd64_static good_amd64_static \
    || fail 'valid static Fleet sidecars rejected'
run_package good_amd64 amd64 good 1.0.0 1 good_amd64 good_amd64_static \
    || fail 'dynamic command proxy dependencies were not collected'
[ "$(cat "$test_root/shlibdeps-count")" = 2 ] \
    || fail 'dynamic command proxy was omitted from dependency collection'
if run_package good_amd64 amd64 good 1.0.0 1 good_arm64 good_arm64_static; then
    fail 'wrong-architecture command proxy accepted'
fi
if run_package good_amd64 amd64 good 1.0.0 1 good_amd64_static bad_interpreter; then
    fail 'invalid Fleet Telemetry ELF accepted'
fi
for mode in bad_endian bad_abi bad_interpreter dynamic_without_interpreter relocatable shared_object; do
    if run_package "$mode" amd64; then
        fail "invalid ELF accepted: $mode"
    fi
done
for mode in missing duplicate fail; do
    if run_package good_amd64 amd64 "$mode"; then
        fail "invalid dpkg-shlibdeps result accepted: $mode"
    fi
done

echo 'test-linux-packaging: PASS'
