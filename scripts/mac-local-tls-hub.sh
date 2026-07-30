#!/usr/bin/env bash
# Prepare and optionally serve a Mac-local TLS Hub for Simulator / device proof.
# Never touches the production VPS. Secrets stay under a private workdir.
#
# Usage:
#   scripts/mac-local-tls-hub.sh prepare          # certs, fixture, pairing URI
#   scripts/mac-local-tls-hub.sh import           # import configured TeslaMate car
#   scripts/mac-local-tls-hub.sh verify-unchanged # two imports retain one snapshot
#   scripts/mac-local-tls-hub.sh serve            # run TLS listener in foreground
set -euo pipefail
IFS=$'\n\t'
umask 077

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
WORKDIR="${TESLATLAS_LOCAL_HUB_DIR:-$HOME/.teslatlas-hub-local}"
PORT="${TESLATLAS_LOCAL_HUB_PORT:-8443}"
BIND="${TESLATLAS_LOCAL_HUB_BIND:-127.0.0.1}"
PUBLIC_URL="${TESLATLAS_LOCAL_HUB_URL:-https://127.0.0.1:${PORT}}"
CMD="${1:-prepare}"
TESLAMATE_URL="${TESLATLAS_LOCAL_TESLAMATE_URL:-}"
TESLAMATE_SOURCE_KEY="${TESLATLAS_LOCAL_TESLAMATE_SOURCE_KEY:-mac-teslamate}"
TESLAMATE_CAR_ID="${TESLATLAS_LOCAL_TESLAMATE_CAR_ID:-1}"
SEED_FIXTURE="${TESLATLAS_LOCAL_SEED_FIXTURE:-1}"

mkdir -p "$WORKDIR/tls" "$WORKDIR/creds" "$WORKDIR/data"
chmod 700 "$WORKDIR" "$WORKDIR/creds" "$WORKDIR/data"

CERT="$WORKDIR/tls/fullchain.pem"
KEY="$WORKDIR/tls/privkey.pem"
if [[ ! -f "$CERT" || ! -f "$KEY" ]]; then
  openssl req -x509 -newkey rsa:2048 -sha256 -days 30 -nodes \
    -keyout "$KEY" -out "$CERT" \
    -subj "/CN=localhost" \
    -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" >/dev/null 2>&1
  chmod 600 "$KEY"
fi

CURSOR="$WORKDIR/creds/cursor-key"
if [[ ! -f "$CURSOR" ]]; then
  dd if=/dev/urandom of="$CURSOR" bs=32 count=1 status=none
  chmod 600 "$CURSOR"
fi

POSTGRES_PASSWORD="$WORKDIR/creds/teslamate-postgres-password"
if [[ -n "$TESLAMATE_URL" && ! -f "$POSTGRES_PASSWORD" ]]; then
  # Local PostgreSQL normally authenticates this Mac user through its local
  # trust policy. The importer still requires a non-empty credential file so
  # production cannot silently weaken its password boundary.
  printf '%s' "mac-local-trust" >"$POSTGRES_PASSWORD"
  chmod 600 "$POSTGRES_PASSWORD"
fi

cat >"$WORKDIR/config.toml" <<EOF
data_dir = "$WORKDIR/data"
bind = "${BIND}:${PORT}"

[tls]
certificate_path = "$CERT"
private_key_path = "$KEY"
public_url = "$PUBLIC_URL"
EOF

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

case "$CMD" in
  prepare)
    "$BIN" --config "$WORKDIR/config.toml" init >/dev/null
    if [[ "$SEED_FIXTURE" == "1" ]]; then
      cargo run -q --example seed_local_hub -- "$WORKDIR/data" |
        tee "$WORKDIR/last-seed.json" >/dev/null
    fi
    PAIRING_JSON="$("$BIN" --config "$WORKDIR/config.toml" pair --label "Mac local" --expires-in-seconds 3600 --json)"
    printf '%s\n' "$PAIRING_JSON" >"$WORKDIR/last-pairing.json"
    python3 - <<PY
import json, pathlib
work = pathlib.Path("$WORKDIR")
pair = json.loads((work / "last-pairing.json").read_text())
seed_path = work / "last-seed.json"
if seed_path.exists():
    seed = json.loads(seed_path.read_text())
    print("vehicleId=" + seed["vehicleId"])
print("pairingUri=" + pair["pairingUri"])
print("endpoint=" + pair["endpoint"])
print("tlsPin=" + pair["tlsPin"])
print("workdir=$WORKDIR")
print("To serve: scripts/mac-local-tls-hub.sh serve")
print("For a physical iPhone, re-run prepare with TESLATLAS_LOCAL_HUB_BIND=0.0.0.0 and TESLATLAS_LOCAL_HUB_URL=https://<mac-lan-ip>:$PORT")
PY
    ;;
  import)
    [[ -n "$TESLAMATE_URL" ]] || {
      echo "TESLATLAS_LOCAL_TESLAMATE_URL is required" >&2
      exit 2
    }
    "$BIN" --config "$WORKDIR/config.toml" init >/dev/null
    "$BIN" --config "$WORKDIR/config.toml" import-tesla-mate \
      --car-id "$TESLAMATE_CAR_ID"
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
    echo "usage: $0 prepare|import|verify-unchanged|serve" >&2
    exit 2
    ;;
esac
