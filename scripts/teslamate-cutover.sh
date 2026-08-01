#!/usr/bin/env bash
set -euo pipefail

# Operator-owned TeslaMate to Hub gate for Debian/systemd hosts. This script
# starts and verifies Hub services only. It never stops, restarts, edits, or
# schedules TeslaMate, Docker, or PostgreSQL.

REPORT_DIR=/var/lib/teslatlas/cutover-reports
DEBIAN_COLLECTION_GATE=/run/teslatlas/import-collector.lock
HUB_BIN=${TESLATLAS_HUB_BIN:-/usr/bin/teslatlas-hub}
HUB_CONFIG=${TESLATLAS_HUB_CONFIG:-/etc/teslatlas/config.toml}
CAR_ID=
CAR_ID_SEEN=false
RESULT=failed
PHASE=preflight
IMPORT_COMPLETED=false
COLLECTOR_COMPLETED=false
WAKE_ACKNOWLEDGED=false
OBSERVATIONS_INSERTED=null
OBSERVATION_WATERMARK=
AUDIT_WATERMARK=
CORRELATION_ID=
NO_WAKE_VERIFIED=false
DIRECT_WAKE_RECEIPTS=null
UNRESOLVED_RECEIPTS=null
UNRESOLVED_STREAM_SESSIONS=null
REPORT=
SUPERVISED_WAS_ACTIVE=false
COLLECT_WAS_ACTIVE=false
SUPERVISED_RESTORED=false
COLLECT_RESTORED=false
SUPERVISED_RESTORE_ATTEMPTED=false
COLLECT_RESTORE_ATTEMPTED=false
COLLECTORS_RESTORED=false

usage() {
    cat <<'EOF'
Usage: teslamate-cutover.sh --car-id ID [--report-dir PATH]

Runs one read-only TeslaMate import, asks the owner to wake the car manually,
waits exactly 60 seconds, then runs one no-wake Hub collection. TeslaMate is
never controlled by this script.
EOF
}

die() {
    printf 'teslatlas cutover: %s\n' "$*" >&2
    exit 1
}

start_debian_collector_unit() {
    [ -d /run/teslatlas ] || return 1
    /usr/bin/flock --shared --nonblock "$DEBIAN_COLLECTION_GATE" \
        systemctl start --wait "$1"
}

write_report() {
    [ -n "$REPORT" ] || return 0
    local finished_at_ms
    finished_at_ms=$(( $(date +%s) * 1000 ))
    cat >"$REPORT" <<EOF
{"result":"$RESULT","phase":"$PHASE","carId":$CAR_ID,"importCompleted":$IMPORT_COMPLETED,"wakeAcknowledged":$WAKE_ACKNOWLEDGED,"collectorCompleted":$COLLECTOR_COMPLETED,"observationsInserted":$OBSERVATIONS_INSERTED,"auditWatermark":${AUDIT_WATERMARK:-null},"correlationId":"${CORRELATION_ID}","noWakeVerified":$NO_WAKE_VERIFIED,"directWakeReceipts":$DIRECT_WAKE_RECEIPTS,"unresolvedReceipts":$UNRESOLVED_RECEIPTS,"unresolvedStreamSessions":$UNRESOLVED_STREAM_SESSIONS,"collectorsRestored":$COLLECTORS_RESTORED,"finishedAtMs":$finished_at_ms}
EOF
}

run_hub() {
    runuser -u teslatlas -- "$HUB_BIN" --config "$HUB_CONFIG" "$@"
}

json_field() {
    local field=$1
    python3 -c 'import json, sys; value = json.load(sys.stdin); candidate = value.get(sys.argv[1]) if isinstance(value, dict) else None; print(candidate if isinstance(candidate, (str, int)) and not isinstance(candidate, bool) else "")' "$field"
}

extract_correlation_id() {
    python3 -c 'import json, sys, uuid; value = json.load(sys.stdin); candidate = value.get("request_audit_correlation_id", value.get("correlationId")) if isinstance(value, dict) else None; parsed = uuid.UUID(candidate) if isinstance(candidate, str) else None; print(str(parsed) if str(parsed) == candidate.lower() else "")'
}

extract_no_wake_counts() {
    python3 -c 'import json, sys; value = json.load(sys.stdin); fields = ("directWakeReceipts", "unresolvedReceipts", "unresolvedStreamSessions"); valid = isinstance(value, dict) and value.get("verified") is True and all(isinstance(value.get(field), int) and not isinstance(value.get(field), bool) and value[field] >= 0 for field in fields); print("\t".join(str(value[field]) for field in fields) if valid else "")'
}

pause_collectors() {
    if systemctl is-active --quiet teslatlas-hub-collect.service; then
        COLLECT_WAS_ACTIVE=true
        systemctl stop teslatlas-hub-collect.service
    fi
    if systemctl is-active --quiet teslatlas-hub-supervised.service; then
        SUPERVISED_WAS_ACTIVE=true
        systemctl stop teslatlas-hub-supervised.service
    fi
    if systemctl is-active --quiet teslatlas-hub-collect.service; then
        die "manual Hub collection remained active"
    fi
    if systemctl is-active --quiet teslatlas-hub-supervised.service; then
        die "supervised Hub collection remained active"
    fi
}

restore_collectors() {
    if [ "$SUPERVISED_WAS_ACTIVE" = true ] && [ "$SUPERVISED_RESTORED" != true ]; then
        [ "$SUPERVISED_RESTORE_ATTEMPTED" != true ] || return 1
        SUPERVISED_RESTORE_ATTEMPTED=true
        start_debian_collector_unit teslatlas-hub-supervised.service || return 1
        systemctl is-active --quiet teslatlas-hub-supervised.service || return 1
        SUPERVISED_RESTORED=true
    fi
    if [ "$COLLECT_WAS_ACTIVE" = true ] && [ "$COLLECT_RESTORED" != true ]; then
        [ "$COLLECT_RESTORE_ATTEMPTED" != true ] || return 1
        COLLECT_RESTORE_ATTEMPTED=true
        start_debian_collector_unit teslatlas-hub-collect.service || return 1
        if systemctl is-failed --quiet teslatlas-hub-collect.service; then
            return 1
        fi
        COLLECT_RESTORED=true
    fi
    COLLECTORS_RESTORED=true
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --car-id)
            [ "$#" -ge 2 ] || die "--car-id needs a value"
            [ "$CAR_ID_SEEN" = false ] || die "--car-id may be specified only once"
            CAR_ID=$2
            CAR_ID_SEEN=true
            shift 2
            ;;
        --report-dir)
            [ "$#" -ge 2 ] || die "--report-dir needs a value"
            REPORT_DIR=$2
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            die "unknown argument: $1"
            ;;
    esac
done

[[ "$CAR_ID" =~ ^[1-9][0-9]*$ ]] || die "--car-id must be a positive integer"
[ "$EUID" -eq 0 ] || die "run as root so systemd may start Hub units"
if ! [[ "$(uname -s)" == Linux && ( "$(uname -m)" == aarch64 || "$(uname -m)" == x86_64 ) && -f /etc/debian_version ]]; then
    die "requires Debian on aarch64 or x86_64"
fi
command -v systemctl >/dev/null 2>&1 || die "systemctl is required"
command -v journalctl >/dev/null 2>&1 || die "journalctl is required"
command -v flock >/dev/null 2>&1 || die "flock is required"
command -v runuser >/dev/null 2>&1 || die "runuser is required"
command -v python3 >/dev/null 2>&1 || die "python3 is required"
command -v "$HUB_BIN" >/dev/null 2>&1 || die "Hub binary is missing"
[ "$HUB_CONFIG" = /etc/teslatlas/config.toml ] || die "Hub config override is unsupported"
[ -f "$HUB_CONFIG" ] || die "Hub configuration is missing"

umask 077
mkdir -p "$REPORT_DIR"
chmod 0700 "$REPORT_DIR"
REPORT=$(mktemp "$REPORT_DIR/teslamate-cutover.XXXXXXXX.json")
on_exit() {
    status=$?
    if [ "$COLLECTORS_RESTORED" != true ]; then
        if ! restore_collectors; then
            RESULT=failed
            PHASE=collector_restore
            status=1
        fi
    fi
    write_report
    exit "$status"
}
trap on_exit EXIT

systemctl start teslatlas-hub.service
systemctl is-active --quiet teslatlas-hub.service || die "Hub service is not active"

PHASE=import
pause_collectors
systemctl start --wait "teslatlas-hub-import@${CAR_ID}.service"
IMPORT_COMPLETED=true

PHASE=watermarks
OBSERVATION_WATERMARK=$(run_hub observation-watermark --car-id "$CAR_ID" | json_field watermark)
[[ "$OBSERVATION_WATERMARK" =~ ^[0-9]+$ ]] || die "observation watermark is invalid"
AUDIT_WATERMARK=$(run_hub audit-watermark | json_field watermark)
[[ "$AUDIT_WATERMARK" =~ ^[0-9]+$ ]] || die "audit watermark is invalid"

PHASE=manual_wake
printf 'Wake the car manually now. Press Enter only after it is awake.\n' >&2
IFS= read -r _
WAKE_ACKNOWLEDGED=true

PHASE=wait_after_wake
sleep 60

PHASE=collect
COLLECT_STARTED_EPOCH=$(date +%s)
start_debian_collector_unit teslatlas-hub-collect.service
COLLECTOR_COMPLETED=true

COLLECTION_REPORT=$(journalctl \
    --unit teslatlas-hub-collect.service \
    --since "@${COLLECT_STARTED_EPOCH}" \
    --no-pager \
    --output=cat \
    | awk '/^\{/{report=$0} END{print report}')
[ -n "$COLLECTION_REPORT" ] || die "collector produced no durable receipt"
OBSERVATIONS_INSERTED=$(printf '%s\n' "$COLLECTION_REPORT" \
    | python3 -c 'import json, sys; value = json.load(sys.stdin); candidate = value.get("observations_inserted") if isinstance(value, dict) else None; print(candidate if isinstance(candidate, int) and not isinstance(candidate, bool) and candidate >= 0 else "")')
[[ "$OBSERVATIONS_INSERTED" =~ ^[0-9]+$ ]] || die "collector receipt is invalid"
[ "$OBSERVATIONS_INSERTED" -ge 1 ] || die "collector wrote no new observations"
CORRELATION_ID=$(printf '%s\n' "$COLLECTION_REPORT" | extract_correlation_id)
[[ "$CORRELATION_ID" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] || die "collector correlation ID is invalid"

PHASE=verify_observation
run_hub verify-observation --car-id "$CAR_ID" --watermark "$OBSERVATION_WATERMARK" >/dev/null

PHASE=verify_no_wake
NO_WAKE_REPORT=$(run_hub verify-no-wake --audit-watermark "$AUDIT_WATERMARK" --correlation-id "$CORRELATION_ID" --car-id "$CAR_ID" --observation-watermark "$OBSERVATION_WATERMARK")
NO_WAKE_COUNTS=$(printf '%s\n' "$NO_WAKE_REPORT" | extract_no_wake_counts)
IFS=$'\t' read -r DIRECT_WAKE_RECEIPTS UNRESOLVED_RECEIPTS UNRESOLVED_STREAM_SESSIONS <<<"$NO_WAKE_COUNTS"
[[ "$DIRECT_WAKE_RECEIPTS" =~ ^[0-9]+$ && "$UNRESOLVED_RECEIPTS" =~ ^[0-9]+$ && "$UNRESOLVED_STREAM_SESSIONS" =~ ^[0-9]+$ ]] || die "no-wake verification failed"
NO_WAKE_VERIFIED=true

restore_collectors || die "Hub collector state could not be restored"

RESULT=passed
PHASE=complete
printf 'Hub cutover gate passed. Report: %s\n' "$REPORT"
printf 'TeslaMate remains unchanged. Keep its operator-owned schedule as-is.\n'
