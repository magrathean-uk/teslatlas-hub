#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
set +x
umask 077

readonly ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
readonly WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-rpc-test.XXXXXX")"
readonly BIN="$WORKDIR/bin"
readonly LOG="$WORKDIR/commands.log"
readonly CREDENTIAL="$WORKDIR/encrypted-credential"
readonly OLD_CREDENTIAL="$WORKDIR/old-encrypted-credential"
readonly SECRET_ACCESS='access-secret-test-value'
readonly SECRET_REFRESH='refresh-secret-test-value'
trap 'rm -rf -- "$WORKDIR"' EXIT HUP INT TERM
mkdir -p "$BIN"

cat >"$BIN/ssh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
host=''
port=''
command=''
while (($#)); do
  if [[ "$1" == '-p' ]]; then
    port="$2"
    shift 2
    continue
  fi
  if [[ "$1" == '--' ]]; then
    shift
    host="$1"
    shift
    command="${1:-}"
    break
  fi
  shift
done
printf 'ssh port=%s host=%s command=%s\n' "$port" "$host" "$command" >>"$FAKE_LOG"
if [[ "$host" == source.example ]]; then
  "$FAKE_DOCKER" "$command"
else
  if [[ "${FAKE_RECEIVER_FAIL:-0}" == 1 ]]; then
    exit 1
  fi
  python3 -c '
import base64
import os
import sys
data = sys.stdin.buffer.read()
if not data:
    raise SystemExit(1)
with open(os.environ["FAKE_CREDENTIAL"], "wb") as output:
    output.write(b"encrypted:")
    output.write(base64.b64encode(data))
'
fi
EOF
cat >"$BIN/docker" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'docker command=%s\n' "$*" >>"$FAKE_LOG"
case "${FAKE_RPC_MODE:-valid}" in
  valid)
    printf '{"access_token":"%s","refresh_token":"%s"}' "$FAKE_ACCESS" "$FAKE_REFRESH"
    ;;
  extra)
    printf '{"access_token":"%s","refresh_token":"%s","token":"fleet-single-token"}' "$FAKE_ACCESS" "$FAKE_REFRESH"
    ;;
  duplicate)
    printf '{"access_token":"first","access_token":"second","refresh_token":"%s"}' "$FAKE_REFRESH"
    ;;
  truncated)
    printf '{"access_token":"%s","refresh_token":"' "$FAKE_ACCESS"
    ;;
  oversized)
    python3 -c 'print("{" + "\"access_token\":\"" + ("a" * 33000) + "\",\"refresh_token\":\"r\"}", end="")'
    ;;
  fleet)
    printf '{"token":"fleet-single-token"}'
    ;;
  missing)
    printf 'nil'
    ;;
  *)
    exit 1
    ;;
esac
EOF
cat >"$BIN/fake-hub" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
python3 -c '
import base64
import os
import sys
data = sys.stdin.buffer.read()
if not data:
    raise SystemExit(1)
with open(os.environ["FAKE_CREDENTIAL"], "wb") as output:
    output.write(b"encrypted:")
    output.write(base64.b64encode(data))
'
EOF
chmod 0755 "$BIN/ssh" "$BIN/docker" "$BIN/fake-hub"

export PATH="$BIN:$PATH"
export FAKE_LOG="$LOG"
export FAKE_DOCKER="$BIN/docker"
export FAKE_CREDENTIAL="$CREDENTIAL"
export FAKE_ACCESS="$SECRET_ACCESS"
export FAKE_REFRESH="$SECRET_REFRESH"

assert_absent() {
  local needle="$1"
  if grep -R -F -q \
    --exclude="$(basename -- "$CREDENTIAL")" \
    --exclude="$(basename -- "$OLD_CREDENTIAL")" \
    -- "$needle" "$WORKDIR"; then
    printf '%s\n' 'secret appeared outside encrypted credential' >&2
    exit 1
  fi
}

run_import() {
  "$ROOT/scripts/import-teslamate-rpc.sh" \
    --source-host source.example \
    --teslamate-container teslamate \
    "$@"
}

run_import --hub-host hub.example
[[ -s "$CREDENTIAL" ]] || { printf '%s\n' 'encrypted credential missing' >&2; exit 1; }
grep -F -q 'store-tesla-mate-owner-tokens' "$LOG"
grep -F -q 'TeslaMate.Auth.get_tokens() do %TeslaMate.Auth.Tokens{access: access_token, refresh: refresh_token} -> IO.write(Jason.encode!(%{access_token: access_token, refresh_token: refresh_token})); _ -> raise "TeslaMate owner tokens unavailable" end' "$LOG"
grep -F -q 'docker command=' "$LOG"
grep -F -q 'ssh port=22 host=source.example' "$LOG"
grep -F -q 'ssh port=22 host=hub.example' "$LOG"
assert_absent "$SECRET_ACCESS"
assert_absent "$SECRET_REFRESH"

run_import --source-port 2201 --hub-port 2202 --source-sudo --hub-host hub.example
grep -F -q 'ssh port=2201 host=source.example command='"'"'sudo'"'"' '"'"'-n'"'"' '"'"'docker'"'"' '"'"'exec'"'"'' "$LOG"
grep -F -q 'ssh port=2202 host=hub.example' "$LOG"
assert_absent "$SECRET_ACCESS"
assert_absent "$SECRET_REFRESH"

run_import --hub-port 2203 --hub-sudo --hub-host hub.example
grep -F -q 'ssh port=2203 host=hub.example command='"'"'sudo'"'"' '"'"'-n'"'"' '"'"'/usr/bin/teslatlas-hub'"'"' '"'"'store-tesla-mate-owner-tokens'"'"'' "$LOG"
assert_absent "$SECRET_ACCESS"
assert_absent "$SECRET_REFRESH"

if run_import --hub-sudo --local --hub-command "$BIN/fake-hub" >/dev/null 2>&1; then
  printf '%s\n' '--hub-sudo was accepted with --local' >&2
  exit 1
fi

for invalid_port_args in \
  '--source-port abc' \
  '--source-port 0' \
  '--hub-port 65536'; do
  if run_import --local --hub-command "$BIN/fake-hub" $invalid_port_args >/dev/null 2>&1; then
    printf 'invalid port accepted: %s\n' "$invalid_port_args" >&2
    exit 1
  fi
done

cp -- "$CREDENTIAL" "$OLD_CREDENTIAL"
for mode in extra duplicate truncated oversized fleet missing; do
  export FAKE_RPC_MODE="$mode"
  if run_import --local --hub-command "$BIN/fake-hub" >/dev/null 2>&1; then
    printf 'malformed mode accepted: %s\n' "$mode" >&2
    exit 1
  fi
  cmp -s "$CREDENTIAL" "$OLD_CREDENTIAL"
done

export FAKE_RPC_MODE=valid
export FAKE_RECEIVER_FAIL=1
if run_import --hub-port 2203 --hub-sudo --hub-host hub.example >/dev/null 2>&1; then
  printf '%s\n' 'receiver failure was accepted' >&2
  exit 1
fi
cmp -s "$CREDENTIAL" "$OLD_CREDENTIAL"

unset FAKE_RECEIVER_FAIL
export FAKE_RPC_MODE=source-failure
if run_import --local --hub-command "$BIN/fake-hub" >/dev/null 2>&1; then
  printf '%s\n' 'source failure was accepted' >&2
  exit 1
fi
cmp -s "$CREDENTIAL" "$OLD_CREDENTIAL"
assert_absent "$SECRET_ACCESS"
assert_absent "$SECRET_REFRESH"

printf '%s\n' 'rpc import tests passed'
