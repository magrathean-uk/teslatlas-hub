#!/usr/bin/env bash
# Prepare and optionally serve a Mac-local TLS Hub for Simulator / device proof.
# Never touches the production VPS. Secrets stay under a private workdir.
#
# Usage:
#   scripts/mac-local-tls-hub.sh prepare   # certs, seed, print pairing URI
#   scripts/mac-local-tls-hub.sh serve     # run TLS listener in foreground
set -euo pipefail
IFS=$'\n\t'
umask 077

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
WORKDIR="${TESLATLAS_LOCAL_HUB_DIR:-$HOME/.teslatlas-hub-local}"
PORT="${TESLATLAS_LOCAL_HUB_PORT:-8443}"
BIND="${TESLATLAS_LOCAL_HUB_BIND:-127.0.0.1}"
PUBLIC_URL="${TESLATLAS_LOCAL_HUB_URL:-https://127.0.0.1:${PORT}}"
CMD="${1:-prepare}"

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

cat >"$WORKDIR/config.toml" <<EOF
data_dir = "$WORKDIR/data"
bind = "${BIND}:${PORT}"

[tls]
certificate_path = "$CERT"
private_key_path = "$KEY"
public_url = "$PUBLIC_URL"
EOF

export CREDENTIALS_DIRECTORY="$WORKDIR/creds"
cd "$ROOT"
cargo build -q --release
BIN="$ROOT/target/release/teslatlas-hub"

case "$CMD" in
  prepare)
    "$BIN" --config "$WORKDIR/config.toml" init >/dev/null
    cargo run -q --example seed_local_hub -- "$WORKDIR/data" | tee "$WORKDIR/last-seed.json" >/dev/null
    PAIRING_JSON="$("$BIN" --config "$WORKDIR/config.toml" pair --label "Mac local" --expires-in-seconds 3600 --json)"
    printf '%s\n' "$PAIRING_JSON" >"$WORKDIR/last-pairing.json"
    python3 - <<PY
import json, pathlib
work = pathlib.Path("$WORKDIR")
seed = json.loads((work / "last-seed.json").read_text())
pair = json.loads((work / "last-pairing.json").read_text())
print("vehicleId=" + seed["vehicleId"])
print("pairingUri=" + pair["pairingUri"])
print("endpoint=" + pair["endpoint"])
print("tlsPin=" + pair["tlsPin"])
print("workdir=$WORKDIR")
print("To serve: scripts/mac-local-tls-hub.sh serve")
print("For a physical iPhone, re-run prepare with TESLATLAS_LOCAL_HUB_BIND=0.0.0.0 and TESLATLAS_LOCAL_HUB_URL=https://<mac-lan-ip>:$PORT")
PY
    ;;
  serve)
    exec env CREDENTIALS_DIRECTORY="$WORKDIR/creds" \
      "$BIN" --config "$WORKDIR/config.toml" serve
    ;;
  *)
    echo "usage: $0 prepare|serve" >&2
    exit 2
    ;;
esac
