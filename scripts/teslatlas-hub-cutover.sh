#!/usr/bin/env bash
set -Eeuo pipefail
IFS=$'\n\t'
umask 077

readonly PROGRAM_NAME="${0##*/}"
readonly REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
readonly PREFLIGHT_COMMAND=preflight-tesla-mate
readonly IMPORT_COMMAND=import-tesla-mate
readonly WATERMARK_COMMAND=observation-watermark
readonly AUDIT_WATERMARK_COMMAND=audit-watermark
readonly COLLECT_COMMAND=collect-once
readonly VERIFY_COMMAND=verify-observation
readonly VERIFY_NO_WAKE_COMMAND=verify-no-wake
readonly IMPORT_UNIT=teslatlas-hub-import@
readonly TARGET_UNIT=teslatlas-hub.service
readonly COLLECT_UNIT=teslatlas-hub-collect.service
readonly SUPERVISED_UNIT=teslatlas-hub-supervised.service
readonly DEBIAN_COLLECTION_GATE=/run/teslatlas/import-collector.lock
readonly SELF_PATH="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)/$(basename -- "${BASH_SOURCE[0]}")"

PLATFORM=auto
CAR_ID=''
CAR_ID_SEEN=false
SOURCE_HOST=''
SSH_PORT=22
SSH_IDENTITY=''
REMOTE_DB_HOST=''
REMOTE_DB_PORT=5432
LOCAL_DB_PORT=15432
REPORT_DIR=''
CONFIG_PATH=''
HUB_BIN="${TESLATLAS_HUB_BIN:-/usr/bin/teslatlas-hub}"
MAC_ROOT="${TESLATLAS_HUB_MAC_ROOT:-$HOME/Library/Application Support/Teslatlas Hub}"
MAC_SERVICE="${TESLATLAS_HUB_MAC_SERVICE:-$REPO_ROOT/scripts/mac-service.sh}"
MAC_SUPERVISED="${TESLATLAS_HUB_MAC_SUPERVISED:-$MAC_ROOT/bin/teslatlas-hub-supervised}"
MAC_LABEL="${TESLATLAS_HUB_MAC_LABEL:-com.teslatlas.hub}"
MAC_PLIST="${TESLATLAS_HUB_MAC_PLIST:-$HOME/Library/LaunchAgents/com.teslatlas.hub.plist}"
POSTGRES_CREDENTIAL="${TESLATLAS_HUB_POSTGRES_CREDENTIAL:-/etc/teslatlas/credentials/teslamate-postgres-password}"
CURSOR_CREDENTIAL="${TESLATLAS_HUB_CURSOR_CREDENTIAL:-/etc/teslatlas/credentials/cursor-key}"

RUN_DIR=''
REPORT_PATH=''
TUNNEL_PID=''
PLATFORM_NAME=''
TRANSIENT_SEQUENCE=0
PHASE=preflight
RESULT=failed
PREFLIGHT_COMPLETED=false
IMPORT_COMPLETED=false
TARGET_ACTIVATED=false
WAKE_ACKNOWLEDGED=false
WATERMARK_CAPTURED=false
AUDIT_WATERMARK_CAPTURED=false
FOLLOWUP_COMPLETED=false
OBSERVATION_VERIFIED=false
NO_WAKE_VERIFIED=false
WATERMARK=''
AUDIT_WATERMARK=''
CORRELATION_ID=''
DIRECT_WAKE_RECEIPTS=''
UNRESOLVED_RECEIPTS=''
UNRESOLVED_STREAM_SESSIONS=''
DEBIAN_COLLECT_WAS_ACTIVE=false
DEBIAN_SUPERVISED_WAS_ACTIVE=false
MAC_SUPERVISED_WAS_ACTIVE=false
DEBIAN_COLLECT_RESTORED=false
DEBIAN_SUPERVISED_RESTORED=false
MAC_SUPERVISED_RESTORED=false
DEBIAN_COLLECT_RESTORE_ATTEMPTED=false
DEBIAN_SUPERVISED_RESTORE_ATTEMPTED=false
MAC_SUPERVISED_RESTORE_ATTEMPTED=false
COLLECTORS_RESTORED=false
CUTOVER_LOCK_HELD=false
MAC_SUPERVISED_STATE_HANDOFF=false
MAC_CUTOVER_HANDOFF=''
MAC_HANDOFF_DIR=''
while (($#)); do
  case "$1" in
    --compatibility-lock-held)
      [[ "$CUTOVER_LOCK_HELD" != true ]] || die 'compatibility-lock-held may be specified only once'
      CUTOVER_LOCK_HELD=true
      shift
      ;;
    --mac-supervised-was-active)
      [[ "$CUTOVER_LOCK_HELD" == true && "$MAC_SUPERVISED_STATE_HANDOFF" != true ]] || \
        die 'invalid internal supervised-state handoff'
      MAC_SUPERVISED_STATE_HANDOFF=true
      MAC_SUPERVISED_WAS_ACTIVE=true
      shift
      ;;
    --mac-cutover-handoff)
      [[ "$CUTOVER_LOCK_HELD" == true && $# -ge 2 && -z "$MAC_CUTOVER_HANDOFF" && "$2" == /* ]] || \
        die 'invalid internal compatibility-lock handoff'
      MAC_CUTOVER_HANDOFF=$2
      shift 2
      ;;
    *) break ;;
  esac
done
ORIGINAL_ARGS=("$@")

usage() {
  cat <<'EOF'
Usage:
  teslatlas-hub-cutover.sh --car-id ID --source-host HOST \
    --remote-db-host HOST [options]

Runs one bounded cutover gate. The source is contacted only by an SSH
local-forward tunnel; this script never starts, stops, schedules, or changes
TeslaMate, Docker, or PostgreSQL.

Required:
  --car-id ID                 Positive TeslaMate car ID
  --source-host HOST          SSH destination for the source host
  --remote-db-host HOST       PostgreSQL host as seen from the source host

Options:
  --platform auto|macos|debian  Target platform (default: auto)
  --ssh-port PORT               Source SSH port (default: 22)
  --ssh-identity PATH           SSH identity file
  --remote-db-port PORT         Remote PostgreSQL port (default: 5432)
  --local-db-port PORT          Loopback tunnel port (default: 15432)
  --config PATH                 Debian Hub config path
  --report-dir PATH             Redacted report directory
  --help                        Show this help

Credentials come from existing systemd credential drop-ins on Debian or the
existing macOS Keychain service wrapper. No secret is accepted as an argument
or environment value by this sidecar.
EOF
}

die() {
  printf '%s: %s\n' "$PROGRAM_NAME" "$1" >&2
  exit 1
}

validate_port() {
  local value=$1
  [[ "$value" =~ ^[0-9]{1,5}$ ]] || die 'port is invalid'
  (( 10#$value >= 1 && 10#$value <= 65535 )) || die 'port is outside 1-65535'
}

validate_host() {
  [[ "$1" =~ ^[A-Za-z0-9._:@\[\]-]+$ ]] || die 'host contains unsupported characters'
}

platform_name() {
  if [[ "$PLATFORM" != auto ]]; then
    printf '%s\n' "$PLATFORM"
    return
  fi
  case "$(uname -s)" in
    Darwin) printf '%s\n' macos ;;
    Linux) printf '%s\n' debian ;;
    *) die 'unsupported target platform' ;;
  esac
}

mac_supervised_state() {
  local state
  state="$("$MAC_SUPERVISED" active-state)" || return 1
  case "$state" in
    active|inactive)
      printf '%s\n' "$state"
      ;;
    *) return 1 ;;
  esac
}

mac_launchd_state() {
  local label=$1
  local domain="gui/$(id -u)"
  local output
  if output="$(launchctl print "$domain/$label" 2>&1)"; then
    printf '%s\n' active
    return 0
  fi
  case "$output" in
    *'Could not find service'*|*'No such process'*)
      printf '%s\n' inactive
      return 0
      ;;
    *) return 1 ;;
  esac
}

start_debian_collector_unit() {
  [[ -d /run/teslatlas ]] || return 1
  /usr/bin/flock --shared --nonblock "$DEBIAN_COLLECTION_GATE" \
    systemctl start --wait "$1"
}

json_object_or_fail() {
  local path=$1
  python3 - "$path" <<'PY'
import json
import pathlib
import sys

if not isinstance(json.loads(pathlib.Path(sys.argv[1]).read_text()), dict):
    raise SystemExit(1)
PY
}

extract_watermark() {
  local path=$1
  python3 - "$path" <<'PY'
import json
import pathlib
import sys

value = json.loads(pathlib.Path(sys.argv[1]).read_text())
if not isinstance(value, dict):
    raise SystemExit(1)
for key in ("watermark", "observation_watermark", "observationWatermark"):
    candidate = value.get(key)
    if isinstance(candidate, (str, int)) and str(candidate):
        rendered = str(candidate)
        if all(char.isalnum() or char in "._:-" for char in rendered):
            print(rendered)
            raise SystemExit(0)
raise SystemExit(1)
PY
}

extract_audit_watermark() {
  local path=$1
  python3 - "$path" <<'PY'
import json
import pathlib
import sys

value = json.loads(pathlib.Path(sys.argv[1]).read_text())
candidate = value.get("watermark") if isinstance(value, dict) else None
if isinstance(candidate, int) and candidate >= 0:
    print(candidate)
elif isinstance(candidate, str) and candidate.isascii() and candidate.isdecimal():
    print(candidate)
else:
    raise SystemExit(1)
PY
}

extract_correlation_id() {
  local path=$1
  python3 - "$path" <<'PY'
import json
import pathlib
import sys
import uuid

value = json.loads(pathlib.Path(sys.argv[1]).read_text())
candidate = None
if isinstance(value, dict):
    candidate = value.get("request_audit_correlation_id", value.get("correlationId"))
if not isinstance(candidate, str):
    raise SystemExit(1)
try:
    parsed = uuid.UUID(candidate)
except ValueError:
    raise SystemExit(1)
if str(parsed) != candidate.lower():
    raise SystemExit(1)
print(str(parsed))
PY
}

extract_no_wake_verification() {
  local path=$1
  python3 - "$path" <<'PY'
import json
import pathlib
import sys

value = json.loads(pathlib.Path(sys.argv[1]).read_text())
if not isinstance(value, dict) or value.get("verified") is not True:
    raise SystemExit(1)
counts = []
for field in ("directWakeReceipts", "unresolvedReceipts", "unresolvedStreamSessions"):
    candidate = value.get(field)
    if isinstance(candidate, bool) or not isinstance(candidate, int) or candidate < 0:
        raise SystemExit(1)
    counts.append(str(candidate))
print("\t".join(counts))
PY
}

write_report() {
  [[ -n "$REPORT_PATH" ]] || return 0
  local temporary="${REPORT_PATH}.tmp"
  printf '{"result":"%s","phase":"%s","carId":%s,"preflightCompleted":%s,"importCompleted":%s,"targetActivated":%s,"wakeAcknowledged":%s,"watermarkCaptured":%s,"followupCompleted":%s,"observationVerified":%s,"collectorsRestored":%s' \
    "$RESULT" "$PHASE" "$CAR_ID" "$PREFLIGHT_COMPLETED" "$IMPORT_COMPLETED" \
    "$TARGET_ACTIVATED" "$WAKE_ACKNOWLEDGED" "$WATERMARK_CAPTURED" \
    "$FOLLOWUP_COMPLETED" "$OBSERVATION_VERIFIED" "$COLLECTORS_RESTORED" >"$temporary"
  if [[ -n "$WATERMARK" ]]; then
    printf ',"watermark":"%s"' "$WATERMARK" >>"$temporary"
  fi
  if [[ -n "$AUDIT_WATERMARK" ]]; then
    printf ',"auditWatermark":"%s"' "$AUDIT_WATERMARK" >>"$temporary"
  fi
  if [[ -n "$CORRELATION_ID" ]]; then
    printf ',"correlationId":"%s"' "$CORRELATION_ID" >>"$temporary"
  fi
  printf ',"noWakeVerified":%s,"directWakeReceipts":%s,"unresolvedReceipts":%s,"unresolvedStreamSessions":%s' \
    "$NO_WAKE_VERIFIED" "${DIRECT_WAKE_RECEIPTS:-null}" "${UNRESOLVED_RECEIPTS:-null}" \
    "${UNRESOLVED_STREAM_SESSIONS:-null}" >>"$temporary"
  printf '}\n' >>"$temporary"
  chmod 600 "$temporary"
  mv -f -- "$temporary" "$REPORT_PATH"
}

record_mac_cutover_handoff() {
  local state=$1
  [[ -n "$MAC_CUTOVER_HANDOFF" ]] || return 0
  [[ -f "$MAC_CUTOVER_HANDOFF" && ! -L "$MAC_CUTOVER_HANDOFF" && -O "$MAC_CUTOVER_HANDOFF" ]] || return 1
  printf '%s\n' "$state" >"$MAC_CUTOVER_HANDOFF"
}

mac_cutover_handoff_restored() {
  local path=$1
  local state
  [[ -f "$path" && ! -L "$path" && -O "$path" ]] || return 1
  IFS= read -r state <"$path" || return 1
  [[ "$state" == restored ]]
}

cleanup() {
  if [[ -n "$TUNNEL_PID" ]]; then
    kill "$TUNNEL_PID" 2>/dev/null || true
    wait "$TUNNEL_PID" 2>/dev/null || true
    TUNNEL_PID=''
  fi
  if [[ -n "$RUN_DIR" ]]; then
    rm -rf -- "$RUN_DIR"
    RUN_DIR=''
  fi
  if [[ -n "$MAC_HANDOFF_DIR" ]]; then
    rm -rf -- "$MAC_HANDOFF_DIR"
    MAC_HANDOFF_DIR=''
  fi
}

on_exit() {
  local status=$?
  cleanup
  if [[ "$COLLECTORS_RESTORED" != true ]]; then
    if ! restore_collectors; then
      RESULT=failed
      PHASE=collector_restore
      status=1
    fi
  fi
  write_report || true
  if [[ -n "$MAC_CUTOVER_HANDOFF" ]]; then
    if [[ "$MAC_SUPERVISED_WAS_ACTIVE" != true || "$MAC_SUPERVISED_RESTORED" == true ]]; then
      if ! record_mac_cutover_handoff restored; then
        RESULT=failed
        PHASE=collector_restore
        status=1
      fi
    else
      record_mac_cutover_handoff restore-failed || true
    fi
  fi
  exit "$status"
}
trap on_exit EXIT

pause_collectors() {
  if [[ "$PLATFORM_NAME" == debian ]]; then
    if systemctl is-active --quiet "$COLLECT_UNIT"; then
      DEBIAN_COLLECT_WAS_ACTIVE=true
    fi
    if systemctl is-active --quiet "$SUPERVISED_UNIT"; then
      DEBIAN_SUPERVISED_WAS_ACTIVE=true
    fi
    if [[ "$DEBIAN_COLLECT_WAS_ACTIVE" == true ]]; then
      systemctl stop "$COLLECT_UNIT" >/dev/null 2>&1 || die 'Hub manual collection could not be paused'
    fi
    if [[ "$DEBIAN_SUPERVISED_WAS_ACTIVE" == true ]]; then
      systemctl stop "$SUPERVISED_UNIT" >/dev/null 2>&1 || die 'Hub supervised collection could not be paused'
    fi
    if systemctl is-active --quiet "$COLLECT_UNIT"; then
      die 'Hub manual collection remained active'
    fi
    if systemctl is-active --quiet "$SUPERVISED_UNIT"; then
      die 'Hub supervised collection remained active'
    fi
  else
    local mac_state
    mac_state="$(mac_supervised_state)" || die 'Hub supervised collection state is unavailable'
    if [[ "$mac_state" == active ]]; then
      MAC_SUPERVISED_WAS_ACTIVE=true
      "$MAC_SUPERVISED" disable >/dev/null 2>&1 || die 'Hub supervised collection could not be paused'
      mac_state="$(mac_supervised_state)" || die 'Hub supervised collection state is unavailable after pause'
      if [[ "$mac_state" != inactive ]]; then
        die 'Hub supervised collection remained active'
      fi
    fi
  fi
}

restore_collectors() {
  if [[ "$PLATFORM_NAME" == debian ]]; then
    if [[ "$DEBIAN_SUPERVISED_WAS_ACTIVE" == true && "$DEBIAN_SUPERVISED_RESTORED" != true ]]; then
      [[ "$DEBIAN_SUPERVISED_RESTORE_ATTEMPTED" != true ]] || return 1
      DEBIAN_SUPERVISED_RESTORE_ATTEMPTED=true
      start_debian_collector_unit "$SUPERVISED_UNIT" >/dev/null 2>&1 || return 1
      systemctl is-active --quiet "$SUPERVISED_UNIT" || return 1
      DEBIAN_SUPERVISED_RESTORED=true
    fi
    if [[ "$DEBIAN_COLLECT_WAS_ACTIVE" == true && "$DEBIAN_COLLECT_RESTORED" != true ]]; then
      [[ "$DEBIAN_COLLECT_RESTORE_ATTEMPTED" != true ]] || return 1
      DEBIAN_COLLECT_RESTORE_ATTEMPTED=true
      start_debian_collector_unit "$COLLECT_UNIT" >/dev/null 2>&1 || return 1
      if systemctl is-failed --quiet "$COLLECT_UNIT"; then
        return 1
      fi
      DEBIAN_COLLECT_RESTORED=true
    fi
  elif [[ "$MAC_SUPERVISED_WAS_ACTIVE" == true && "$MAC_SUPERVISED_RESTORED" != true ]]; then
    [[ "$MAC_SUPERVISED_RESTORE_ATTEMPTED" != true ]] || return 1
    MAC_SUPERVISED_RESTORE_ATTEMPTED=true
    "$MAC_SUPERVISED" enable >/dev/null 2>&1 || return 1
    [[ "$(mac_supervised_state)" == active ]] || return 1
    MAC_SUPERVISED_RESTORED=true
  fi
  COLLECTORS_RESTORED=true
}

run_debian_transient() {
  local timeout_seconds=$1
  shift
  TRANSIENT_SEQUENCE=$((TRANSIENT_SEQUENCE + 1))
  local unit="teslatlas-hub-cutover-${$}-${TRANSIENT_SEQUENCE}"
  systemd-run --quiet --wait --pipe --collect \
    --unit="$unit" \
    --property=User=teslatlas \
    --property=Group=teslatlas \
    --property=UMask=0077 \
    --property=WorkingDirectory=/var/lib/teslatlas \
    --property="TimeoutStartSec=${timeout_seconds}s" \
    --property="LoadCredentialEncrypted=teslamate-postgres-password:${POSTGRES_CREDENTIAL}" \
    --property="LoadCredentialEncrypted=cursor-key:${CURSOR_CREDENTIAL}" \
    "$HUB_BIN" --config "$CONFIG_PATH" "$@"
}

run_hub() {
  if [[ "$PLATFORM_NAME" == macos ]]; then
    TESLATLAS_HUB_MAC_ROOT="$MAC_ROOT" "$MAC_SERVICE" "$@"
  else
    run_debian_transient 90 "$@"
  fi
}

run_hub_quiet() {
  run_hub "$@" >/dev/null 2>/dev/null
}

start_tunnel() {
  local forward="127.0.0.1:${LOCAL_DB_PORT}:${REMOTE_DB_HOST}:${REMOTE_DB_PORT}"
  local ssh_args=(
    -T -N
    -o BatchMode=yes
    -o ExitOnForwardFailure=yes
    -o LogLevel=ERROR
    -p "$SSH_PORT"
    -L "$forward"
  )
  if [[ -n "$SSH_IDENTITY" ]]; then
    ssh_args+=( -i "$SSH_IDENTITY" )
  fi
  ssh "${ssh_args[@]}" -- "$SOURCE_HOST" >/dev/null 2>&1 &
  TUNNEL_PID=$!
  for _ in {1..40}; do
    kill -0 "$TUNNEL_PID" 2>/dev/null || die 'SSH tunnel failed to start'
    sleep 0.25
  done
}

run_preflight() {
  PHASE=preflight
  run_hub_quiet "$PREFLIGHT_COMMAND" --car-id "$CAR_ID" || \
    die 'Hub TeslaMate preflight failed'
  PREFLIGHT_COMPLETED=true
}

run_import() {
  PHASE=import
  pause_collectors
  if [[ "$PLATFORM_NAME" == debian ]]; then
    systemctl start --wait "${IMPORT_UNIT}${CAR_ID}.service" >/dev/null 2>&1 || \
      die 'Hub historical import failed'
  else
    run_hub_quiet "$IMPORT_COMMAND" --car-id "$CAR_ID" || \
      die 'Hub historical import failed'
  fi
  IMPORT_COMPLETED=true
}

activate_target() {
  PHASE=target_activation
  if [[ "$PLATFORM_NAME" == debian ]]; then
    systemctl start "$TARGET_UNIT" >/dev/null 2>&1 || die 'Hub service activation failed'
    systemctl is-active --quiet "$TARGET_UNIT" || die 'Hub service is not active'
  else
    local target_state
    target_state="$(mac_launchd_state "$MAC_LABEL")" || die 'macOS Hub LaunchAgent state is unavailable'
    if [[ "$target_state" == inactive ]]; then
      [[ -f "$MAC_PLIST" ]] || die 'macOS Hub LaunchAgent plist is missing'
      launchctl bootstrap "gui/$(id -u)" "$MAC_PLIST" >/dev/null 2>&1 || \
        die 'macOS Hub LaunchAgent activation failed'
    fi
    launchctl kickstart "gui/$(id -u)/$MAC_LABEL" >/dev/null 2>&1 || \
      die 'macOS Hub LaunchAgent start failed'
    target_state="$(mac_launchd_state "$MAC_LABEL")" || die 'macOS Hub LaunchAgent state is unavailable after start'
    [[ "$target_state" == active ]] || \
      die 'macOS Hub LaunchAgent is not active'
  fi
  TARGET_ACTIVATED=true
}

capture_watermark() {
  PHASE=observation_watermark
  local output="$RUN_DIR/watermark.json"
  : >"$output"
  chmod 600 "$output"
  run_hub "$WATERMARK_COMMAND" --car-id "$CAR_ID" >"$output" 2>/dev/null || \
    die 'Hub observation watermark command failed'
  json_object_or_fail "$output" || die 'Hub observation watermark evidence was not valid JSON'
  WATERMARK="$(extract_watermark "$output")" || die 'Hub observation watermark was missing'
  rm -f -- "$output"
  WATERMARK_CAPTURED=true
}

capture_audit_watermark() {
  PHASE=audit_watermark
  local output="$RUN_DIR/audit-watermark.json"
  : >"$output"
  chmod 600 "$output"
  run_hub "$AUDIT_WATERMARK_COMMAND" >"$output" 2>/dev/null || \
    die 'Hub audit watermark command failed'
  json_object_or_fail "$output" || die 'Hub audit watermark evidence was not valid JSON'
  AUDIT_WATERMARK="$(extract_audit_watermark "$output")" || die 'Hub audit watermark was missing'
  rm -f -- "$output"
  AUDIT_WATERMARK_CAPTURED=true
}

run_followup() {
  PHASE=manual_wake
  printf '%s\n' 'Wake the car manually now. Press Enter only after it is awake.' >&2
  IFS= read -r _
  WAKE_ACKNOWLEDGED=true
  PHASE=wait_after_wake
  sleep 60
  PHASE=followup_collection
  local collection_output="$RUN_DIR/collection.json"
  : >"$collection_output"
  chmod 600 "$collection_output"
  if [[ "$PLATFORM_NAME" == debian ]]; then
    local collection_started_epoch
    collection_started_epoch=$(date +%s)
    start_debian_collector_unit "$COLLECT_UNIT" >/dev/null 2>&1 || \
      die 'Hub follow-up collection failed'
    journalctl --unit "$COLLECT_UNIT" --since "@${collection_started_epoch}" \
      --no-pager --output=cat | awk '/^\{/{report=$0} END{print report}' >"$collection_output"
  else
    run_hub "$COLLECT_COMMAND" >"$collection_output" 2>/dev/null || \
      die 'Hub follow-up collection failed'
  fi
  json_object_or_fail "$collection_output" || die 'Hub collector receipt was not valid JSON'
  CORRELATION_ID="$(extract_correlation_id "$collection_output")" || \
    die 'Hub collector receipt correlation ID was invalid'
  rm -f -- "$collection_output"
  FOLLOWUP_COMPLETED=true
}

verify_observation() {
  PHASE=verify_observation
  run_hub_quiet "$VERIFY_COMMAND" --car-id "$CAR_ID" \
    --watermark "$WATERMARK" || \
    die 'Hub durable observation verification failed'
  OBSERVATION_VERIFIED=true
}

verify_no_wake() {
  PHASE=verify_no_wake
  local output="$RUN_DIR/no-wake-verification.json"
  : >"$output"
  chmod 600 "$output"
  run_hub "$VERIFY_NO_WAKE_COMMAND" --audit-watermark "$AUDIT_WATERMARK" \
    --correlation-id "$CORRELATION_ID" --car-id "$CAR_ID" \
    --observation-watermark "$WATERMARK" >"$output" 2>/dev/null || \
    die 'Hub no-wake verification command failed'
  json_object_or_fail "$output" || die 'Hub no-wake verification was not valid JSON'
  IFS=$'\t' read -r DIRECT_WAKE_RECEIPTS UNRESOLVED_RECEIPTS UNRESOLVED_STREAM_SESSIONS \
    < <(extract_no_wake_verification "$output") || \
    die 'Hub no-wake verification failed'
  rm -f -- "$output"
  NO_WAKE_VERIFIED=true
}

while (($#)); do
  case "$1" in
    --platform|--car-id|--source-host|--ssh-port|--ssh-identity|--remote-db-host|\
    --remote-db-port|--local-db-port|--config|--report-dir)
      (($# >= 2)) || die 'option requires a value'
      if [[ "$1" == --car-id && "$CAR_ID_SEEN" == true ]]; then
        die 'car ID may be specified only once'
      fi
      case "$1" in
        --platform) PLATFORM=$2 ;;
        --car-id) CAR_ID=$2; CAR_ID_SEEN=true ;;
        --source-host) SOURCE_HOST=$2 ;;
        --ssh-port) SSH_PORT=$2 ;;
        --ssh-identity) SSH_IDENTITY=$2 ;;
        --remote-db-host) REMOTE_DB_HOST=$2 ;;
        --remote-db-port) REMOTE_DB_PORT=$2 ;;
        --local-db-port) LOCAL_DB_PORT=$2 ;;
        --config) CONFIG_PATH=$2 ;;
        --report-dir) REPORT_DIR=$2 ;;
      esac
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      die 'unknown option'
      ;;
  esac
done

[[ "$PLATFORM" == auto || "$PLATFORM" == macos || "$PLATFORM" == debian ]] || \
  die 'platform is invalid'
[[ "$CAR_ID" =~ ^[1-9][0-9]*$ ]] || die 'car ID is invalid'
[[ -n "$SOURCE_HOST" ]] || die 'source host is required'
[[ -n "$REMOTE_DB_HOST" ]] || die 'remote database host is required'
validate_host "$SOURCE_HOST"
validate_host "$REMOTE_DB_HOST"
validate_port "$SSH_PORT"
validate_port "$REMOTE_DB_PORT"
validate_port "$LOCAL_DB_PORT"

PLATFORM_NAME=$(platform_name)
if [[ "$PLATFORM_NAME" == debian ]]; then
  [[ "$(uname -s)" == Linux && ( "$(uname -m)" == aarch64 || "$(uname -m)" == x86_64 ) && -f /etc/debian_version ]] || \
    die 'Debian path requires Debian on aarch64 or x86_64'
  [[ "$EUID" -eq 0 ]] || die 'Debian path requires root for systemd credentials'
  command -v systemd-run >/dev/null 2>&1 || die 'systemd-run is required'
  command -v systemctl >/dev/null 2>&1 || die 'systemctl is required'
  command -v flock >/dev/null 2>&1 || die 'flock is required'
  command -v "$HUB_BIN" >/dev/null 2>&1 || die 'Hub binary is missing'
  CONFIG_PATH=${CONFIG_PATH:-/etc/teslatlas/config.toml}
  [[ "$CONFIG_PATH" == /etc/teslatlas/config.toml ]] || \
    die 'Debian config override is unsupported; use /etc/teslatlas/config.toml'
else
  [[ "$(uname -s)" == Darwin && "$(uname -m)" == arm64 ]] || \
    die 'macOS path requires macOS on arm64'
  [[ -x "$MAC_SERVICE" ]] || die 'macOS Hub service wrapper is missing'
  [[ -x "$MAC_SUPERVISED" ]] || die 'macOS Hub supervised controller is missing'
  CONFIG_PATH=${CONFIG_PATH:-$MAC_ROOT/config.toml}
  [[ "$CONFIG_PATH" == "$MAC_ROOT/config.toml" ]] || \
    die 'macOS config override is unsupported; use the installed config path'
fi

if [[ "$PLATFORM_NAME" == macos ]]; then
  if [[ "$CUTOVER_LOCK_HELD" != true ]]; then
    # The supervised collector may itself own the compatibility lease. Pause it
    # before acquiring the cutover lease; a failed acquisition leaves this
    # parent alive to restore the collector through its EXIT trap.
    pause_collectors
    MAC_HANDOFF_DIR="$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-hub-cutover-lock.XXXXXX")" || die 'could not create macOS lock handoff directory'
    chmod 700 "$MAC_HANDOFF_DIR" || die 'could not secure macOS lock handoff directory'
    mac_handoff_path="$MAC_HANDOFF_DIR/restoration"
    : >"$mac_handoff_path"
    chmod 600 "$mac_handoff_path" || die 'could not secure macOS lock handoff'
    child_args=("$SELF_PATH" --compatibility-lock-held --mac-cutover-handoff "$mac_handoff_path")
    if [[ "$MAC_SUPERVISED_WAS_ACTIVE" == true ]]; then
      child_args+=(--mac-supervised-was-active)
    fi
    child_args+=("${ORIGINAL_ARGS[@]}")
    if "$MAC_SERVICE" with-compatibility-lock -- "${child_args[@]}"; then
      child_status=0
    else
      child_status=$?
    fi
    if mac_cutover_handoff_restored "$mac_handoff_path"; then
      MAC_SUPERVISED_RESTORED=true
    fi
    rm -rf -- "$MAC_HANDOFF_DIR"
    MAC_HANDOFF_DIR=''
    exit "$child_status"
  fi
  "$MAC_SERVICE" verify-compatibility-lock || \
    die 'macOS compatibility lock lease is invalid'
fi
command -v ssh >/dev/null 2>&1 || die 'ssh is required'
command -v python3 >/dev/null 2>&1 || die 'python3 is required'
[[ -z "$SSH_IDENTITY" || -r "$SSH_IDENTITY" ]] || die 'SSH identity is not readable'
[[ -f "$CONFIG_PATH" ]] || die 'Hub configuration is missing'

if [[ -z "$REPORT_DIR" ]]; then
  if [[ "$PLATFORM_NAME" == debian ]]; then
    REPORT_DIR=/var/lib/teslatlas/cutover-reports
  else
    REPORT_DIR="$MAC_ROOT/cutover-reports"
  fi
fi
mkdir -p -- "$REPORT_DIR"
chmod 700 "$REPORT_DIR"
RUN_DIR=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-hub-cutover.XXXXXX")
REPORT_PATH=$(mktemp "$REPORT_DIR/teslatlas-hub-cutover.XXXXXXXX.json")
chmod 600 "$REPORT_PATH"

start_tunnel
run_preflight
run_import
activate_target
capture_watermark
capture_audit_watermark
run_followup
verify_observation
verify_no_wake

restore_collectors || die 'Hub collector state could not be restored'

RESULT=passed
PHASE=complete
printf '%s\n' 'Hub cutover gate passed.'
printf 'Redacted report: %s\n' "$REPORT_PATH"
