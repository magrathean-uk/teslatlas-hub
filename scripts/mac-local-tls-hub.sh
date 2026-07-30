#!/usr/bin/env bash
# Prepare and optionally serve a Mac-local TLS Hub for Simulator / device proof.
# Never touches the production VPS. Secrets stay under a private workdir.
#
# Usage:
#   scripts/mac-local-tls-hub.sh prepare          # certs, fixture, pairing URI
#   scripts/mac-local-tls-hub.sh bootstrap        # prepare + optional TeslaMate import + pairing
#   scripts/mac-local-tls-hub.sh import           # import configured TeslaMate car
#   scripts/mac-local-tls-hub.sh pair             # one new one-time pairing URI
#   scripts/mac-local-tls-hub.sh verify-unchanged # two imports retain one snapshot
#   scripts/mac-local-tls-hub.sh release-candidate # import->snapshot ready->pairing->serve
#   scripts/mac-local-tls-hub.sh serve            # run TLS listener in foreground
set -euo pipefail
IFS=$'\n\t'
umask 077

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
WORKDIR="${TESLATLAS_LOCAL_HUB_DIR:-$HOME/.teslatlas-hub-local}"
PORT="${TESLATLAS_LOCAL_HUB_PORT:-8443}"
AUTO_DETECT_LAN="${TESLATLAS_LOCAL_HUB_AUTO_DETECT_LAN:-1}"
BIND="${TESLATLAS_LOCAL_HUB_BIND:-}"
CMD="${1:-prepare}"
TESLAMATE_URL="${TESLATLAS_LOCAL_TESLAMATE_URL:-}"
TESLAMATE_SOURCE_KEY="${TESLATLAS_LOCAL_TESLAMATE_SOURCE_KEY:-mac-teslamate}"
TESLAMATE_CAR_ID="${TESLATLAS_LOCAL_TESLAMATE_CAR_ID:-1}"
TESLAMATE_POSTGRES_PASSWORD="${TESLATLAS_LOCAL_TESLAMATE_POSTGRES_PASSWORD:-}"
TESLAMATE_POSTGRES_PASSWORD_FILE="${TESLATLAS_LOCAL_TESLAMATE_POSTGRES_PASSWORD_FILE:-}"
SEED_FIXTURE="${TESLATLAS_LOCAL_SEED_FIXTURE:-1}"
AUTO_IMPORT="${TESLATLAS_LOCAL_TESLAMATE_AUTO_IMPORT:-0}"
BOOTSTRAP_IMPORT="${TESLATLAS_LOCAL_TESLAMATE_BOOTSTRAP_IMPORT:-1}"

bool_true() {
  local lower
  lower="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')"
  case "$lower" in
    1|true|yes|on) return 0 ;;
    *) return 1 ;;
  esac
}

detect_lan_address() {
  local interface=""
  local address=""

  interface="$(route -n get default 2>/dev/null | awk '/interface:/{print $2; exit}')"
  if [[ -n "$interface" ]]; then
    address="$(ipconfig getifaddr "$interface" 2>/dev/null || true)"
  fi

  if [[ -z "$address" ]]; then
    for interface in en0 en1 en2 en3 en4; do
      address="$(ipconfig getifaddr "$interface" 2>/dev/null || true)"
      if [[ -n "$address" ]]; then
        break
      fi
    done
  fi

  if [[ -z "$address" ]]; then
    address="127.0.0.1"
  fi
  printf '%s\n' "$address"
}

if [[ -z "$BIND" ]]; then
  if bool_true "$AUTO_DETECT_LAN"; then
    BIND="$(detect_lan_address)"
  else
    BIND="127.0.0.1"
  fi
fi
PUBLIC_URL="${TESLATLAS_LOCAL_HUB_URL:-https://$BIND:$PORT}"

mkdir -p "$WORKDIR/tls" "$WORKDIR/creds" "$WORKDIR/data"
chmod 700 "$WORKDIR" "$WORKDIR/creds" "$WORKDIR/data"

CERT="$WORKDIR/tls/fullchain.pem"
KEY="$WORKDIR/tls/privkey.pem"
CERT_HOST="$(python3 - "$PUBLIC_URL" <<'PY'
import sys
import urllib.parse

host = urllib.parse.urlsplit(sys.argv[1]).hostname
if not host:
    raise SystemExit("public Hub URL has no host")
print(host)
PY
)"
if [[ "$CERT_HOST" =~ ^[0-9a-fA-F:.]+$ ]]; then
  CERT_SAN="IP:$CERT_HOST"
else
  CERT_SAN="DNS:$CERT_HOST"
fi
if [[ ! -f "$CERT" || ! -f "$KEY" ]]; then
  openssl req -x509 -newkey rsa:2048 -sha256 -days 30 -nodes \
    -keyout "$KEY" -out "$CERT" \
    -subj "/CN=$CERT_HOST" \
    -addext "subjectAltName=$CERT_SAN" >/dev/null 2>&1
  chmod 600 "$KEY"
fi

CURSOR="$WORKDIR/creds/cursor-key"
if [[ ! -f "$CURSOR" ]]; then
  dd if=/dev/urandom of="$CURSOR" bs=32 count=1 status=none
  chmod 600 "$CURSOR"
fi

POSTGRES_PASSWORD="$WORKDIR/creds/teslamate-postgres-password"
if [[ -n "$TESLAMATE_URL" ]]; then
  if [[ -n "$TESLAMATE_POSTGRES_PASSWORD_FILE" ]]; then
    if [[ ! -r "$TESLAMATE_POSTGRES_PASSWORD_FILE" ]]; then
      echo "TESLATLAS_LOCAL_TESLAMATE_POSTGRES_PASSWORD_FILE is not readable" >&2
      exit 2
    fi
    TESLAMATE_POSTGRES_PASSWORD="$(tr -d '\r\n' < "$TESLAMATE_POSTGRES_PASSWORD_FILE")"
  fi

  if [[ -z "$TESLAMATE_POSTGRES_PASSWORD" ]]; then
    if [[ -f "$POSTGRES_PASSWORD" ]]; then
      TESLAMATE_POSTGRES_PASSWORD="$(tr -d '\r\n' < "$POSTGRES_PASSWORD")"
    fi
  fi
  if [[ -z "$TESLAMATE_POSTGRES_PASSWORD" ]]; then
    echo "missing TESLATLAS_LOCAL_TESLAMATE_POSTGRES_PASSWORD for import flow" >&2
    exit 2
  fi
  printf '%s' "$TESLAMATE_POSTGRES_PASSWORD" >"$POSTGRES_PASSWORD"
  chmod 600 "$POSTGRES_PASSWORD"
fi

cat >"$WORKDIR/config.toml" <<CFG
data_dir = "$WORKDIR/data"
bind = "${BIND}:${PORT}"

[tls]
certificate_path = "$CERT"
private_key_path = "$KEY"
public_url = "$PUBLIC_URL"
CFG

if [[ -n "$TESLAMATE_URL" ]]; then
  {
    printf '\n[teslamate]\n'
    printf 'source_url = "%s"\n' "$TESLAMATE_URL"
    printf 'source_key = "%s"\n' "$TESLAMATE_SOURCE_KEY"
    printf 'page_size = 10000\n'
    printf 'maximum_rows = 20000000\n'
    printf 'minimum_free_bytes = 0\n'
  } >>"$WORKDIR/config.toml"
fi

export CREDENTIALS_DIRECTORY="$WORKDIR/creds"
cd "$ROOT"
cargo build -q --release
BIN="$ROOT/target/release/teslatlas-hub"

save_pairing() {
  printf '%s\n' "$1" >"$WORKDIR/last-pairing.json"
  python3 - "$WORKDIR/last-pairing.json" <<'PY'
import json
import pathlib
import sys

payload = json.loads(pathlib.Path(sys.argv[1]).read_text())
print("pairingUri=" + payload["pairingUri"])
print("endpoint=" + payload["endpoint"])
print("tlsPin=" + payload["tlsPin"])
print("workdir=" + pathlib.Path(sys.argv[1]).parent.as_posix())
print("To serve: scripts/mac-local-tls-hub.sh serve")
PY
}

pair_local() {
  local pairing_json
  pairing_json="$($BIN --config "$WORKDIR/config.toml" pair --label "Mac local" --expires-in-seconds 3600 --json)"
  save_pairing "$pairing_json"
}

seed_local() {
  if [[ "$SEED_FIXTURE" == "1" ]]; then
    cargo run -q --example seed_local_hub -- "$WORKDIR/data" |
      tee "$WORKDIR/last-seed.json" >/dev/null
  fi
}

run_import() {
  [[ -n "$TESLAMATE_URL" ]] || {
    echo "TESLATLAS_LOCAL_TESLAMATE_URL is required" >&2
    exit 2
  }
  "$BIN" --config "$WORKDIR/config.toml" import-tesla-mate \
    --car-id "$TESLAMATE_CAR_ID"
}

wait_for_snapshot_ready() {
  local max_attempts=120
  local attempt=0
  local database_path="$WORKDIR/data/hub.sqlite"

  while (( attempt < max_attempts )); do
    if sqlite3 "$database_path" "SELECT COUNT(*) FROM sync_manifests;" 2>/dev/null \
      | awk 'NR==1 && $1 ~ /^[0-9]+$/ && $1 > 0 { exit 0 } { exit 1 }'; then
      return 0
    fi
    attempt=$((attempt + 1))
    sleep 0.25
  done
  echo "timed out waiting for sync manifest rows" >&2
  exit 2
}

run_release_candidate() {
  "$BIN" --config "$WORKDIR/config.toml" init >/dev/null
  seed_local
  if [[ -n "$TESLAMATE_URL" ]]; then
    run_import | tee "$WORKDIR/last-import.json"
    wait_for_snapshot_ready
  fi
  pair_local
  exec env CREDENTIALS_DIRECTORY="$WORKDIR/creds" \
    "$BIN" --config "$WORKDIR/config.toml" serve
}

prepare_and_pair() {
  local import_enabled="$1"

  seed_local
  if bool_true "$import_enabled"; then
    run_import | tee "$WORKDIR/last-import.json"
  fi
  pair_local
}

case "$CMD" in
  prepare)
    "$BIN" --config "$WORKDIR/config.toml" init >/dev/null
    prepare_and_pair "$AUTO_IMPORT"
    if [[ -f "$WORKDIR/last-seed.json" ]]; then
      python3 - "$WORKDIR/last-seed.json" <<'PY'
import json
import pathlib
import sys
seed = json.loads(pathlib.Path(sys.argv[1]).read_text())
print("seedVehicleId=" + seed["vehicleId"])
PY
    fi
    ;;
  bootstrap)
    "$BIN" --config "$WORKDIR/config.toml" init >/dev/null
    prepare_and_pair "$BOOTSTRAP_IMPORT"
    ;;
  release-candidate)
    run_release_candidate
    ;;
  pair)
    "$BIN" --config "$WORKDIR/config.toml" init >/dev/null
    pair_local
    ;;
  import)
    run_import
    ;;
  verify-unchanged)
    [[ -n "$TESLAMATE_URL" ]] || {
      echo "TESLATLAS_LOCAL_TESLAMATE_URL is required" >&2
      exit 2
    }
    "$BIN" --config "$WORKDIR/config.toml" init >/dev/null
    "$BIN" --config "$WORKDIR/config.toml" import-tesla-mate \
      --car-id "$TESLAMATE_CAR_ID" >"$WORKDIR/first-import.json"
    "$BIN" --config "$WORKDIR/config.toml" import-tesla-mate \
      --car-id "$TESLAMATE_CAR_ID" >"$WORKDIR/second-import.json"
    python3 - "$WORKDIR/first-import.json" "$WORKDIR/second-import.json" <<'PY'
import json
import pathlib
import sys

first = json.loads(pathlib.Path(sys.argv[1]).read_text())
second = json.loads(pathlib.Path(sys.argv[2]).read_text())
for field in ("snapshotId", "sequence", "projectedRows", "vehicleId"):
    if first[field] != second[field]:
        raise SystemExit(f"unchanged import changed {field}")
print(json.dumps(second, separators=(",", ":"), sort_keys=True))
PY
    ;;
  serve)
    exec env CREDENTIALS_DIRECTORY="$WORKDIR/creds" \
      "$BIN" --config "$WORKDIR/config.toml" serve
    ;;
  *)
    echo "usage: $0 prepare|bootstrap|import|pair|verify-unchanged|release-candidate|serve" >&2
    exit 2
    ;;
esac
