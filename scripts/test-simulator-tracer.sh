#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
umask 077

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
TESLATLAS_WORKTREE="${TESLATLAS_WORKTREE:-$ROOT/../teslatlas-hub-test}"
DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode-beta.app/Contents/Developer}"
SIMULATOR_NAME="${TESLATLAS_SIMULATOR_NAME:-iPhone 17 Pro}"
CODE_SIGNING_ALLOWED="${TESLATLAS_CODE_SIGNING_ALLOWED:-YES}"
TRACER_RUNS="${TESLATLAS_HUB_TEST_RUNS:-1}"
USE_REAL_COUNTS="${TESLATLAS_HUB_TEST_USE_REAL_COUNTS:-0}"
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-hub-tracer.XXXXXX")"
SERVER_PID=""
SIMULATOR_UDID=""

PAIR_ENV_VARS=(
  TESLATLAS_HUB_TEST_PAIRING_URI
  TESLATLAS_HUB_TEST_EXPECTED_MANIFEST_ROWS
  TESLATLAS_HUB_TEST_EXPECTED_PACKS
  TESLATLAS_HUB_TEST_EXPECTED_CARS
  TESLATLAS_HUB_TEST_EXPECTED_DRIVES
  TESLATLAS_HUB_TEST_EXPECTED_POSITIONS
  TESLATLAS_HUB_TEST_EXPECTED_CHARGES
  TESLATLAS_HUB_TEST_EXPECTED_CHARGE_SAMPLES
)

bool_true() {
  local lower
  lower="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')"
  case "$lower" in
    1|true|yes|on) return 0 ;;
    *) return 1 ;;
  esac
}

if bool_true "$USE_REAL_COUNTS"; then
  : "${TESLATLAS_LOCAL_SEED_FIXTURE:=0}"
  : "${TESLATLAS_LOCAL_TESLAMATE_AUTO_IMPORT:=1}"
  : "${TESLATLAS_HUB_TEST_EXPECTED_MANIFEST_ROWS:=8984040}"
  : "${TESLATLAS_HUB_TEST_EXPECTED_PACKS:=367}"
  : "${TESLATLAS_HUB_TEST_EXPECTED_CARS:=1}"
  : "${TESLATLAS_HUB_TEST_EXPECTED_DRIVES:=2644}"
  : "${TESLATLAS_HUB_TEST_EXPECTED_POSITIONS:=8764495}"
  : "${TESLATLAS_HUB_TEST_EXPECTED_CHARGES:=604}"
  : "${TESLATLAS_HUB_TEST_EXPECTED_CHARGE_SAMPLES:=216296}"
fi

: "${TESLATLAS_HUB_TEST_EXPECTED_MANIFEST_ROWS:=4}"
: "${TESLATLAS_HUB_TEST_EXPECTED_PACKS:=1}"
: "${TESLATLAS_HUB_TEST_EXPECTED_CARS:=1}"
: "${TESLATLAS_HUB_TEST_EXPECTED_DRIVES:=1}"
: "${TESLATLAS_HUB_TEST_EXPECTED_POSITIONS:=2}"
: "${TESLATLAS_HUB_TEST_EXPECTED_CHARGES:=0}"
: "${TESLATLAS_HUB_TEST_EXPECTED_CHARGE_SAMPLES:=0}"
: "${TESLATLAS_HUB_TEST_PAIRING_URI:=}"
if bool_true "$USE_REAL_COUNTS" && [[ -z "${TESLATLAS_HUB_TEST_RUNS:-}" ]]; then
  TRACER_RUNS=2
fi

if ! [[ "$TRACER_RUNS" =~ ^[1-9][0-9]*$ ]]; then
  echo "TESLATLAS_HUB_TEST_RUNS must be a positive integer" >&2
  exit 2
fi

cleanup() {
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [[ -n "${SIMULATOR_UDID:-}" ]]; then
    for variable in "${PAIR_ENV_VARS[@]}"; do
      DEVELOPER_DIR="$DEVELOPER_DIR" xcrun simctl spawn "$SIMULATOR_UDID" \
        launchctl unsetenv "$variable" >/dev/null 2>&1 || true
    done
  fi
  /usr/bin/trash "$WORKDIR" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

PORT="$(python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)"
export TESLATLAS_LOCAL_HUB_DIR="$WORKDIR/hub"
export TESLATLAS_LOCAL_HUB_PORT="$PORT"
export TESLATLAS_LOCAL_HUB_BIND="127.0.0.1"
export TESLATLAS_LOCAL_HUB_URL="https://127.0.0.1:$PORT"
if [[ -n "${TESLATLAS_LOCAL_TESLAMATE_AUTO_IMPORT:-}" ]]; then
  export TESLATLAS_LOCAL_TESLAMATE_AUTO_IMPORT
fi
if [[ -n "${TESLATLAS_LOCAL_SEED_FIXTURE:-}" ]]; then
  export TESLATLAS_LOCAL_SEED_FIXTURE
fi

wait_for_hub() {
  local retries="$1"
  for _ in $(seq 1 "$retries"); do
    if curl --silent --fail --insecure "https://127.0.0.1:$PORT/readyz" >/dev/null; then
      return 0
    fi
    if [[ -n "$SERVER_PID" ]] && ! kill -0 "$SERVER_PID" 2>/dev/null; then
      return 1
    fi
    sleep 0.1
  done
  return 1
}

start_hub_server() {
  local run_tag="${1:-1}"
  "$ROOT/scripts/mac-local-tls-hub.sh" prepare >"$WORKDIR/prepare-${run_tag}.txt"
  "$ROOT/scripts/mac-local-tls-hub.sh" serve >"$WORKDIR/server-${run_tag}.log" 2>&1 &
  SERVER_PID="$!"
  if ! wait_for_hub 100; then
    echo "Hub tracer server failed readiness for run $run_tag" >&2
    exit 1
  fi
}

restart_hub_server() {
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  SERVER_PID=""
}

start_hub_server 1

SIMULATOR_UDID="$(
  DEVELOPER_DIR="$DEVELOPER_DIR" xcrun simctl list devices available -j |
    python3 -c 'import json,sys; name=sys.argv[1]; devices=json.load(sys.stdin)["devices"]; print(next(d["udid"] for runtime in devices.values() for d in runtime if d["name"] == name))' \
      "$SIMULATOR_NAME"
)"
DEVELOPER_DIR="$DEVELOPER_DIR" xcrun simctl boot "$SIMULATOR_UDID" 2>/dev/null || true
DEVELOPER_DIR="$DEVELOPER_DIR" xcrun simctl bootstatus "$SIMULATOR_UDID" -b

export CREDENTIALS_DIRECTORY="$WORKDIR/hub/creds"

for variable in "${PAIR_ENV_VARS[@]}"; do
  DEVELOPER_DIR="$DEVELOPER_DIR" xcrun simctl spawn "$SIMULATOR_UDID" \
    launchctl setenv "$variable" "${!variable}"
done

pairing_uri() {
  local pair_json
  local pairing_uri
  pair_json="$(
    CREDENTIALS_DIRECTORY="$WORKDIR/hub/creds" \
    "$ROOT/target/release/teslatlas-hub" --config "$WORKDIR/hub/config.toml" \
      pair --label "iOS Simulator tracer" --expires-in-seconds 3600 --json
  )"
  pairing_uri="$(printf '%s\n' "$pair_json" | python3 - <<'PY'
import re
import sys

match = re.search(r'"pairingUri"\s*:\s*"([^"]+)"', sys.stdin.read())
if not match:
  sys.exit(1)
print(match.group(1))
PY
  )" || pairing_uri=""
  if [[ -z "$pairing_uri" ]] && [[ -f "$WORKDIR/hub/last-pairing.json" ]]; then
    pairing_uri="$(python3 - "$WORKDIR/hub/last-pairing.json" <<'PY'
import json
import pathlib
import sys

print(json.loads(pathlib.Path(sys.argv[1]).read_text())["pairingUri"])
PY
    )"
  fi
  if [[ -z "$pairing_uri" ]]; then
    echo "failed to parse pairing URI" >&2
    exit 1
  fi
  printf '%s\n' "$pairing_uri"
}

for run in $(seq 1 "$TRACER_RUNS"); do
  if (( run > 1 )); then
    restart_hub_server
    start_hub_server "$run"
  fi
  PAIRING_URI="$(pairing_uri)"
  DEVELOPER_DIR="$DEVELOPER_DIR" xcrun simctl spawn "$SIMULATOR_UDID" \
    launchctl setenv TESLATLAS_HUB_TEST_PAIRING_URI "$PAIRING_URI"

  xcodebuild test -quiet \
    -project "$TESLATLAS_WORKTREE/Teslatlas.xcodeproj" \
    -scheme "Teslatlas (Dev)" \
    -destination "platform=iOS Simulator,id=$SIMULATOR_UDID" \
    -parallel-testing-enabled NO \
    -test-timeouts-enabled YES \
    -default-test-execution-time-allowance 120 \
    -maximum-test-execution-time-allowance 600 \
    -only-testing:TeslatlasTests/TeslatlasHubLiveTracerTests \
    CODE_SIGNING_ALLOWED="$CODE_SIGNING_ALLOWED"
done
