#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
umask 077

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
TESLATLAS_WORKTREE="${TESLATLAS_WORKTREE:-$ROOT/../teslatlas-hub-test}"
DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode-beta.app/Contents/Developer}"
SIMULATOR_NAME="${TESLATLAS_SIMULATOR_NAME:-iPhone 17 Pro}"
CODE_SIGNING_ALLOWED="${TESLATLAS_CODE_SIGNING_ALLOWED:-YES}"
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-hub-tracer.XXXXXX")"
SERVER_PID=""

cleanup() {
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [[ -n "${SIMULATOR_UDID:-}" ]]; then
    for variable in \
      TESLATLAS_HUB_TEST_PAIRING_URI \
      TESLATLAS_HUB_TEST_EXPECTED_MANIFEST_ROWS
    do
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

"$ROOT/scripts/mac-local-tls-hub.sh" prepare >"$WORKDIR/prepare.txt"
"$ROOT/scripts/mac-local-tls-hub.sh" serve >"$WORKDIR/server.log" 2>&1 &
SERVER_PID="$!"

for _ in {1..100}; do
  if curl --silent --fail --insecure "https://127.0.0.1:$PORT/readyz" >/dev/null; then
    break
  fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "Hub tracer server stopped before readiness" >&2
    exit 1
  fi
  sleep 0.1
done
curl --silent --fail --insecure "https://127.0.0.1:$PORT/readyz" >/dev/null

PAIRING_URI="$(python3 - "$WORKDIR/hub/last-pairing.json" <<'PY'
import json
import pathlib
import sys
print(json.loads(pathlib.Path(sys.argv[1]).read_text())["pairingUri"])
PY
)"
SIMULATOR_UDID="$(
  DEVELOPER_DIR="$DEVELOPER_DIR" xcrun simctl list devices available -j |
    python3 -c 'import json,sys; name=sys.argv[1]; devices=json.load(sys.stdin)["devices"]; print(next(d["udid"] for runtime in devices.values() for d in runtime if d["name"] == name))' \
      "$SIMULATOR_NAME"
)"
DEVELOPER_DIR="$DEVELOPER_DIR" xcrun simctl boot "$SIMULATOR_UDID" 2>/dev/null || true
DEVELOPER_DIR="$DEVELOPER_DIR" xcrun simctl bootstatus "$SIMULATOR_UDID" -b
DEVELOPER_DIR="$DEVELOPER_DIR" xcrun simctl spawn "$SIMULATOR_UDID" \
  launchctl setenv TESLATLAS_HUB_TEST_PAIRING_URI "$PAIRING_URI"
DEVELOPER_DIR="$DEVELOPER_DIR" xcrun simctl spawn "$SIMULATOR_UDID" \
  launchctl setenv TESLATLAS_HUB_TEST_EXPECTED_MANIFEST_ROWS 4

DEVELOPER_DIR="$DEVELOPER_DIR" xcodebuild test -quiet \
  -project "$TESLATLAS_WORKTREE/Teslatlas.xcodeproj" \
  -scheme "Teslatlas (Dev)" \
  -destination "platform=iOS Simulator,id=$SIMULATOR_UDID" \
  -parallel-testing-enabled NO \
  -only-testing:TeslatlasTests/TeslatlasHubLiveTracerTests \
  CODE_SIGNING_ALLOWED="$CODE_SIGNING_ALLOWED"
